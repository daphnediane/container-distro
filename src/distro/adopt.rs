/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Upgrade paths: adopting pre-journal distros and folding legacy `preserved/` state.

use super::create::stamped_origin;
use super::host::host_home;
use super::journal::{
    ensure_state_locked, journal_dir, journal_fs_path, journal_present, preserved_dir,
    preserved_journal, preserved_rootfs, with_journal_dir_unlocked, with_preserved_unlocked,
    write_journal,
};
use super::lifecycle::cleanup_preserved;
use super::lock::{is_locked, set_locked, unlock, write_nofollow};
use super::{distro_name, distros, find};
use crate::container::{self, ensure_started};
use crate::distro::spec::{DistroSpec, JournalSpec, OriginSource};
use crate::naming::state_dir;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::{env, fs};

/// `state_dir()/journalized` — the one-time migration marker. Its
/// presence + `uchg` flag means the pre-journal migration pass already
/// ran; the flag can't be set through a shared home, so an unlocked
/// marker is forged and the pass runs anyway.
fn migrated_marker() -> Result<PathBuf> {
    state_dir().map(|d| d.join("journalized"))
}

/// Auto-adoption sunsets after the 1.1 line — drop the pass in 1.2.0.
/// `CARGO_PKG_VERSION` may carry a `-dev` prerelease.
fn auto_adoption_supported() -> bool {
    auto_adoption_supported_in(env!("CARGO_PKG_VERSION"))
}

fn auto_adoption_supported_in(version: &str) -> bool {
    let mut it = version
        .split('-')
        .next()
        .unwrap_or("0.0.0")
        .split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0));
    match (it.next().unwrap_or(0), it.next().unwrap_or(0)) {
        (0, _) => true,
        (1, m) => m <= 1,
        _ => false,
    }
}

/// Basenames of staged `preserved/<name>.{json,ext4}` files — either
/// half of an interrupted `set`'s staging.
fn staged_preserved_names() -> Result<Vec<String>> {
    let dir = preserved_dir()?;
    let mut names = Vec::new();
    for e in fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "json" || x == "ext4")
            && let Some(n) = p.file_stem().and_then(|s| s.to_str())
            && !names.iter().any(|m| m == n)
        {
            names.push(n.to_string());
        }
    }
    Ok(names)
}

/// The one-time pre-journal migration, marker-gated: while the marker
/// is absent, journal-less distro containers and staged `preserved/`
/// pairs are still legitimately ours — the world pre-journal builds
/// left behind — so they're folded in wholesale and the marker is
/// written `uchg`-locked. Afterwards a journal-less labeled container
/// can only be planted (a guest can write `containers/<name>/` through
/// a shared home, but can't set `uchg` — and can't forge the marker),
/// so [`ensure_journal`] refuses it and [`adopt`] is the explicit
/// opt-in. Past the version sunset no auto-adoption runs at all.
pub(super) fn migrate_once() -> Result<()> {
    let marker = migrated_marker()?;
    if marker.exists() {
        if is_locked(&marker) {
            return Ok(());
        }
        let _ = fs::remove_file(&marker);
        eprintln!(
            "container-distro: removed a forged migration marker (unlocked — guests can't set uchg)"
        );
    }
    ensure_state_locked()?;
    // The pass needs `container list`; if the daemon can't be queried
    // there's nothing to enumerate — retry next run rather than close
    // the window early.
    let Ok(containers) = distros() else {
        return Ok(());
    };
    if auto_adoption_supported() {
        for name in staged_preserved_names()? {
            migrate_legacy_preserved(&name)?;
        }
        for c in containers {
            let Some(name) = distro_name(&c).map(String::from) else {
                continue;
            };
            if journal_present(&name) {
                continue;
            }
            match DistroSpec::from_container(&c, &host_home()?)
                .map(|spec| write_journal(&name, &spec))
            {
                Ok(Ok(())) => eprintln!("container-distro: adopted pre-journal distro `{name}`"),
                _ => eprintln!(
                    "container-distro: couldn't adopt `{name}` automatically — \
                     `container distro migrate {name}` (or `rm {name}`)"
                ),
            }
        }
    }
    write_nofollow(&marker, b"journal migration done\n", 0o644)?;
    set_locked(&marker, true)?;
    Ok(())
}

/// Lock the state directories and run the one-time migration — what
/// ops call before trusting journal state.
pub(super) fn ensure_state() -> Result<()> {
    ensure_state_locked()?;
    migrate_once()
}

/// Fold a legacy `preserved/<name>.{json,ext4}` pair — the pre-journal
/// `set` staging — into `name`'s journal. The `.json` holds the spec
/// the interrupted recreate was heading for. No-op when nothing is
/// staged; a staged `.json` without `uchg` can't be ours (the dir is
/// locked at rest and guests can't set the flag) — dropped, not read.
fn migrate_legacy_preserved(name: &str) -> Result<()> {
    let staged = preserved_rootfs(name)?;
    let journal = preserved_journal(name)?;
    if !staged.exists() && !journal.exists() {
        return Ok(());
    }
    // The `.json` is the spec the interrupted `set` was heading for.
    // Missing — the `set` died between staging the disk and writing
    // it — or unlocked — planted, guests can't set `uchg` — it can't
    // be trusted. A surviving container supersedes the staged disk
    // (adoption covers it), so the leftovers go; otherwise keep the
    // disk for `list`/`rm` to surface rather than silently deleting
    // someone's filesystem.
    if !journal.exists() || !is_locked(&journal) {
        if journal.exists() {
            eprintln!(
                "container-distro: ignoring `{name}`'s unlocked staged journal — \
                 a guest can't set `uchg`, so it wasn't written by a `set`"
            );
        }
        if find(name).is_ok() {
            cleanup_preserved(name);
        } else {
            with_preserved_unlocked(|| {
                unlock(&journal);
                let _ = fs::remove_file(&journal);
                Ok(())
            })?;
        }
        return Ok(());
    }
    let js: JournalSpec = serde_json::from_slice(
        &fs::read(&journal)
            .with_context(|| format!("`{name}`'s interrupted `set` journal is unreadable"))?,
    )
    .context("invalid `set` journal")?;
    let (spec, _) = js.into_spec();
    if spec.name != name {
        bail!(
            "`set` journal for `{name}` names `{}` — refusing to trust it",
            spec.name
        );
    }
    write_journal(name, &spec)?;
    with_preserved_unlocked(|| {
        unlock(&journal);
        let _ = fs::remove_file(&journal);
        if staged.exists() {
            unlock(&staged);
            let dst = journal_fs_path(name)?;
            with_journal_dir_unlocked(name, || {
                fs::rename(&staged, &dst)
                    .with_context(|| format!("failed to move {}", staged.display()))?;
                set_locked(&dst, true)
            })?;
        }
        Ok(())
    })?;
    eprintln!("container-distro: folded `{name}`'s staged filesystem into its journal");
    Ok(())
}

/// Bring `name`'s journal into existence when the distro predates it.
/// The one-time migration ([`migrate_once`]) folded everything ours
/// already; anything still journal-less here is foreign — a planted
/// "old-style" distro would be trusted verbatim, so it isn't.
/// [`adopt`] is the explicit opt-in. A journal dir that exists but
/// isn't `uchg`-locked is refused — the flag can't be set through a
/// shared mount, so it wasn't written by us. Errors when `name` is no
/// distro at all.
pub(super) fn ensure_journal(name: &str) -> Result<()> {
    ensure_state()?;
    if journal_present(name) {
        return Ok(());
    }
    if find(name).is_ok() {
        bail!(
            "`{name}` predates journaled state — `container distro migrate {name}` \
             records its current config, `container distro rm {name}` deletes it"
        );
    }
    let dir = journal_dir(name)?;
    if dir.exists() {
        bail!(
            "`{name}`'s journal at {} is unusable ({}); `container distro rm {name}` removes it",
            dir.display(),
            if is_locked(&dir) {
                "never finished being written"
            } else {
                "not host-locked"
            }
        );
    }
    bail!("no distro named `{name}`")
}

/// Journal a pre-journal distro's live configuration under the user's
/// explicit say-so (`container distro migrate NAME`, `cm --migrate`).
/// This is the only trust-the-container path past the one-time
/// migration — running it on a name is blessing whatever that
/// container's recorded config says, same trust posture as
/// `--import`-ing a tarball.
pub fn adopt(name: &str) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_state()?;
    if journal_present(name) {
        println!("`{name}` is already journaled");
        return Ok(());
    }
    let info = find(name)?;
    let mut spec = DistroSpec::from_container(&info, &host_home()?)?;
    spec.origin = Some(stamped_origin(OriginSource::Adopted));
    write_journal(name, &spec)?;
    println!("adopted `{name}` — its live config is now the journal");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Auto-adoption of pre-journal distros is a sunsetted migration
    /// path — supported through the 1.1 line, gone in 1.2.
    #[test]
    fn adoption_sunset_version_gate() {
        for v in ["0.4.2", "0.5.0-dev", "1.0.0", "1.1.0", "1.1.9"] {
            assert!(auto_adoption_supported_in(v), "{v}");
        }
        for v in ["1.2.0", "2.0.0"] {
            assert!(!auto_adoption_supported_in(v), "{v}");
        }
    }
}
