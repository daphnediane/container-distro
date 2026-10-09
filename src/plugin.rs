/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Registering `container-distro` as a `container` CLI plugin.
//!
//! `container` scans `<prefix>/libexec/container-plugins/<name>/` for a
//! `config.toml` plus `bin/<name>`; a config without `servicesConfig` is a
//! CLI plugin, and `container <name> …` execs `bin/<name>`.

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::naming::APP_NAME;
use anyhow::{Context, Result, bail};

use crate::ops;

/// The plugin (and subcommand) name.
pub const PLUGIN_NAME: &str = "distro";
const ABSTRACT: &str = "Machine-like distros with extra mounts and published ports";

/// The plugin's `config.toml`.
#[must_use]
pub fn config_toml() -> String {
    format!(
        "abstract = \"{ABSTRACT} ({APP_NAME})\"\nauthor = \"{APP_NAME}\"\nversion = \"{}\"\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// `<prefix>/libexec/container-plugins` for a `container` binary at
/// `<prefix>/bin/container`.
#[must_use]
pub fn plugin_root_for(container_bin: &Path) -> Option<PathBuf> {
    Some(
        container_bin
            .parent()?
            .parent()?
            .join("libexec/container-plugins"),
    )
}

fn find_on_path(name: &Path) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

fn container_bin() -> Result<PathBuf> {
    // CONTAINER_CLI wins; then the documented install location; then PATH.
    let found = if let Some(name) = env::var_os("CONTAINER_CLI").map(PathBuf::from) {
        if name.components().count() > 1 {
            Some(name)
        } else {
            find_on_path(&name)
        }
    } else {
        let installed = Path::new(crate::container::INSTALLED_CONTAINER_PATH);
        if installed.is_file() {
            Some(installed.to_path_buf())
        } else {
            find_on_path(Path::new("container"))
        }
    };
    let bin = found.context("`container` not found (expected its install location or PATH)")?;
    fs::canonicalize(&bin).with_context(|| format!("failed to resolve {}", bin.display()))
}

pub(crate) fn plugin_dir(root: Option<PathBuf>) -> Result<PathBuf> {
    let root = match root {
        Some(r) => r,
        None => plugin_root_for(&container_bin()?)
            .context("cannot derive the plugin directory from the `container` path")?,
    };
    Ok(root.join(PLUGIN_NAME))
}

fn permission_hint(e: std::io::Error, what: &Path) -> anyhow::Error {
    let err = anyhow::Error::from(e);
    if err
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == ErrorKind::PermissionDenied)
    {
        err.context(format!(
            "cannot write {} (re-run with sudo, or pass --plugin-dir)",
            what.display()
        ))
    } else {
        err.context(format!("failed to write {}", what.display()))
    }
}

/// Whether `a` and `b` are the same underlying file (device + inode).
fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(a), Ok(b)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    a.dev() == b.dev() && a.ino() == b.ino()
}

pub fn install(root: Option<PathBuf>, from: Option<&Path>) -> Result<()> {
    let dir = plugin_dir(root)?;
    let exe = match from {
        Some(f) => {
            fs::canonicalize(f).with_context(|| format!("failed to resolve {}", f.display()))?
        }
        None => fs::canonicalize(env::current_exe()?)?,
    };
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| permission_hint(e, &bin_dir))?;
    let config = dir.join("config.toml");
    fs::write(&config, config_toml()).map_err(|e| permission_hint(e, &config))?;
    // Init assets live next to the binary: outside `$HOME`, root-owned
    // for a standard install, so no share into a guest can reach what
    // runs as PID 1. `assets_dir` prefers this copy when it matches the
    // running build; the per-user copies are the fallback, so a failed
    // write here (e.g. a `--plugin-dir` the user can't own) is a
    // warning, not a failure.
    for admin in [true, false] {
        let assets = dir.join(ops::assets_flavor(admin));
        if let Err(e) = ops::write_assets(&assets, admin) {
            eprintln!(
                "container-distro: warning: cannot install init assets ({e:#}); \
                 distros will use the per-user copies"
            );
        }
    }
    // Copy, not symlink: the plugin dir is typically root-owned while the
    // build output is user-writable — a link would let whoever owns the
    // binary replace what other users exec via `container distro`.
    let bin = bin_dir.join(PLUGIN_NAME);
    // Running `install-plugin` *via the installed plugin* finds itself as
    // the source; leave it alone. (Re-install from a newer build instead:
    // run that binary's install-plugin, or pass --from.) A symlink — even
    // one pointing at this binary — is replaced with a copy.
    if bin
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_file())
        && same_file(&bin, &exe)
    {
        println!(
            "`container {PLUGIN_NAME}` is already installed ({})",
            bin.display()
        );
        println!("  to update, run install-plugin from the new binary or pass --from");
        return Ok(());
    }
    if bin.symlink_metadata().is_ok() {
        fs::remove_file(&bin).map_err(|e| permission_hint(e, &bin))?;
    }
    // fs::copy is fclonefileat/fcopyfile on macOS — an APFS clone (CoW)
    // where the filesystem supports it.
    fs::copy(&exe, &bin).map_err(|e| permission_hint(e, &bin))?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
        .map_err(|e| permission_hint(e, &bin))?;
    println!("Installed `container {PLUGIN_NAME}` ({})", bin.display());
    println!("  copied from {}", exe.display());
    Ok(())
}

/// Remove `path` if present; warn (don't fail) on anything else.
fn remove_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => println!("Removed {}", path.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => eprintln!("container-distro: cannot remove {}: {e}", path.display()),
    }
}

/// Remove `dir` only if empty, so uninstall can never delete files it
/// didn't install. Returns false (with a warning) when files remain.
fn remove_dir_if_empty(dir: &Path) -> bool {
    match fs::remove_dir(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == ErrorKind::NotFound => true,
        Err(e) => {
            eprintln!("container-distro: {} left behind: {e}", dir.display());
            false
        }
    }
}

pub fn uninstall(root: Option<PathBuf>) -> Result<()> {
    let dir = plugin_dir(root)?;
    let config = dir.join("config.toml");
    match fs::read_to_string(&config) {
        Ok(c) if c.contains(ABSTRACT) => {}
        Ok(_) => bail!("{} was not installed by container-distro", dir.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            println!("Not installed ({})", dir.display());
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    // Delete only what install writes — the files, then the dirs iff
    // they end up empty. Check-then-`remove_dir_all` could take out
    // whatever a renamed directory held (C11); this can't. The assets
    // are `uchg`-locked, so unlock before removing.
    let bin_dir = dir.join("bin");
    remove_file(&bin_dir.join(PLUGIN_NAME));
    remove_file(&config);
    for admin in [true, false] {
        let assets = dir.join(ops::assets_flavor(admin));
        ops::unlock(&assets);
        for name in ["init", "create-user.sh", "grant-admin.sh"] {
            let p = assets.join(name);
            ops::unlock(&p);
            remove_file(&p);
        }
        remove_dir_if_empty(&assets);
    }
    let bin_gone = remove_dir_if_empty(&bin_dir);
    let gone = remove_dir_if_empty(&dir) && bin_gone;
    if gone {
        println!("Removed {}", dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn plugin_root_from_bin() {
        assert_eq!(
            plugin_root_for(Path::new("/usr/local/bin/container")).unwrap(),
            Path::new("/usr/local/libexec/container-plugins")
        );
    }

    #[test]
    fn config_is_cli_plugin() {
        let c = config_toml();
        assert!(c.contains("abstract = "));
        assert!(!c.contains("servicesConfig"));
    }

    /// Whether `path` carries the user-immutable flag (uchg).
    fn is_locked(path: &Path) -> bool {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path; stat only reads it.
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        (unsafe { libc::stat(c.as_ptr(), &mut st) } == 0) && st.st_flags & libc::UF_IMMUTABLE != 0
    }

    #[test]
    fn install_and_uninstall_in_temp_root() {
        let root = tempfile::tempdir().unwrap();
        install(Some(root.path().to_path_buf()), None).unwrap();
        let bin = root.path().join("distro/bin/distro");
        assert!(bin.symlink_metadata().unwrap().file_type().is_file());
        assert_eq!(
            fs::metadata(&bin).unwrap().permissions().mode() & 0o111,
            0o111
        );
        // Both init-assets flavors are installed, and locked so a guest
        // can't rewrite them through a shared home.
        for flavor in ["sbin.distro", "sbin.distro.restricted"] {
            let dir = root.path().join("distro").join(flavor);
            assert!(dir.join("init").is_file());
            assert!(dir.join("create-user.sh").is_file());
            assert!(is_locked(&dir) && is_locked(&dir.join("init")));
        }
        assert!(
            root.path()
                .join("distro/sbin.distro/grant-admin.sh")
                .is_file()
        );
        assert!(
            !root
                .path()
                .join("distro/sbin.distro.restricted/grant-admin.sh")
                .exists()
        );
        // Re-installing from the installed binary is a no-op, not a
        // delete-then-fail.
        install(Some(root.path().to_path_buf()), Some(&bin)).unwrap();
        assert!(bin.is_file());
        uninstall(Some(root.path().to_path_buf())).unwrap();
        assert!(!root.path().join("distro").exists());
    }

    #[test]
    fn uninstall_leaves_foreign_files() {
        let root = tempfile::tempdir().unwrap();
        install(Some(root.path().to_path_buf()), None).unwrap();
        let keep = root.path().join("distro/keep.txt");
        fs::write(&keep, b"not ours").unwrap();
        // Our files go; the foreign one and its directories stay.
        uninstall(Some(root.path().to_path_buf())).unwrap();
        assert!(!root.path().join("distro/config.toml").exists());
        assert!(!root.path().join("distro/bin/distro").exists());
        assert!(keep.exists());
    }
}
