/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `migrate`: between machines and distros.

use super::adopt::{adopt, ensure_journal};
use super::converge::{converge, dismantle, materialize};
use super::create::{CreateOptions, boot, check_sources, ensure_running_locked, stamped_origin};
use super::default::{DefaultType, no_effective_default, write_default};
use super::images::{
    imported_ref_sha, machine_import_ref, remove_snapshot_images, tag_image, untag_image,
};
use super::journal::{
    DistroLock, container_dir, distro_exists, journal_fs_path, journal_present,
    refresh_conf_snapshots, with_journal_dir_unlocked, write_journal,
};
use super::lifecycle::{cleanup_preserved, remove_journal_dir};
use super::lock::set_locked;
use super::machine::{
    boot_machine, create_machine, machine_create_image, machine_rootfs, reject_machine_opts,
    rootfs_override_source, spec_from_machine,
};
use super::names::{bail_distro_shadow, bail_if_staged, bail_machine_shadow};
use super::{find, run_container_quiet};
use crate::container::{self, ensure_started, validate_name};
use crate::distro::spec::OriginSource;
use crate::distro::terminal;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

/// Migrate a `container machine` into a distro (`migrate`, `--in`),
/// carrying its root filesystem across with it. Unless `keep`, the
/// machine is removed once the distro exists. `target` renames the
/// result. Returns the distro's name.
///
/// The reverse — distro → machine — is [`migrate_to_machine`]
/// (`migrate --out`).
pub fn migrate(
    machine: &str,
    target: Option<String>,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    ensure_started()?;
    // Keeping the source means the result can't take its name — the
    // same-name machine+distro pair is exactly the shadow the create
    // paths refuse.
    if keep && target.is_none() {
        bail!("`--keep` needs a target name (`-n NAME`) — the source keeps its name");
    }
    // A journaled distro is already home; a journal-less labeled
    // container is pre-journal — adopted, not migrated. Naming it is
    // the explicit opt-in the journal gate asks for; `-n`/create
    // options don't apply since its live config is recorded as-is.
    // The journal check comes first: a stopped distro has no container
    // for `find` to see.
    if journal_present(machine) {
        bail!("`{machine}` is already a distro");
    }
    if find(machine).is_ok() {
        if target.is_some() || *opts != CreateOptions::default() {
            bail!(
                "`{machine}` is a pre-journal distro — `migrate` adopts its \
                 live config as-is; name and create options don't apply"
            );
        }
        adopt(machine)?;
        return Ok(format!(
            "`{machine}` adopted — its live config is now the journal"
        ));
    }
    if !container::list_machines()?.iter().any(|m| m.id == machine) {
        bail!("no machine or distro named `{machine}`");
    }
    let target = target.unwrap_or_else(|| machine.to_string());
    migrate_to_distro(machine, &target, keep, opts)
}

/// Machine → distro: clone the machine's rootfs into the new distro's
/// journal — its resting state — then materialize and boot.
fn migrate_to_distro(
    machine: &str,
    name: &str,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    validate_name(name)?;
    // Fail fast, before the machine is stopped or its disk cloned.
    bail_if_staged(name)?;
    if distro_exists(name)? {
        bail!("distro `{name}` already exists");
    }
    // The source machine's own name is taken over — a plain migration
    // removes it. A *different* machine's name (or the source's, kept)
    // is a shadow the distro would cast over it in `cm`.
    if container::list_machines()?
        .iter()
        .any(|m| m.id == name && (m.id != machine || keep))
    {
        bail_machine_shadow(name, opts.force)?;
    }
    let detail = container::inspect_machine(machine)?;
    let mut spec = spec_from_machine(name, &detail, opts)?;
    spec.origin = Some(stamped_origin(OriginSource::Machine {
        name: machine.to_string(),
    }));
    check_sources(&spec)?;
    let was_running = detail.is_running();
    if was_running {
        run_container_quiet(&["machine", "stop", machine])?;
    }
    container::warn_unverified_version();
    write_journal(name, &spec)?;
    let lock = DistroLock::acquire(name)?;
    match machine_rootfs(&detail)? {
        Some(src) => {
            let dst = journal_fs_path(name)?;
            with_journal_dir_unlocked(name, || {
                // `fs::copy` is a clonefile on APFS — instant and
                // copy-on-write.
                fs::copy(&src, &dst)
                    .with_context(|| format!("failed to clone {}", src.display()))?;
                set_locked(&dst, true)
            })?;
        }
        None => {
            eprintln!(
                "container-distro: machine `{machine}` has no rootfs to carry — the distro starts from its image"
            );
        }
    }
    if let Err(e) = materialize(name, &spec, &lock).and_then(|id| {
        if opts.no_boot {
            dismantle(name, &id, &lock)
        } else {
            boot(name, &id, &lock)
        }
    }) {
        if let Ok(info) = find(name) {
            let _ = run_container_quiet(&["delete", info.id()]);
        }
        let _ = remove_journal_dir(name);
        if was_running {
            let _ = boot_machine(machine);
        }
        return Err(e);
    }
    if !keep {
        if let Err(e) = run_container_quiet(&["machine", "rm", machine]) {
            eprintln!("container-distro: migrated, but removing machine `{machine}` failed: {e:#}");
        }
    } else {
        if name == machine {
            eprintln!(
                "container-distro: `{name}` is now both a machine and a distro; `cm` resolves it to the distro"
            );
        }
        if was_running {
            let _ = boot_machine(machine);
        }
    }
    if opts.set_default || no_effective_default(name)? {
        write_default(&DefaultType::Distro(name.to_string()))?;
    }
    let profiles = spec.resolve().0.terminal_profile;
    terminal::sync(name, profiles);
    if profiles {
        let _ = terminal::ensure_profile(name);
    }
    Ok(name.to_string())
}

/// A distro's ext4 rootfs: the journaled `rootfs.ext4` (its resting
/// home) first, then — for a first boot or a pre-journal distro — the
/// conventional `containers/<id>/rootfs.ext4` or an override source.
/// `None` before the first boot: the filesystem is still the pristine
/// image.
fn distro_rootfs(name: &str) -> Result<Option<PathBuf>> {
    let jfs = journal_fs_path(name)?;
    if jfs.is_file() {
        return Ok(Some(jfs));
    }
    let Some(dir) = find(name).ok().and_then(|i| container_dir(i.id()).ok()) else {
        return Ok(None);
    };
    if let Some(src) = fs::read_to_string(dir.join("runtime-configuration.json"))
        .ok()
        .and_then(|s| rootfs_override_source(&s))
        && src.is_file()
    {
        return Ok(Some(src));
    }
    let ext4 = dir.join("rootfs.ext4");
    Ok(ext4.is_file().then_some(ext4))
}

/// Migrate a distro into a `container machine` (`migrate --out`), the
/// mirror of [`migrate_to_distro`]: create the machine from the distro's
/// image — its internal boot materializes the plugin-state disk — stop
/// it, then clone the distro's `rootfs.ext4` over the machine's. No
/// tar/image round-trip. Unless `keep`, the distro is removed once the
/// machine exists. `target` renames the result. Returns the machine's
/// name.
pub fn migrate_to_machine(
    distro: &str,
    target: Option<String>,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    ensure_started()?;
    // Refuse distro-only flags before the distro is stopped — the same
    // check runs inside `create_machine` for the shared entry point.
    reject_machine_opts(opts)?;
    // The carried rootfs is already one arch — `machine create` can't
    // be pointed at another, so an explicit --arch would silently
    // no-op. Refuse it instead (the machine keeps the distro's arch).
    if opts.arch.is_some() {
        bail!(
            "`--arch` cannot be applied to `migrate --out` — the machine keeps the distro's architecture"
        );
    }
    // Keeping the source means the result can't take its name — the
    // same-name distro+machine pair is exactly the shadow the create
    // paths refuse.
    if keep && target.is_none() {
        bail!("`--keep` needs a target name (`-n NAME`) — the source keeps its name");
    }
    let name = target.unwrap_or_else(|| distro.to_string());
    validate_name(&name)?;
    if container::list_machines()?.iter().any(|m| m.id == name) {
        bail!("a machine named `{name}` already exists (use `-n` to pick another)");
    }
    // The source distro's own name is taken over — a plain migration
    // removes it. A *different* distro's name (or the source's, kept)
    // would hide the new machine behind it in `cm`.
    if name != distro || keep {
        bail_distro_shadow(&name, opts.force)?;
    }
    // A pre-journal distro named for `--out` is explicit opt-in —
    // adopt its live config first, then carry the disk out.
    if !journal_present(distro) && find(distro).is_ok() {
        adopt(distro)?;
    } else {
        ensure_journal(distro)?;
    }
    // Serialize against a concurrent `set`/`start`/`rm`, then converge
    // the distro to its resting state before cloning its disk.
    let mut lock = DistroLock::acquire(distro)?;
    container::warn_unverified_version();
    let spec = lock.spec()?;
    let was_running = if let Some(info) = find(distro).ok().filter(|i| i.is_running()) {
        refresh_conf_snapshots(distro, info.id(), &spec);
        run_container_quiet(&["stop", info.id()])?;
        true
    } else {
        false
    };
    converge(distro, &lock)?;
    let src = distro_rootfs(distro)?;
    if src.is_none() {
        eprintln!(
            "container-distro: distro `{distro}` has no rootfs to carry — the machine starts from its image"
        );
    }
    let image = machine_create_image(&spec.image, src.is_some())?;
    // An imported distro's image ref is the *distro's* tag on shared
    // content — `rm` untags it below. Give the machine its own
    // `local/machine-*` tag on the same image first so its recorded
    // reference stays valid; untag it again if the create fails.
    let machine_tag = imported_ref_sha(&image).map(|sha| machine_import_ref(&name, sha));
    let image = match &machine_tag {
        Some(t) if tag_image(&image, t).is_ok() => t.clone(),
        Some(_) => {
            eprintln!(
                "container-distro: warning: couldn't tag `{image}` for machine `{name}` — \
                 its recorded reference goes stale when `{distro}` is removed"
            );
            image
        }
        None => image,
    };
    if let Err(e) = create_machine(&name, &image, &spec, opts) {
        if image != spec.image {
            untag_image(&image);
        }
        if was_running {
            let _ = ensure_running_locked(distro, &mut lock);
        }
        return Err(e);
    }
    // Replace the machine's filesystem while it is stopped: clone to a
    // sibling then rename over its disk, so an interruption leaves the
    // old disk or the new one, never a partial file.
    let swap = (|| -> Result<()> {
        run_container_quiet(&["machine", "stop", &name])?;
        let Some(src) = src else { return Ok(()) };
        let detail = container::inspect_machine(&name)?;
        let dst = machine_rootfs(&detail)?.context("the new machine has no rootfs to replace")?;
        let mut tmp = dst.clone().into_os_string();
        tmp.push(".new");
        let tmp = PathBuf::from(tmp);
        let _ = fs::remove_file(&tmp);
        // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
        fs::copy(&src, &tmp).with_context(|| format!("failed to clone {}", src.display()))?;
        fs::rename(&tmp, &dst).with_context(|| format!("failed to install {}", dst.display()))?;
        Ok(())
    })();
    if let Err(e) = swap {
        // The machine holds only its image filesystem; remove it so a
        // retry isn't blocked, and restart the distro.
        if let Err(r) = run_container_quiet(&["machine", "rm", &name]) {
            eprintln!("container-distro: removing incomplete machine `{name}` failed: {r:#}");
        }
        if was_running {
            let _ = ensure_running_locked(distro, &mut lock);
        }
        return Err(e.context(format!(
            "failed to move `{distro}`'s filesystem into machine `{name}`"
        )));
    }
    // `machine create`'s internal boot already absorbed the first-boot
    // provisioning flake; this run verifies the swapped filesystem and
    // leaves the machine running, like `migrate --in` leaves the distro.
    if !opts.no_boot {
        let booted = (0..3).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            boot_machine(&name).is_ok()
        });
        if !booted {
            if was_running {
                let _ = ensure_running_locked(distro, &mut lock);
            }
            bail!(
                "machine `{name}` holds the migrated filesystem but failed to boot; \
                 distro `{distro}` left in place (see `container machine logs {name}`)"
            );
        }
    }
    if !keep {
        // The distro is already converged (container dismantled);
        // remove the journal and its images. The default marker is
        // left for the converge below — a stale `Distro` record
        // repoints to what remains.
        remove_journal_dir(distro)?;
        cleanup_preserved(distro);
        remove_snapshot_images(distro, None);
        terminal::remove(distro);
    } else {
        if name == distro {
            eprintln!(
                "container-distro: `{name}` is now both a distro and a machine; `cm` resolves the name to the distro"
            );
        }
        if was_running {
            let _ = ensure_running_locked(distro, &mut lock);
        }
    }
    // An explicit claim (or being the only usable target) makes the
    // machine `container`'s default *and* our recorded `Machine` —
    // unlike a lazy pick, which never touches `container`'s default.
    if opts.set_default || no_effective_default(&name)? {
        run_container_quiet(&["machine", "set-default", &name])?;
        write_default(&DefaultType::Machine(name.clone()))?;
    }
    Ok(name)
}
