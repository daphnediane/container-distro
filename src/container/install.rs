/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

use crate::catalog::wsl;
use crate::container::upstream::PKG_SHA256;
use crate::container::{INSTALLED_CONTAINER_PATH, PINNED_CONTAINER_VERSION};
use crate::setup::privilege::is_root;
use anyhow::{Context, Result, anyhow, bail};
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

/// Set to skip the install offer entirely and get the manual
/// instructions instead.
const NO_OFFER_ENV: &str = "CONTAINER_DISTRO_NO_INSTALL_OFFER";

/// How long we wait for Installer.app to drop the CLI before telling
/// the user to re-run — the installer UI moves at human speed.
const INSTALL_WAIT: Duration = Duration::from_secs(600);

const INSTALL_POLL: Duration = Duration::from_secs(3);

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
pub(crate) fn prompt_yes(question: &str) -> Result<bool> {
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

pub(crate) fn sha256_of(path: &Path) -> Result<String> {
    let f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    Ok(crate::util::oci::hash_reader(f)?.0)
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
/// and louder). Returns the signing team — the `(<TEAMID>)` on the
/// `Developer ID Installer:` leaf line — so self-update can compare it
/// against the installed copy's.
pub(crate) fn check_signature(pkg: &Path) -> Result<Option<String>> {
    let out = Command::new("/usr/sbin/pkgutil")
        .arg("--check-signature")
        .arg(pkg)
        .output()
        .context("failed to run `pkgutil --check-signature`")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !text.contains("Status: signed") {
        bail!("{} failed its signature check:\n{text}", pkg.display());
    }
    let line = text.lines().find(|l| l.contains("Developer ID Installer:"));
    if let Some(line) = line {
        eprintln!("  signed by {}", line.trim());
    }
    Ok(line.and_then(|l| {
        l.rsplit('(')
            .next()
            .and_then(|s| s.strip_suffix(')'))
            .map(str::to_string)
    }))
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
