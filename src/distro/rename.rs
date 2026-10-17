/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `rename`, for distros and machines.

use super::adopt::ensure_journal;
use super::converge::converge;
use super::default::{DefaultType, recorded_default, write_default};
use super::images::{
    imported_ref_sha, machine_import_ref, release_machine_images, snapshot_repo, tag_image,
    untag_image,
};
use super::journal::{
    CONF_FILES, DistroLock, distro_exists, journal_conf_path, journal_fs_path, preserved_rootfs,
    refresh_conf_snapshots, with_journal_dir_unlocked, write_journal,
};
use super::lifecycle::{remove_journal_dir, start};
use super::lock::{set_locked, unlock};
use super::machine::{boot_machine, machine_rootfs};
use super::names::{bail_distro_shadow, bail_if_staged, bail_machine_shadow};
use super::{find, run_container, run_container_quiet};
use crate::container::{self, ensure_started, validate_name};
use crate::distro::spec::Arch;
use crate::distro::terminal;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

/// Rename a distro (`container distro rename`): the journal — spec,
/// disk, and init assets — moves to `new`, and a distro that was
/// running is rematerialized under the new name once the move lands.
/// The ephemeral container is never renamed (the logical name lives in
/// its label, not the runtime id); it's converged away before the move
/// and recreated fresh after, so nothing container-side ever carries
/// `old` past the journal swap.
///
/// A same-named *machine* is refused unless `force` — `cm` would
/// resolve `new` to the distro, silently shadowing it.
pub fn rename(old: &str, new: &str, force: bool) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    validate_name(new)?;
    if old == new {
        bail!("`{old}` is already the distro's name");
    }
    ensure_journal(old)?;
    if distro_exists(new)? {
        bail!("distro `{new}` already exists");
    }
    bail_if_staged(new)?;
    bail_machine_shadow(new, force)?;
    // Staged state under `old`'s name would be orphaned by the move —
    // a rare interrupted-`set` leftover for the user to resolve first.
    let staged = preserved_rootfs(old)?;
    if staged.exists() {
        bail!(
            "`{old}` has a staged filesystem from an interrupted `set` — \
             inspect or remove {} before renaming",
            staged.display()
        );
    }
    let lock = DistroLock::acquire(old)?;
    let mut spec = lock.spec()?;
    // Converge to the resting state first: the journal move then
    // carries the disk, and no container is left naming `old`.
    let was_running = if let Some(info) = find(old).ok().filter(|i| i.is_running()) {
        refresh_conf_snapshots(old, info.id(), &spec);
        run_container_quiet(&["stop", info.id()])?;
        true
    } else {
        false
    };
    converge(old, &lock)?;
    spec.name = new.to_string();
    write_journal(new, &spec)?;
    // Move the disk into the new journal: a rename, so an interruption
    // leaves it whole under one name or the other, never a partial
    // copy — the same swap `migrate` and `set` use.
    if let Err(e) = with_journal_dir_unlocked(new, || {
        with_journal_dir_unlocked(old, || {
            let src = journal_fs_path(old)?;
            if src.exists() {
                unlock(&src);
                fs::rename(&src, journal_fs_path(new)?)
                    .with_context(|| format!("failed to move {}", src.display()))?;
            }
            // The guest-config snapshots move with the journal too.
            for (_, which) in CONF_FILES {
                let src = journal_conf_path(old, which)?;
                if src.exists() {
                    unlock(&src);
                    fs::rename(&src, journal_conf_path(new, which)?)
                        .with_context(|| format!("failed to move {}", src.display()))?;
                    set_locked(&journal_conf_path(new, which)?, true)?;
                }
            }
            Ok(())
        })?;
        let dst = journal_fs_path(new)?;
        if dst.exists() {
            set_locked(&dst, true)?;
        }
        Ok(())
    }) {
        // The disk never left `old`; drop the just-written journal so
        // the rename can be retried.
        let _ = remove_journal_dir(new);
        return Err(e);
    }
    remove_journal_dir(old)?;
    if recorded_default() == Some(DefaultType::Distro(old.to_string())) {
        write_default(&DefaultType::Distro(new.to_string()))?;
    }
    terminal::rename(old, new, spec.resolve().0.terminal_profile);
    drop(lock);
    // An imported distro's snapshot image keeps `old` in its reference
    // and labels — cleanup keys off the new name, so `rm` won't find
    // it later. Say so rather than leaking it silently then.
    if spec.image.starts_with(&snapshot_repo(old)) {
        eprintln!(
            "container-distro: image `{}` is still labeled for `{old}` — \
             `rm` won't remove it automatically",
            spec.image
        );
    }
    if was_running {
        start(new)?;
    }
    Ok(())
}

/// Rename a `container machine` (`cm --rename --machine`). Upstream has
/// no rename — the machine's id is its bundle path — so the machine is
/// recreated: `machine create` under `new` with the same image and
/// resources, then the old `rootfs.ext4` is cloned over the new one's
/// (the same disk swap [`migrate_to_machine`] uses). The old machine
/// is removed once the new one exists and its carried disk boots; a
/// default marker is re-pointed at `new`.
///
/// A same-named *distro* is refused unless `force` — `cm` would
/// resolve `new` to the distro, hiding the machine entirely.
pub fn rename_machine(old: &str, new: &str, force: bool) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    validate_name(new)?;
    if old == new {
        bail!("`{old}` is already the machine's name");
    }
    let machines = container::list_machines()?;
    if machines.iter().any(|m| m.id == new) {
        bail!("a machine named `{new}` already exists");
    }
    bail_distro_shadow(new, force)?;
    let detail = container::inspect_machine(old)?;
    let src = machine_rootfs(&detail)?
        .with_context(|| format!("machine `{old}` has no root filesystem to carry"))?;
    let was_default = machines.iter().any(|m| m.id == old && m.default);
    let was_running = detail.is_running();
    if was_running {
        run_container_quiet(&["machine", "stop", old])?;
    }
    let image = detail
        .image
        .as_ref()
        .map(|i| i.reference.clone())
        .context("machine has no image reference")?;
    // An imported machine's image ref is a `local/machine-<old>` tag —
    // retag it under `new` so the recorded reference stays valid once
    // `old`'s tags are released. A failed retag just keeps `old`'s ref.
    let (image, retagged) = match imported_ref_sha(&image) {
        Some(sha) => {
            let t = machine_import_ref(new, sha);
            if tag_image(&image, &t).is_ok() {
                (t, true)
            } else {
                (image, false)
            }
        }
        None => (image, false),
    };
    // From here a failure tries to leave `old` as it was found.
    let renamed = (|| -> Result<()> {
        // `machine create` boots the machine; its internal boot
        // materializes the plugin-state disk the swap replaces below.
        let mut a = vec![
            "machine".to_string(),
            "create".to_string(),
            "--name".to_string(),
            new.to_string(),
        ];
        if let Some(c) = detail.cpus {
            a.extend(["--cpus".into(), c.to_string()]);
        }
        if let Some(m) = detail.memory {
            a.extend(["--memory".into(), format!("{}M", m / (1024 * 1024))]);
        }
        if let Some(h) = &detail.home_mount {
            a.extend(["--home-mount".into(), h.clone()]);
        }
        // Keep the machine's platform — an amd64 machine renamed to an
        // arm64 create would boot its x86-64 rootfs without Rosetta.
        if let Some(arch) = detail
            .platform
            .as_ref()
            .and_then(|p| p.architecture.parse::<Arch>().ok())
        {
            a.extend(["--arch".into(), arch.as_str().into()]);
        }
        a.push(image.clone());
        run_container(&a.iter().map(String::as_str).collect::<Vec<_>>())?;
        // Replace the machine's filesystem while it is stopped: clone
        // to a sibling then rename over its disk, so an interruption
        // leaves the old disk or the new one, never a partial file.
        let swapped = (|| -> Result<()> {
            run_container_quiet(&["machine", "stop", new])?;
            let dst = machine_rootfs(&container::inspect_machine(new)?)?
                .context("the new machine has no rootfs to replace")?;
            let mut tmp = dst.clone().into_os_string();
            tmp.push(".new");
            let tmp = PathBuf::from(tmp);
            let _ = fs::remove_file(&tmp);
            // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
            fs::copy(&src, &tmp).with_context(|| format!("failed to clone {}", src.display()))?;
            fs::rename(&tmp, &dst)
                .with_context(|| format!("failed to install {}", dst.display()))?;
            Ok(())
        })();
        if let Err(e) = swapped {
            // `new` still holds only its image filesystem — remove it
            // so a retry isn't blocked, along with the tag we made.
            let _ = run_container_quiet(&["machine", "rm", new]);
            if retagged {
                untag_image(&image);
            }
            return Err(e.context(format!(
                "failed to move `{old}`'s filesystem into machine `{new}`"
            )));
        }
        // `machine create`'s boot ran the image filesystem; the carried
        // disk hasn't run yet. Verify it like `migrate --out` does —
        // the first-boot flake is why it retries.
        let booted = (0..3).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            boot_machine(new).is_ok()
        });
        if !booted {
            bail!(
                "machine `{new}` holds `{old}`'s filesystem but failed to boot; \
                 `{old}` was left in place (see `container machine logs {new}`)"
            );
        }
        if !was_running {
            let _ = run_container_quiet(&["machine", "stop", new]);
        }
        Ok(())
    })();
    if let Err(e) = renamed {
        if was_running {
            let _ = boot_machine(old);
        }
        return Err(e);
    }
    if let Err(e) = run_container_quiet(&["machine", "rm", old]) {
        eprintln!("container-distro: renamed, but removing machine `{old}` failed: {e:#}");
    } else if retagged {
        // The retagged image is recorded under `new`; `old`'s
        // `local/machine-<old>` tag is bookkeeping to release.
        release_machine_images(old);
    }
    if was_default {
        let _ = run_container_quiet(&["machine", "set-default", new]);
        // A `machine:<old>` pick is stale now — repoint it at `new`.
        // (`machine:` follows the moved default on its own.)
        if recorded_default() == Some(DefaultType::Machine(old.to_string())) {
            let _ = write_default(&DefaultType::Machine(new.to_string()));
        }
    }
    Ok(())
}
