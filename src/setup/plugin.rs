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

use crate::distro;

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
    let bin = crate::container::resolve_binary().context(
        "`container` not found — cannot derive the plugin directory (pass --plugin-dir)",
    )?;
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

/// Whether `a` and `b` are files with identical bytes — size first,
/// then SHA-256. The common no-op `install-plugin` is a cargo-installed
/// binary whose bytes equal the plugin copy at a *different* path,
/// which `same_file` can't see.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return false;
    }
    if same_file(a, b) {
        return true;
    }
    let (Ok(fa), Ok(fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    matches!(
        (crate::util::oci::hash_reader(fa), crate::util::oci::hash_reader(fb)),
        (Ok((ha, _)), Ok((hb, _))) if ha == hb
    )
}

/// Whether the installed plugin under `dir` already matches what we'd
/// install — identical binary (with an exec bit) and identical config —
/// so `install-plugin` can answer "up to date" without elevating. All
/// reads; anything unreadable counts as not-current (elevate and let
/// the privileged half decide).
fn plugin_current(dir: &Path, exe: &Path) -> bool {
    let installed = dir.join("bin").join(PLUGIN_NAME);
    let Ok(cfg) = fs::read_to_string(dir.join("config.toml")) else {
        return false;
    };
    cfg == config_toml()
        && same_bytes(&installed, exe)
        && fs::metadata(&installed).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// Install the plugin, elevating only for the root-owned file ops.
///
/// `container` is resolved — and the interactive install offer run —
/// as the invoking user, before any sudo step: under `sudo` the offer
/// couldn't fire at all, and env_reset/secure_path would resolve a
/// different `container` (and strip `CONTAINER_CLI`). When the plugin
/// dir isn't user-writable the file ops are re-exec'd as
/// `sudo <self> install-plugin --plugin-dir <root>` — the same trust
/// as the `sudo` invocation this replaces. An installed plugin that
/// already matches (binary SHA-256 + config + exec bit) short-circuits
/// before any of that — no sudo prompt for a no-op reinstall.
pub fn install(root: Option<PathBuf>, from: Option<&Path>) -> Result<()> {
    let dir = match &root {
        Some(_) => plugin_dir(root.clone())?,
        None => {
            crate::container::require_installed()?;
            plugin_dir(None)?
        }
    };
    // Resolve the binary up front: `plugin_current` compares its bytes,
    // and the elevated child gets it as an explicit --from so sudo's
    // env_reset can't move which file is installed.
    let exe = match from {
        Some(f) => {
            fs::canonicalize(f).with_context(|| format!("failed to resolve {}", f.display()))?
        }
        None => fs::canonicalize(env::current_exe()?)?,
    };
    // Identical content already installed? Answer without elevating —
    // the common no-op is a re-run after `cargo install`.
    if plugin_current(&dir, &exe) {
        println!("`container {PLUGIN_NAME}` is already installed and up to date");
        println!("  to update, run install-plugin from the new binary or pass --from");
        return Ok(());
    }
    let plugin_root = dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf);
    let extra = vec!["--from".to_string(), exe.to_string_lossy().into_owned()];
    crate::setup::privilege::with_plugin_privilege("install-plugin", &plugin_root, &extra, || {
        install_into(&dir, &exe)
    })
}

fn install_into(dir: &Path, exe: &Path) -> Result<()> {
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| permission_hint(e, &bin_dir))?;
    let config = dir.join("config.toml");
    fs::write(&config, config_toml()).map_err(|e| permission_hint(e, &config))?;
    // Init assets are no longer installed next to the binary: each
    // distro keeps its own `sbin.distro` inside its journal dir, which
    // `uchg` protects just as well. Older installs may still carry the
    // plugin-dir copies — uninstall removes them; a stale set only
    // costs a boot-time warning until `set` recreates the distro.
    // Copy, not symlink: the plugin dir is typically root-owned while the
    // build output is user-writable — a link would let whoever owns the
    // binary replace what other users exec via `container distro`. A
    // symlink — even one pointing at this binary — is replaced with a
    // copy.
    let bin = bin_dir.join(PLUGIN_NAME);
    if bin.symlink_metadata().is_ok() {
        fs::remove_file(&bin).map_err(|e| permission_hint(e, &bin))?;
    }
    // fs::copy is fclonefileat/fcopyfile on macOS — an APFS clone (CoW)
    // where the filesystem supports it.
    fs::copy(exe, &bin).map_err(|e| permission_hint(e, &bin))?;
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

/// Remove the plugin registration, elevating via `sudo` re-exec when
/// the plugin dir isn't user-writable — same model as [`install`].
pub fn uninstall(root: Option<PathBuf>) -> Result<()> {
    let dir = plugin_dir(root)?;
    // Nothing to remove? Answer without elevating. `metadata` (not
    // read) so an unreadable-by-us file still counts as present — the
    // privileged half sorts out what it actually is.
    match fs::metadata(dir.join("config.toml")) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            println!("Not installed ({})", dir.display());
            return Ok(());
        }
        _ => {}
    }
    let plugin_root = dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf);
    crate::setup::privilege::with_plugin_privilege("uninstall-plugin", &plugin_root, &[], || {
        uninstall_from(&dir)
    })
}

fn uninstall_from(dir: &Path) -> Result<()> {
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
    // are `uchg`-locked, so unlock before removing. The `sbin.distro*`
    // dirs only exist on installs from before assets moved per-distro.
    let bin_dir = dir.join("bin");
    remove_file(&bin_dir.join(PLUGIN_NAME));
    // Installs made while `container distro fs` was on main also hold
    // the distrofs client beside the plugin binary.
    remove_file(&bin_dir.join("distrofs"));
    remove_file(&config);
    for admin in [true, false] {
        let assets = dir.join(distro::assets_flavor(admin));
        distro::unlock(&assets);
        for name in ["init", "create-user.sh", "grant-admin.sh"] {
            let p = assets.join(name);
            distro::unlock(&p);
            remove_file(&p);
        }
        remove_dir_if_empty(&assets);
    }
    // The same installs kept the guest file server under the plugin
    // prefix's libexec — remove both artifact names so the retired
    // feature leaves nothing behind.
    let libexec_app = crate::setup::layout::libexec_dir(dir);
    remove_file(&libexec_app.join("distrofs-server-aarch64"));
    remove_file(&libexec_app.join("distrofs"));
    remove_dir_if_empty(&libexec_app);
    if let Some(libexec) = libexec_app.parent() {
        remove_dir_if_empty(libexec);
    }
    let bin_gone = remove_dir_if_empty(&bin_dir);
    let gone = remove_dir_if_empty(dir) && bin_gone;
    if gone {
        println!("Removed {}", dir.display());
    }
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
        // Init assets are per-distro now — nothing but bin/ and
        // config.toml lands in the plugin dir.
        assert!(!root.path().join("distro/sbin.distro").exists());
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
