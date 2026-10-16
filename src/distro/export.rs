/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `export`: a distro rootfs as a tar.

use super::adopt::ensure_journal;
use super::converge::converge;
use super::create::ensure_running_locked;
use super::journal::{DistroLock, container_dir, journal_fs_path, refresh_conf_snapshots};
use super::machine::{ScratchContainer, export_scratch_name, scratch_image};
use super::{find, run_container, run_container_quiet};
use crate::container::{self, ensure_started};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

/// `container export` deletes an existing output directory
/// (apple/container#2325) — refuse before it can.
pub(super) fn check_export_output(o: &Path) -> Result<()> {
    if o.is_dir() {
        bail!("export output `{}` is an existing directory", o.display());
    }
    Ok(())
}

/// `container distro export`. `container export` snapshots
/// `containers/<id>/rootfs.ext4` literally — a distro mounts its disk
/// from the journal via `rootFsOverride`, so export can't see it (the
/// same gap `export_machine` works around). The workaround is the same
/// too: clone the journaled disk into a scratch container that exports
/// normally, then delete it. A running distro is stopped for a
/// crash-consistent clone, then restarted.
pub fn export(name: &str, output: Option<&Path>) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    if let Some(o) = output {
        check_export_output(o)?;
    }
    let scratch = ScratchContainer::create(
        &export_scratch_name(name),
        &scratch_image(lock.spec()?.image.as_str())?,
        name,
    )?;
    // Stop + converge before cloning — a running disk's clone would
    // only be crash-consistent.
    let was_running = if let Some(info) = find(name).ok().filter(|i| i.is_running()) {
        if let Ok(spec) = lock.spec() {
            refresh_conf_snapshots(name, info.id(), &spec);
        }
        run_container_quiet(&["stop", info.id()])?;
        true
    } else {
        false
    };
    converge(name, &lock)?;
    let cloned = (|| -> Result<()> {
        let src = journal_fs_path(name)?;
        if !src.is_file() {
            bail!("distro `{name}` has no root filesystem to export (it has never been started)");
        }
        // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
        fs::copy(&src, container_dir(&scratch.0)?.join("rootfs.ext4"))
            .with_context(|| format!("failed to clone {}", src.display()))?;
        Ok(())
    })();
    if was_running && let Err(e) = ensure_running_locked(name, &mut lock) {
        eprintln!("container-distro: warning: failed to restart distro `{name}`: {e:#}");
    }
    cloned?;
    let mut args = vec!["export".to_string(), scratch.0.clone()];
    if let Some(o) = output {
        args.push("--output".into());
        args.push(o.to_string_lossy().into_owned());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container(&args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn export_output_rejects_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_export_output(dir.path()).is_err());
        // A missing path and an existing regular file are both fine —
        // `export` refuses only when it would clobber a directory.
        let missing = dir.path().join("out.tar");
        assert!(check_export_output(&missing).is_ok());
        let file = dir.path().join("exists.tar");
        fs::write(&file, b"").unwrap();
        assert!(check_export_output(&file).is_ok());
    }
}
