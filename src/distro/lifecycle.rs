/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `start`, `stop`, and `delete`.

use super::adopt::{ensure_journal, ensure_state};
use super::converge::{converge, dismantle, materialize};
use super::create::boot;
use super::default::{default_name, write_default};
use super::images::remove_snapshot_images;
use super::journal::{
    DistroLock, journal_dir, journal_names, journal_present, preserved_dir, preserved_journal,
    preserved_rootfs, refresh_conf_snapshots, with_journal_root_unlocked, with_preserved_unlocked,
};
use super::lock::unlock;
use super::{distro_name, distros, find, run_container_quiet};
use crate::container::{self, ensure_started};
use crate::distro::terminal;
use anyhow::{Context, Result, bail};
use std::fs;

pub fn start(name: &str) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_journal(name)?;
    let lock = DistroLock::acquire(name)?;
    let id = match converge(name, &lock)? {
        Some(info) => info.id().to_string(),
        None => {
            let spec = lock.spec()?;
            materialize(name, &spec, &lock)?
        }
    };
    // Runs on an already-running distro too — `init -u` and the port
    // report are idempotent.
    boot(name, &id, &lock)
}

/// `container distro stop` / `cm -t`: stop the container and dismantle
/// it — the distro rests as its journal.
pub fn stop(names: &[String]) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    for n in names {
        ensure_journal(n)?;
        let lock = DistroLock::acquire(n)?;
        if let Ok(info) = find(n) {
            if info.is_running() {
                // Last chance to capture guest config — `cp` needs a
                // running container.
                if let Ok(spec) = lock.spec() {
                    refresh_conf_snapshots(n, info.id(), &spec);
                }
                run_container_quiet(&["stop", info.id()])?;
            }
            dismantle(n, info.id(), &lock)?;
        }
    }
    Ok(())
}

/// Remove anything a pre-journal `set` staged for `name`; true when
/// something was. Called without the journal lock (there may be no
/// journal to lock).
pub(super) fn cleanup_preserved(name: &str) -> bool {
    if preserved_dir().is_err() {
        return false;
    }
    let mut removed = false;
    let _ = with_preserved_unlocked(|| {
        for p in [preserved_rootfs(name), preserved_journal(name)]
            .into_iter()
            .flatten()
        {
            unlock(&p);
            removed |= fs::remove_file(&p).is_ok();
        }
        Ok(())
    });
    removed
}

/// Remove `name`'s journal (spec, disk, and dir), clearing `uchg` flags
/// first — including a dir that was never fully written or was planted.
pub(super) fn remove_journal_dir(name: &str) -> Result<()> {
    let dir = journal_dir(name)?;
    if !dir.exists() {
        return Ok(());
    }
    with_journal_root_unlocked(|| {
        // Every entry the journal can hold — `uchg` blocks removal.
        let assets = dir.join("sbin.distro");
        if let Ok(entries) = fs::read_dir(&assets) {
            for e in entries.flatten() {
                unlock(&e.path());
            }
        }
        unlock(&assets);
        for f in [
            "rootfs.ext4",
            "spec.json",
            "wsl.conf",
            "wsl-distribution.conf",
        ] {
            unlock(&dir.join(f));
        }
        unlock(&dir);
        fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))
    })
}

pub fn delete(force: bool, names: &[String]) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_state()?;
    for n in names {
        let container = find(n).ok();
        if container.is_none() && !journal_present(n) {
            // Not a distro — but a half-written or unsigned journal dir,
            // or a filesystem staged on a pre-journal build, can still
            // be cleaned up.
            let mut removed = journal_dir(n)?.exists();
            if removed {
                remove_journal_dir(n)?;
            }
            removed |= cleanup_preserved(n);
            if !removed {
                bail!("no distro named `{n}`");
            }
            eprintln!("removed `{n}`'s journal/staging leftovers");
            continue;
        }
        // A journal-less distro container — pre-journal leftovers the
        // migration didn't reach, or planted — is deleted without
        // adoption; there's nothing to lock without a journal.
        let _lock = journal_present(n)
            .then(|| DistroLock::acquire(n))
            .transpose()?;
        if let Some(info) = &container {
            if info.is_running() {
                if !force {
                    bail!("distro `{n}` is running (stop it first or use --force)");
                }
                run_container_quiet(&["stop", info.id()])?;
            }
            run_container_quiet(&["delete", info.id()])?;
        }
        remove_journal_dir(n)?;
        cleanup_preserved(n);
        remove_snapshot_images(n, None);
        terminal::remove(n);
        if default_name().as_deref() == Some(n) {
            write_default(None)?;
        }
    }
    Ok(())
}

/// Delete every distro — `delete --all`, the package uninstaller's
/// cleanup path. Names are the union of journaled distros (stopped
/// distros exist only there) and labeled containers (pre-journal
/// leftovers exist only there); `delete` handles both shapes.
pub fn delete_all(force: bool) -> Result<()> {
    let mut names = journal_names()?;
    names.extend(distros()?.iter().filter_map(distro_name).map(String::from));
    names.sort();
    names.dedup();
    delete(force, &names)
}
