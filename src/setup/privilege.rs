/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Running `install-plugin`/`uninstall-plugin` file ops with the
//! privilege they need: directly when the plugin dir is writable,
//! else via a `sudo` re-exec of this binary.

use anyhow::{Context, Result, bail};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

/// Whether this process runs with euid 0.
pub(crate) fn is_root() -> bool {
    // SAFETY: geteuid is always safe to call.
    unsafe { libc::geteuid() == 0 }
}

/// Whether `dir` — or its nearest existing ancestor, when `dir` itself
/// doesn't exist yet — is writable by this user. The test for whether
/// plugin install/uninstall needs root.
fn user_writable(dir: &Path) -> bool {
    let mut probe = Some(dir);
    while let Some(d) = probe {
        if d.exists() {
            let Ok(c) = CString::new(d.as_os_str().as_bytes()) else {
                return false;
            };
            // SAFETY: c is a NUL-terminated path we just built.
            return unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } == 0;
        }
        probe = d.parent();
    }
    false
}

/// Re-exec this binary under `sudo` for the privileged half of
/// `install-plugin`/`uninstall-plugin`: only the plugin-dir file ops
/// run as root — `container` resolution (and the install offer) always
/// ran as the invoking user first. The child is invoked with an
/// explicit `--plugin-dir`, so `sudo`'s env_reset/secure_path can't
/// change which `container` the dir is derived from. Same trust as
/// telling the user to run `sudo` themselves — just fewer steps.
pub(crate) fn sudo_re_exec(subcommand: &str, plugin_dir: &Path, extra: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let mut cmd = Command::new("/usr/bin/sudo");
    cmd.arg(exe)
        .arg(subcommand)
        .arg("--plugin-dir")
        .arg(plugin_dir)
        .args(extra);
    eprintln!("{} requires administrator rights:", plugin_dir.display());
    let status = cmd.status().context("failed to run sudo")?;
    if !status.success() {
        bail!("sudo did not complete — re-run `sudo container-distro {subcommand}` yourself");
    }
    Ok(())
}

/// Run the plugin-dir file ops directly when they're user-writable, or
/// via [`sudo_re_exec`] when they need root. `plugin_dir` is the
/// `<root>/container-plugins` value `--plugin-dir` takes.
pub(crate) fn with_plugin_privilege(
    subcommand: &str,
    plugin_dir: &Path,
    extra: &[String],
    ops: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if is_root() || user_writable(&plugin_dir.join(crate::setup::plugin::PLUGIN_NAME)) {
        return ops();
    }
    sudo_re_exec(subcommand, plugin_dir, extra)
}
