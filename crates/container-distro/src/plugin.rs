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

use anyhow::{Context, Result, bail};
use cm_core::naming::APP_NAME;

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

fn container_bin() -> Result<PathBuf> {
    let name = env::var_os("CONTAINER_CLI").unwrap_or_else(|| "container".into());
    let name = PathBuf::from(name);
    let found = if name.components().count() > 1 {
        Some(name)
    } else {
        env::var_os("PATH").and_then(|paths| {
            env::split_paths(&paths)
                .map(|d| d.join(&name))
                .find(|p| p.is_file())
        })
    };
    let bin = found.context("`container` not found on PATH")?;
    fs::canonicalize(&bin).with_context(|| format!("failed to resolve {}", bin.display()))
}

fn plugin_dir(root: Option<PathBuf>) -> Result<PathBuf> {
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
    fs::remove_dir_all(&dir).map_err(|e| permission_hint(e, &dir))?;
    println!("Removed {}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Re-installing from the installed binary is a no-op, not a
        // delete-then-fail.
        install(Some(root.path().to_path_buf()), Some(&bin)).unwrap();
        assert!(bin.is_file());
        uninstall(Some(root.path().to_path_buf())).unwrap();
        assert!(!root.path().join("distro").exists());
    }
}
