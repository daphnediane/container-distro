/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The `sbin.distro` init assets each journal carries.

use super::find;
use super::journal::{ensure_state_locked, journal_dir, with_journal_dir_unlocked};
use super::lock::{chmod_nofollow, set_locked, unlock, write_nofollow};
use crate::distro::spec::INIT_DIR;
use crate::naming::state_dir;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const INIT_SCRIPT: &str = include_str!("../../assets/init");

const CREATE_USER_SCRIPT: &str = include_str!("../../assets/create-user.sh");

const GRANT_ADMIN_SCRIPT: &str = include_str!("../../assets/grant-admin.sh");

/// The init-assets flavor's directory name. The restricted flavor lacks
/// `grant-admin.sh`, so restricted guests have no privilege-granting
/// code at all.
pub(crate) fn assets_flavor(admin: bool) -> &'static str {
    if admin {
        "sbin.distro"
    } else {
        "sbin.distro.restricted"
    }
}

/// The files an `admin` flavor carries: `(name, embedded body)` pairs.
fn asset_files(admin: bool) -> Vec<(&'static str, &'static str)> {
    let mut files = vec![
        ("init", INIT_SCRIPT),
        ("create-user.sh", CREATE_USER_SCRIPT),
    ];
    if admin {
        files.push(("grant-admin.sh", GRANT_ADMIN_SCRIPT));
    }
    files
}

/// Write the init assets into `dir`, refreshing them if this build's
/// copies differ, then lock the directory and its files (`uchg`).
/// `admin` selects the flavor: only admin distros get `grant-admin.sh`.
///
/// The lock is what keeps a guest from rewriting these files through a
/// rw-shared home (`~/Library/Application Support` is inside it), so a
/// re-run unlocks the dir and each file before writing and re-locks
/// after. A failure mid-write leaves the dir unlocked — no worse than
/// never locking.
pub(crate) fn write_assets(dir: &Path, admin: bool) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    unlock(dir);
    if !admin {
        // No stray grant script may linger in the restricted flavor.
        let stray = dir.join("grant-admin.sh");
        unlock(&stray);
        match fs::remove_file(&stray) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            r => r.with_context(|| format!("failed to clean {}", dir.display()))?,
        }
    }
    for (name, body) in asset_files(admin) {
        write_locked_asset(&dir.join(name), body.as_bytes())?;
    }
    set_locked(dir, true)
}

/// Write one init asset at `path` (refreshing only when it differs),
/// `0755`, `uchg`-locked; a planted symlink is removed, never followed.
fn write_locked_asset(path: &Path, body: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove planted link {}", path.display()))?;
    }
    unlock(path);
    if fs::read(path).ok().as_deref() != Some(body) {
        write_nofollow(path, body, 0o755)?;
    } else {
        chmod_nofollow(path, 0o755)?;
    }
    set_locked(path, true)
}

/// The init-assets dir inside `name`'s journal — every journaled
/// distro mounts its own `sbin.distro`, like a machine bundle's
/// `sbin.machine`. Written fresh at each materialize, it can never
/// drift from this build the way a shared copy could.
pub(super) fn journal_assets_dir(name: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join("sbin.distro"))
}

/// Refresh both legacy init-assets flavors in the per-user state dir,
/// and re-lock the default-distro marker if one exists (it's only
/// locked when written, so an older build may have left it open). Only
/// containers a pre-journal-assets build materialized still mount these.
fn refresh_assets() -> Result<()> {
    let state = state_dir()?;
    write_assets(&state.join(assets_flavor(true)), true)?;
    write_assets(&state.join(assets_flavor(false)), false)?;
    let default = state.join("default-distro");
    if default.exists() {
        set_locked(&default, true)?;
    }
    Ok(())
}

/// Refresh the init assets the distro will actually mount at boot.
///
/// A per-distro mount source is rewritten in place (upgrading to this
/// build and the spec's admin flavor, and re-locking). Legacy mount
/// sources predate the per-distro assets: a shared per-user copy is
/// refreshed in place; an installed (plugin-dir) copy can't be
/// rewritten from here — a drifted `init` warns once, and `set` moves
/// the distro onto its own assets.
pub(super) fn refresh_mounted_assets(name: &str, admin: bool) -> Result<()> {
    ensure_state_locked()?;
    let mounted = find(name).ok().and_then(|info| {
        info.configuration
            .mounts
            .iter()
            .find(|m| m.destination == INIT_DIR)
            .map(|m| PathBuf::from(&m.source))
    });
    let own = journal_assets_dir(name)?;
    match mounted {
        Some(src) if src == own => with_journal_dir_unlocked(name, || write_assets(&own, admin)),
        Some(src) if !src.starts_with(state_dir()?) => {
            if fs::read(src.join("init")).ok().as_deref() != Some(INIT_SCRIPT.as_bytes()) {
                static WARN: std::sync::Once = std::sync::Once::new();
                WARN.call_once(|| {
                    eprintln!(
                        "container-distro: warning: `{name}`'s init assets differ from this build \
                         ({}) — `set` the distro to move it onto its journal's assets",
                        src.display()
                    );
                });
            }
            Ok(())
        }
        Some(_) => refresh_assets(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    use std::path::Path;
    use std::process::Stdio;

    use crate::container::{self, container_cmd};

    use crate::distro::spec::INIT_DIR;

    use super::super::lock::unlock;
    use super::super::test_support::skip;

    /// Image for the guest-script tests; `DISTRO_TEST_IMAGE` overrides
    /// the default. None when the service or image isn't available —
    /// tests then skip (with a notice) and never pull.
    fn test_image(test: &str) -> Option<String> {
        if !container::system_running() {
            skip(test, "container service not running");
            return None;
        }
        let image = env::var("DISTRO_TEST_IMAGE").unwrap_or_else(|_| "alpine:latest".to_string());
        let out = container_cmd()
            .args(["image", "list", "--quiet"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let suffix = format!("/{image}");
        if String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .any(|r| r == image || r.ends_with(&suffix))
        {
            Some(image)
        } else {
            skip(
                test,
                &format!("image {image} not present (set DISTRO_TEST_IMAGE)"),
            );
            None
        }
    }

    /// Run `sh ARGV...` in a throwaway container with the init assets
    /// mounted read-only at their production `/sbin.distro` path.
    /// Returns the exit code, or None if the run failed to start.
    fn sh_in_guest(image: &str, argv: &[&str]) -> Option<i32> {
        sh_in_guest_dir(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets")),
            image,
            argv,
        )
    }

    /// Like [`sh_in_guest`] but mounts `dir`, so tests can assemble a
    /// restricted init-assets flavor (no `grant-admin.sh`).
    fn sh_in_guest_dir(dir: &Path, image: &str, argv: &[&str]) -> Option<i32> {
        container_cmd()
            .args([
                "run",
                "--rm",
                "--entrypoint",
                "/bin/sh",
                "--volume",
                &format!("{}:{INIT_DIR}:ro", dir.display()),
            ])
            .arg(image)
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .ok()?
            .code()
    }

    #[test]
    fn locked_assets_resist_writes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("sbin.distro");
        write_assets(&d, true).unwrap();
        let init = d.join("init");
        // The boundary a guest sees through virtiofs, applied to the
        // owner too: no write, no unlink, no dir-entry churn, no rename
        // of the locked dir itself.
        assert!(fs::write(&init, "tampered").is_err());
        assert!(fs::remove_file(&init).is_err());
        assert!(fs::write(d.join("planted"), "x").is_err());
        assert!(fs::rename(&d, dir.path().join("moved")).is_err());
        assert!(fs::read_to_string(&init).unwrap() == INIT_SCRIPT);
        // Re-refreshing works through the lock, and the restricted
        // flavor drops grant-admin.sh even though it is locked.
        write_assets(&d, false).unwrap();
        assert!(!d.join("grant-admin.sh").exists());
        write_assets(&d, true).unwrap();
        assert!(d.join("grant-admin.sh").is_file());
        // Unlock so tempdir cleanup can remove the tree.
        for n in ["init", "create-user.sh", "grant-admin.sh"] {
            unlock(&d.join(n));
        }
        unlock(&d);
    }

    /// Every unsafe CONTAINER_* value must fail validation before any
    /// /etc file is touched.
    #[test]
    fn create_user_rejects_unsafe_env() {
        let Some(image) = test_image("create_user_rejects_unsafe_env") else {
            return;
        };
        let script = r#"
            s=/sbin.distro/create-user.sh
            rc=0
            for u in '' '-evil' '.hidden' 'bad name' 'x:y' 'a/b' 'u$'; do
                if CONTAINER_USER=$u CONTAINER_UID=501 CONTAINER_GID=20 \
                   CONTAINER_HOME=/home/u $s 2>/dev/null; then rc=1; fi
            done
            for v in '' x '1.2' '501;rm' ' 1'; do
                if CONTAINER_USER=u CONTAINER_UID=$v CONTAINER_GID=20 \
                   CONTAINER_HOME=/home/u $s 2>/dev/null; then rc=1; fi
                if CONTAINER_USER=u CONTAINER_UID=501 CONTAINER_GID=$v \
                   CONTAINER_HOME=/home/u $s 2>/dev/null; then rc=1; fi
            done
            for h in '' relative '/home/../etc' '/x/..' '../y' '/a b' '/x:y'; do
                if CONTAINER_USER=u CONTAINER_UID=501 CONTAINER_GID=20 \
                   CONTAINER_HOME=$h $s 2>/dev/null; then rc=1; fi
            done
            exit $rc
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    /// Re-provisioning must converge: admin edits and deletions survive,
    /// no duplicate entries appear, and a changed CONTAINER_USER still
    /// gets provisioned.
    #[test]
    fn create_user_is_idempotent() {
        let Some(image) = test_image("create_user_is_idempotent") else {
            return;
        };
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne CONTAINER_ADMIN=1
            mkdir -p /etc/doas.d
            /sbin.distro/init -u
            grep -q '^daphne:x:501:20::/home/daphne:' /etc/passwd
            grep -q '^daphne ' /etc/sudoers.d/daphne
            grep -q 'daphne' /etc/doas.d/daphne.conf
            [ -f /etc/.distro.user.daphne ]
            [ -f /etc/.distro.admin.daphne ]
            # An admin removal survives re-provisioning (sentinel gate).
            rm /etc/sudoers.d/daphne /etc/doas.d/daphne.conf
            /sbin.distro/init -u
            [ ! -e /etc/sudoers.d/daphne ]
            [ ! -e /etc/doas.d/daphne.conf ]
            [ "$(grep -c '^daphne:' /etc/passwd)" = 1 ]
            # A different CONTAINER_USER is still provisioned.
            CONTAINER_USER=other CONTAINER_UID=502 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/other /sbin.distro/init -u
            grep -q '^other:x:502:20::/home/other:' /etc/passwd
            grep -q '^other ' /etc/sudoers.d/other
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    /// `init -u` runs on every boot; create-user.sh idempotency means
    /// admin edits persist while provisioning still converges.
    #[test]
    fn init_u_converges() {
        let Some(image) = test_image("init_u_converges") else {
            return;
        };
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne CONTAINER_ADMIN=1
            /sbin.distro/init -u
            [ -f /etc/.distro.initialized ]
            rm /etc/sudoers.d/daphne
            /sbin.distro/init -u
            [ ! -e /etc/sudoers.d/daphne ]
            CONTAINER_USER=other CONTAINER_UID=502 /sbin.distro/init -u
            grep -q '^other:' /etc/passwd
            [ -f /etc/.distro.user.other ]
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    /// A restricted distro mounts an assets dir without grant-admin.sh:
    /// `init -u` provisions the account but no privilege files exist.
    #[test]
    fn restricted_boot_grants_nothing() {
        let Some(image) = test_image("restricted_boot_grants_nothing") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        for f in ["init", "create-user.sh"] {
            fs::copy(
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets")).join(f),
                dir.path().join(f),
            )
            .unwrap();
        }
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne
            /sbin.distro/init -u
            grep -q '^daphne:' /etc/passwd
            [ ! -e /etc/sudoers.d/daphne ]
            [ ! -e /etc/doas.d/daphne.conf ]
            [ ! -e /etc/.distro.admin.daphne ]
        "#;
        assert_eq!(
            sh_in_guest_dir(dir.path(), &image, &["-c", script]),
            Some(0)
        );
    }

    /// Without CONTAINER_ADMIN (a container created before the split,
    /// or `admin=never`) grant-admin.sh defers: no grant, no sentinel —
    /// a later armed `set --sudo` still grants.
    #[test]
    fn grant_admin_requires_env() {
        let Some(image) = test_image("grant_admin_requires_env") else {
            return;
        };
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne
            /sbin.distro/create-user.sh
            /sbin.distro/grant-admin.sh
            [ ! -e /etc/sudoers.d/daphne ]
            [ ! -e /etc/.distro.admin.daphne ]
            CONTAINER_ADMIN=1 /sbin.distro/grant-admin.sh
            grep -q '^daphne ' /etc/sudoers.d/daphne
            [ -f /etc/.distro.admin.daphne ]
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }
}
