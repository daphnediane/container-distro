/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! What we know about the upstream `container` release: the version
//! the install offer pins, the minors whose appRoot internals we have
//! verified, and the installed daemon's own version. The pin, the SHA,
//! and the verified list all bump together when a new upstream minor
//! is verified — docs/design/container-internals.md "version gating".

use std::cmp::Ordering;
use std::path::PathBuf;

/// The `container` release the interactive install offer downloads and
/// opens — pinned to a release whose appRoot internals we have
/// verified, so bump it together with `VERIFIED_CONTAINER_MINOR` (and
/// `PKG_SHA256`) once the new release's layout checks out —
/// docs/design/container-internals.md "version gating".
pub const PINNED_CONTAINER_VERSION: &str = "1.6.1";

/// SHA-256 of `container-<PINNED_CONTAINER_VERSION>-installer-signed.pkg`
/// — the `digest` GitHub reports for the release asset. Pinned so the
/// download can't be swapped en route; bump together with
/// `PINNED_CONTAINER_VERSION` (docs/design/container-internals.md).
pub(crate) const PKG_SHA256: &str =
    "cd47846819f06478d067060efe21e516b562ac0a9af8eecad6d5fea0b556b200";

/// `container` `major.minor` versions whose internal storage layout we
/// have verified: `containers/<id>/rootfs.ext4`,
/// `containers/<id>/runtime-configuration.json` and its
/// `options.rootFsOverride`. Extended as new `container` minors are
/// verified — a release-checklist step (docs/design/release.md).
pub const VERIFIED_CONTAINER_MINOR: &[&str] = &["1.5", "1.6"];

/// `container system status --format json`, or `None` when the daemon
/// isn't answering.
fn system_status() -> Option<serde_json::Value> {
    let out = super::container_cmd()
        .args(["system", "status", "--format", "json"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Whether `container` services are currently running.
pub fn system_running() -> bool {
    system_status().is_some_and(|s| s.get("status").and_then(|v| v.as_str()) == Some("running"))
}

/// `container`'s data directory (`paths.appRoot` in `container system status`).
pub fn app_root() -> Option<PathBuf> {
    system_status()?
        .pointer("/paths/appRoot")?
        .as_str()
        .map(PathBuf::from)
}

/// The `container` daemon version (`server.version` in `system status`).
/// `None` while services are down — the daemon writes it.
pub fn container_version() -> Option<String> {
    system_status()?
        .pointer("/server/version")?
        .as_str()
        .map(String::from)
}

/// The installed `container` CLI's version (`container --version`),
/// daemon or not — `None` when no binary resolves.
pub fn installed_version() -> Option<String> {
    let out = super::container_cmd().arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    cli_version(&String::from_utf8_lossy(&out.stdout))
}

/// The version in `container --version` output —
/// `container CLI version 1.6.1 (build: release, commit: …)`.
fn cli_version(text: &str) -> Option<String> {
    let v = text.split("version ").nth(1)?.split([' ', '(']).next()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// `X.Y.Z[-suffix]`: the numeric triple first, then a prerelease sorts
/// before its release (`0.6.0-dev` < `0.6.0`). Two prerelease suffixes
/// compare lexically — `dev` < `rc.N` — close enough for our tags.
pub(crate) fn cmp_version(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> ([u64; 3], Option<&str>) {
        let v = v.strip_prefix('v').unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (v, None),
        };
        let mut numbers = [0u64; 3];
        for (n, part) in core.split('.').take(3).enumerate() {
            numbers[n] = part.parse().unwrap_or(0);
        }
        (numbers, pre)
    }
    let (an, ap) = split(a);
    let (bn, bp) = split(b);
    an.cmp(&bn).then_with(|| match (ap, bp) {
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (x, y) => x.cmp(&y),
    })
}

/// The installed `container` version when it's behind the pinned
/// release — `None` when nothing is installed (the install offer owns
/// that case), or when it is current or newer than the pin.
pub fn behind_pin() -> Option<String> {
    let installed = installed_version()?;
    (cmp_version(&installed, PINNED_CONTAINER_VERSION) == Ordering::Less).then_some(installed)
}

/// A quiet one-line nudge that `container` is behind the newest
/// verified release — `update` and the version surfaces (`cm -v`,
/// `container distro version`) all show it. Never blocks; a
/// newer-than-pinned install is upstream of the verified range and
/// `warn_unverified_version` covers that on the paths that touch
/// internals.
pub fn note_behind_pin() {
    if let Some(installed) = behind_pin() {
        eprintln!(
            "note: `container` {installed} is installed; {PINNED_CONTAINER_VERSION} is the newest verified release — https://github.com/apple/container/releases/latest"
        );
    }
}

/// Warn (once per process) when the `container` daemon is not a version
/// whose appRoot internals we have verified. Code paths that read or
/// write `appRoot` contents directly should call this first — the layout
/// is not a public interface and may change without notice.
pub fn warn_unverified_version() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(v) = container_version()
            && !VERIFIED_CONTAINER_MINOR
                .iter()
                .any(|k| v == *k || v.starts_with(&format!("{k}.")))
        {
            eprintln!(
                "warning: `container` {v} has not been verified — operations using its internal storage layout may break"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_version() {
        assert_eq!(
            cli_version("container CLI version 1.6.1 (build: release, commit: aa8d76b)\n"),
            Some("1.6.1".to_string())
        );
        assert_eq!(
            cli_version("container CLI version 1.5.0\n"),
            Some("1.5.0".to_string())
        );
        assert_eq!(cli_version("garbage"), None);
    }

    #[test]
    fn version_ordering() {
        assert_eq!(cmp_version("0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(cmp_version("0.5.0", "v0.5.0"), Ordering::Equal);
        assert_eq!(cmp_version("0.5.0", "0.6.0"), Ordering::Less);
        assert_eq!(cmp_version("0.6.0-dev", "0.5.0"), Ordering::Greater);
        assert_eq!(cmp_version("0.6.0-dev", "0.6.0"), Ordering::Less);
        assert_eq!(cmp_version("0.6.0-rc.1", "0.6.0"), Ordering::Less);
        assert_eq!(cmp_version("0.6.0-dev", "0.6.0-rc.1"), Ordering::Less);
    }
}
