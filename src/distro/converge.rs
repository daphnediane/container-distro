/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Materializing a container from the journal and dismantling it back.

use super::assets::{journal_assets_dir, write_assets};
use super::create::create_container;
use super::journal::{DistroLock, container_dir, journal_fs_path, with_journal_dir_unlocked};
use super::lock::{set_locked, unlock};
use super::machine::rootfs_override_source;
use super::set::warn_spec_drift;
use super::{find, run_container_quiet, runtime_id};
use crate::container::ContainerInfo;
use crate::distro::spec::DistroSpec;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Mark `rootfs` as the container's root filesystem via `rootFsOverride`
/// in `runtime-configuration.json`, so `container start` mounts it —
/// otherwise the daemon copies the image snapshot over `rootfs.ext4`,
/// which fails once the file already exists. This is the same mechanism
/// `container machine` uses for its plugin-state disks.
fn set_rootfs_override(dir: &Path, rootfs: &Path) -> Result<()> {
    let cfg = dir.join("runtime-configuration.json");
    let mut v: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&cfg).with_context(|| format!("failed to read {}", cfg.display()))?,
    )?;
    v["options"]["rootFsOverride"] = serde_json::json!({
        "type": {"block": {"sync": {"fsync": {}}, "format": "ext4", "cache": {"on": {}}}},
        "source": rootfs,
        "destination": "/",
        "options": [],
    });
    fs::write(&cfg, serde_json::to_string(&v)?)
        .with_context(|| format!("failed to write {}", cfg.display()))
}

/// Create `name`'s container fresh from `spec` under a new randomized
/// runtime id and attach the journaled disk — the machine model's
/// materialization (`<name>-<6hex>`, like `poc-a07fee`). The disk is
/// unlocked for as long as the container exists (the daemon mounts it
/// read-write); the journaled spec and journal dir never become
/// mutable. Returns the container's runtime id.
///
/// A failure leaves the distro stopped with journal and disk intact:
/// like `machine boot`, there is no stale recorded configuration to
/// fall back to — the next start retries a fresh materialization.
pub(super) fn materialize(name: &str, spec: &DistroSpec, _lock: &DistroLock) -> Result<String> {
    // The init assets a distro mounts live in its own journal dir —
    // written fresh for this build and the spec's admin flavor, so no
    // shared or plugin-installed copy can drift out from under it.
    let assets = journal_assets_dir(name)?;
    let (resolved, _) = spec.resolve();
    with_journal_dir_unlocked(name, || write_assets(&assets, resolved.admin))?;
    let id = runtime_id(name);
    create_container(spec, &id)?;
    let fs_path = journal_fs_path(name)?;
    if fs_path.is_file() {
        set_rootfs_override(&container_dir(&id)?, &fs_path)?;
        set_locked(&fs_path, false)?;
    }
    Ok(id)
}

/// Dismantle `name`'s stopped container `id`: move the disk into the
/// journal when it still lives under `containers/` (first boot, or a
/// distro that predates the journal), then delete the container.
/// Whatever the runtime configuration recorded — fields `DistroSpec`
/// models and ones it doesn't — is discarded with the container.
/// Afterwards the distro is its journal alone.
pub(super) fn dismantle(name: &str, id: &str, _lock: &DistroLock) -> Result<()> {
    let dir = container_dir(id)?;
    let jfs = journal_fs_path(name)?;
    // The journal path is the disk's home once the container mounted it
    // via override — nothing to move. An un-overridden container's disk
    // still lives at the conventional path.
    let mounted_journal = fs::read_to_string(dir.join("runtime-configuration.json"))
        .ok()
        .and_then(|s| rootfs_override_source(&s))
        .is_some_and(|s| s == jfs);
    let conv = dir.join("rootfs.ext4");
    if !mounted_journal && conv.is_file() {
        with_journal_dir_unlocked(name, || {
            unlock(&jfs);
            if jfs.exists() {
                // A journal disk can't have been mounted while the
                // container carried its own — the container's is the
                // truth; replace.
                fs::remove_file(&jfs)
                    .with_context(|| format!("failed to remove stale {}", jfs.display()))?;
            }
            fs::rename(&conv, &jfs)
                .with_context(|| format!("failed to move {} into the journal", conv.display()))
        })?;
    }
    run_container_quiet(&["delete", id])?;
    // The disk rests locked while nothing mounts it.
    if jfs.is_file() {
        set_locked(&jfs, true)?;
    }
    Ok(())
}

/// Converge `name` to its canonical resting state under the lock: a
/// leftover stopped container — a bare `container stop`, a crash, an
/// interrupted `set` — is dismantled, its disk rejoining the journal.
/// Returns the container only when it is *running*.
pub(super) fn converge(name: &str, lock: &DistroLock) -> Result<Option<ContainerInfo>> {
    match find(name).ok() {
        Some(info) if info.is_running() => {
            warn_spec_drift(name, &info, lock);
            Ok(Some(info))
        }
        Some(info) => {
            eprintln!(
                "container-distro: `{name}` has a leftover container — \
                 regenerating it from the journal"
            );
            dismantle(name, info.id(), lock)?;
            Ok(None)
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use std::path::Path;

    #[test]
    fn rootfs_override_patches_runtime_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("runtime-configuration.json"),
            r#"{"options":{"autoRemove":true},"containerConfiguration":{"id":"d1"}}"#,
        )
        .unwrap();
        set_rootfs_override(dir.path(), Path::new("/keep/rootfs.ext4")).unwrap();
        let v: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.path().join("runtime-configuration.json")).unwrap(),
        )
        .unwrap();
        let o = &v["options"]["rootFsOverride"];
        assert_eq!(o["source"], "/keep/rootfs.ext4");
        assert_eq!(o["destination"], "/");
        assert_eq!(o["type"]["block"]["format"], "ext4");
        // Existing options survive the patch.
        assert_eq!(v["options"]["autoRemove"], true);
        assert_eq!(v["containerConfiguration"]["id"], "d1");
    }
}
