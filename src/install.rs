/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Installing Apple's `container` itself, and running privileged
//! plugin file ops.
//!
//! `container` is a hard prerequisite — nothing works without it — so
//! when it can't be resolved we offer (interactive terminals only,
//! never as root) to download the signed installer for the release
//! this build is verified against and `open` it. Installer.app does
//! its own signature/notarization checks and collects the admin
//! password; we never run `sudo installer` ourselves. The download is
//! verified against a SHA-256 pinned in this binary and sanity-checked
//! with `pkgutil --check-signature` — see security.md T8.
//!
//! [`sudo_re_exec`] is the other half: `install-plugin`/`uninstall-plugin`
//! re-exec themselves under `sudo` when the plugin dir isn't
//! user-writable, so the `container` resolution and the offer above
//! always run as the invoking user.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tempfile::NamedTempFile;

use crate::container::{INSTALLED_CONTAINER_PATH, PINNED_CONTAINER_VERSION};
use crate::wsl;

/// Set to skip the install offer entirely and get the manual
/// instructions instead.
const NO_OFFER_ENV: &str = "CONTAINER_DISTRO_NO_INSTALL_OFFER";

/// SHA-256 of `container-<PINNED_CONTAINER_VERSION>-installer-signed.pkg`
/// — the `digest` GitHub reports for the release asset. Pinned so the
/// download can't be swapped en route; bump together with
/// `PINNED_CONTAINER_VERSION` (docs/design/container-internals.md).
const PKG_SHA256: &str = "a24808cb202318fa1c3bbee0c6c6887fe1225fe899d7b687a0ddd939bd6573f8";

/// How long we wait for Installer.app to drop the CLI before telling
/// the user to re-run — the installer UI moves at human speed.
const INSTALL_WAIT: Duration = Duration::from_secs(600);
const INSTALL_POLL: Duration = Duration::from_secs(3);

/// Whether this process runs with euid 0.
pub(crate) fn is_root() -> bool {
    // SAFETY: geteuid is always safe to call.
    unsafe { libc::geteuid() == 0 }
}

fn asset_name() -> String {
    format!("container-{PINNED_CONTAINER_VERSION}-installer-signed.pkg")
}

fn asset_url() -> String {
    format!(
        "https://github.com/apple/container/releases/download/{PINNED_CONTAINER_VERSION}/{}",
        asset_name()
    )
}

/// The manual-install instructions — the error when the offer can't
/// run or was declined.
pub(crate) fn not_installed_message() -> String {
    format!(
        "`container` is not installed.\n  \
        install it from https://github.com/apple/container/releases/tag/{PINNED_CONTAINER_VERSION} \
        (the signed .pkg), then run `container system start`"
    )
}

/// Offer to download and open the signed `container` installer.
/// Returns `true` when `container` resolves afterwards; `false` means
/// no offer was made or the user declined — the caller reports
/// [`not_installed_message`]. Errors carry their own context (a failed
/// download, a hash mismatch, a refused `open`, an installer that
/// outlived the wait).
///
/// The offer never fires without a terminal on stdin+stderr, as root
/// (installer's privilege belongs to Installer.app, not us), or with
/// `CONTAINER_DISTRO_NO_INSTALL_OFFER` set.
pub(crate) fn offer() -> Result<bool> {
    if std::env::var_os(NO_OFFER_ENV).is_some()
        || is_root()
        || !io::stdin().is_terminal()
        || !io::stderr().is_terminal()
    {
        return Ok(false);
    }
    if !platform_supported() {
        eprintln!("`container` requires macOS 26 on Apple silicon");
        return Ok(false);
    }
    eprintln!("`container` is not installed.");
    warn_newer_release();
    if !prompt_yes(&format!(
        "Download and open the signed container {PINNED_CONTAINER_VERSION} installer? [y/N] "
    ))? {
        return Ok(false);
    }
    let pkg = installer_pkg()?;
    check_signature(&pkg)?;
    if !Command::new("/usr/bin/open")
        .arg(&pkg)
        .status()
        .context("failed to run `open`")?
        .success()
    {
        bail!("`open` could not open {}", pkg.display());
    }
    eprintln!(
        "opened {} in Installer.app — macOS will ask for an admin password",
        pkg.display()
    );
    wait_for_binary()
}

/// A y/N question on stderr, answered on stdin.
fn prompt_yes(question: &str) -> Result<bool> {
    eprint!("{question}");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("cannot read the reply")?;
    let a = line.trim();
    Ok(a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes"))
}

/// `container` needs Apple silicon and macOS 26 — checked via
/// `sysctlbyname`, no `uname`/`sw_vers` subprocess (C2).
fn platform_supported() -> bool {
    sysctl_string(c"hw.machine").is_some_and(|m| m == "arm64")
        && sysctl_string(c"kern.osproductversion")
            .and_then(|v| v.split('.').next()?.parse::<u32>().ok())
            .is_some_and(|major| major >= 26)
}

/// A string-valued `sysctlbyname`.
fn sysctl_string(name: &std::ffi::CStr) -> Option<String> {
    let mut len = 0usize;
    // SAFETY: null data pointer queries the required size.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: buf is len bytes, the size sysctlbyname reported.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len.saturating_sub(1)); // trailing NUL
    String::from_utf8(buf).ok()
}

/// Best-effort heads-up when `container` has moved past our pin —
/// informational only; nothing from the response runs. Silent on any
/// failure (offline, API change, rate limit).
fn warn_newer_release() {
    let Some(latest) = latest_release_tag() else {
        return;
    };
    if latest != PINNED_CONTAINER_VERSION {
        eprintln!(
            "note: container {latest} is the latest release; this container-distro installs and \
            is verified against {PINNED_CONTAINER_VERSION} — check for an updated container-distro"
        );
    }
}

fn latest_release_tag() -> Option<String> {
    let mut resp = wsl::probe_agent()
        .get("https://api.github.com/repos/apple/container/releases/latest")
        .call()
        .ok()?;
    let mut text = String::new();
    resp.body_mut()
        .as_reader()
        .take(1 << 20)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("tag_name")?
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
}

/// The installer pkg, verified against the pinned SHA-256. It lives in
/// the per-user Darwin cache (`…/C/container-distro/`) so it survives
/// the `open` handoff — Installer.app reads it after we return. A
/// cached copy is re-verified before reuse, same rule as the `.wsl`
/// cache: the hash is the trust root.
fn installer_pkg() -> Result<PathBuf> {
    let path = wsl::app_cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(asset_name());
    if path.is_file() && sha256_of(&path).is_ok_and(|h| h == PKG_SHA256) {
        eprintln!("{}: already downloaded", asset_url());
        return Ok(path);
    }
    let parent = path
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf);
    let mut tmp = NamedTempFile::new_in(&parent).context("cannot create a temp file")?;
    wsl::download_to(&asset_url(), tmp.as_file_mut())?;
    verify_sha256(tmp.path())?;
    tmp.persist(&path)
        .map_err(|e| anyhow!(e.error))
        .with_context(|| format!("cannot move the download to {}", path.display()))?;
    Ok(path)
}

fn sha256_of(path: &Path) -> Result<String> {
    let f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    Ok(crate::oci::hash_reader(f)?.0)
}

fn verify_sha256(path: &Path) -> Result<()> {
    eprintln!("  verifying SHA-256 …");
    let got = sha256_of(path)?;
    if got != PKG_SHA256 {
        bail!(
            "SHA-256 mismatch for {}: expected {PKG_SHA256}, got {got}",
            asset_url()
        );
    }
    Ok(())
}

/// `pkgutil` sanity on top of the pinned hash: prove it's an
/// Apple-signed installer before handing it to Installer.app (which
/// re-runs its own notarization check anyway — this just fails earlier
/// and louder).
fn check_signature(pkg: &Path) -> Result<()> {
    let out = Command::new("/usr/sbin/pkgutil")
        .arg("--check-signature")
        .arg(pkg)
        .output()
        .context("failed to run `pkgutil --check-signature`")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !text.contains("Status: signed") {
        bail!("{} failed its signature check:\n{text}", pkg.display());
    }
    if let Some(line) = text.lines().find(|l| l.contains("Developer ID Installer:")) {
        eprintln!("  signed by {}", line.trim());
    }
    Ok(())
}

/// Poll for the installed CLI; the installer moves at human speed, so
/// give up after `INSTALL_WAIT` and tell the user to re-run.
fn wait_for_binary() -> Result<bool> {
    eprintln!("waiting for {INSTALLED_CONTAINER_PATH} (Ctrl-C to stop) …");
    let deadline = Instant::now() + INSTALL_WAIT;
    while Instant::now() < deadline {
        if crate::container::resolve_binary().is_some() {
            eprintln!("`container` installed");
            return Ok(true);
        }
        std::thread::sleep(INSTALL_POLL);
    }
    bail!("the installer did not finish in time — re-run your command when it completes")
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
    if is_root() || user_writable(&plugin_dir.join(crate::plugin::PLUGIN_NAME)) {
        return ops();
    }
    sudo_re_exec(subcommand, plugin_dir, extra)
}
