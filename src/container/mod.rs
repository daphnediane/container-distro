/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

pub(crate) mod install;
mod machine;
#[cfg(test)]
mod test_support;
mod types;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use std::env;
use std::fs::{self};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

pub use machine::{
    ArgvMode, guest_passwd_home, inspect_machine, inspect_machine_or_default, list_machines,
    machine_exec_command, machine_workdir, resolve_shell, run_command, shell_quote,
};
pub use types::{
    ContainerConfig, ContainerInfo, ContainerStatus, ImageInfo, ImageListConfig, ImageListEntry,
    ImageUserConfig, ImageVariant, ImageVariantConfig, Machine, MachineDetail, MountInfo,
    NetworkAttachment, NetworkStatus, Platform, PublishedPort, Resources, UserSetup,
};

/// All images as `container image list --format json` reports them —
/// `None` when the listing can't be fetched or parsed.
pub fn list_images() -> Option<Vec<ImageListEntry>> {
    let out = container_cmd()
        .args(["image", "list", "--format", "json"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Where apple/container's installer puts the CLI (per its docs and
/// released installers). Preferred over a bare PATH lookup so a hijacked
/// PATH can't substitute a different binary.
pub const INSTALLED_CONTAINER_PATH: &str = "/usr/local/bin/container";

/// The `container` binary to invoke: `CONTAINER_CLI`, else the installed
/// location, else whatever PATH resolves.
pub fn container_binary() -> &'static str {
    if Path::new(INSTALLED_CONTAINER_PATH).is_file() {
        INSTALLED_CONTAINER_PATH
    } else {
        "container"
    }
}

/// The `CONTAINER_CLI` override, when it may be honored. Values that
/// can't redirect anything — the bare `container` name (a PATH lookup
/// is the default anyway) or the installed path spelled exactly —
/// always apply; any other path or name needs `--dev`, so a variable
/// planted in a shell profile can't swap the binary every command
/// execs. Ignored values warn once per process, not per spawn.
fn cli_override() -> Option<std::ffi::OsString> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let v = env::var_os("CONTAINER_CLI")?;
    let p = Path::new(&v);
    if p == Path::new(INSTALLED_CONTAINER_PATH) || p == Path::new("container") || crate::dev_mode()
    {
        return Some(v);
    }
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "ignoring CONTAINER_CLI={} (pass --dev to honor it)",
            p.display()
        );
    }
    None
}

/// A `container` CLI invocation; `CONTAINER_CLI` overrides the binary
/// path — honor rules in [`cli_override`].
pub fn container_cmd() -> Command {
    Command::new(cli_override().unwrap_or_else(|| container_binary().into()))
}

/// The `container` release the interactive install offer downloads and
/// opens — pinned to a release whose appRoot internals we have
/// verified, so bump it together with `VERIFIED_CONTAINER_MINOR` (and
/// `PKG_SHA256` in install.rs) once the new release's layout checks
/// out — docs/design/container-internals.md "version gating".
pub const PINNED_CONTAINER_VERSION: &str = "1.5.0";

/// `name` looked up on `PATH` — the first match that is a file.
pub(crate) fn find_on_path(name: &Path) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// The resolved `container` binary: a `--dev`-gated `CONTAINER_CLI`
/// override (a path, or a name found on PATH), then the installed
/// location, then PATH. `None` when no `container` can be found —
/// [`require_installed`] turns that into an install offer or an error.
pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(name) = cli_override().map(PathBuf::from) {
        return if name.components().count() > 1 {
            name.is_file().then_some(name)
        } else {
            find_on_path(&name)
        };
    }
    let installed = Path::new(INSTALLED_CONTAINER_PATH);
    if installed.is_file() {
        Some(installed.to_path_buf())
    } else {
        find_on_path(Path::new("container"))
    }
}

/// Fail early when `container` can't be resolved: interactive sessions
/// on a supported platform get an offer to download and open the
/// signed installer; everything else gets the manual instructions. A
/// `CONTAINER_CLI` that doesn't resolve is reported, not offered over —
/// the fix is to unset it, not to install another copy.
pub fn require_installed() -> Result<()> {
    if resolve_binary().is_some() {
        return Ok(());
    }
    if cli_override().is_some() {
        bail!("`container` not found via $CONTAINER_CLI or on PATH");
    }
    if !install::offer()? {
        bail!("{}", install::not_installed_message());
    }
    Ok(())
}

fn system_status() -> Option<serde_json::Value> {
    let out = container_cmd()
        .args(["system", "status", "--format", "json"])
        .stderr(Stdio::null())
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
pub fn container_version() -> Option<String> {
    system_status()?
        .pointer("/server/version")?
        .as_str()
        .map(String::from)
}

/// `container` `major.minor` versions whose internal storage layout we
/// have verified: `containers/<id>/rootfs.ext4`,
/// `containers/<id>/runtime-configuration.json` and its
/// `options.rootFsOverride`. Extended as new `container` minors are
/// verified — a release-checklist step (docs/design/release.md).
pub const VERIFIED_CONTAINER_MINOR: &[&str] = &["1.5"];

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

/// Host disk space a container's root filesystem uses: the allocated size
/// of its sparse `rootfs.ext4`, which is what `container machine list`
/// reports as DISK.
pub fn container_disk_usage(app_root: &Path, id: &str) -> Option<u64> {
    let meta = std::fs::metadata(app_root.join("containers").join(id).join("rootfs.ext4")).ok()?;
    Some(meta.blocks() * 512)
}

/// The marker `ensure_started` leaves when it — not an earlier
/// `container system start` by the user or another tool — brought the
/// services up. `cm --shutdown` uses it for its "stop what we
/// started" rule: only stop services this project's tooling started
/// (#11).
fn services_marker() -> Result<PathBuf> {
    Ok(crate::naming::state_dir()?.join("services-started"))
}

/// Whether our tooling started the currently running services — the
/// marker `ensure_started` wrote when it ran `system start`.
#[must_use]
pub fn services_started_by_us() -> bool {
    services_marker().is_ok_and(|p| p.is_file())
}

/// Forget that we started the services: after `system stop`, or when
/// services are already down and the marker is stale.
pub fn clear_services_marker() {
    if let Ok(p) = services_marker() {
        let _ = fs::remove_file(p);
    }
}

/// Start `container` services if they are not already running.
pub fn ensure_started() -> Result<()> {
    require_installed()?;
    if system_running() {
        return Ok(());
    }
    let status = container_cmd()
        .args(["system", "start"])
        .status()
        .context("failed to run `container system start`")?;
    if !status.success() {
        bail!("`container system start` exited with {status}");
    }
    // Record that this start was ours, so a later `cm --shutdown` can
    // stop services it finds idle (best-effort bookkeeping — a write
    // failure just means --shutdown stays conservative).
    if let Ok(marker) = services_marker()
        && let Some(parent) = marker.parent()
    {
        let _ = fs::create_dir_all(parent);
        let by = env::args()
            .next()
            .and_then(|a| {
                PathBuf::from(a)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "?".into());
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = fs::write(
            &marker,
            format!(
                "{{\"by\":{by:?},\"pid\":{},\"at\":{at}}}\n",
                std::process::id()
            ),
        );
    }
    Ok(())
}

/// Run `container <args>` and parse its stdout as JSON.
pub fn json_output<T: DeserializeOwned>(args: &[&str]) -> Result<T> {
    let what = format!("container {}", args.join(" "));
    let out = container_cmd()
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{what}`"))?;
    if !out.status.success() {
        bail!(
            "`{what}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).with_context(|| format!("failed to parse `{what}` output"))
}

/// All containers, running or not.
pub fn list_containers() -> Result<Vec<ContainerInfo>> {
    json_output(&["list", "--all", "--format", "json"])
}

/// Check a machine/distro name: lowercase DNS-style (`[a-z0-9-]`, not
/// starting or ending with `-`), as `container` requires.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !ok {
        bail!("invalid name `{name}`: use lowercase letters, digits, and `-` (e.g. `ubuntu-24`)");
    }
    Ok(())
}

/// Check a `--user` value: a username or `uid[:gid]` — nothing that a
/// `container` flag position could mistake for an option.
pub fn validate_user(user: &str) -> Result<()> {
    let ok = !user.is_empty()
        && !user.starts_with('-')
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'));
    if !ok {
        bail!("invalid user `{user}`: expected a username or uid[:gid]");
    }
    Ok(())
}

/// Expand an image reference for comparison: implied `docker.io` (and
/// `library/` for a bare name) plus the `:latest` tag, so `alpine`,
/// `docker.io/alpine`, and `docker.io/library/alpine:latest` all
/// compare equal. `container` reports references in this expanded form.
fn normalize_image_ref(r: &str) -> String {
    let (name, digest) = match r.split_once('@') {
        Some((n, d)) => (n, format!("@{d}")),
        None => (r, String::new()),
    };
    let mut name = name.to_string();
    // A first component is a registry only when followed by `/` and it
    // looks like a host (`.`/`:`/`localhost`) — `name:tag` alone is a
    // bare name, not `registry:port`. Otherwise docker.io is implied,
    // and `library/` for a bare name.
    let first = name.split('/').next().unwrap_or_default();
    let has_registry =
        name.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost");
    if !has_registry {
        name = format!("docker.io/{name}");
    }
    if let Some(rest) = name.strip_prefix("docker.io/")
        && !rest.contains('/')
    {
        name = format!("docker.io/library/{rest}");
    }
    if !name.rsplit('/').next().is_some_and(|l| l.contains(':')) {
        name.push_str(":latest");
    }
    format!("{name}{digest}")
}

/// Whether two image references name the same image once registry and
/// tag defaults are applied.
#[must_use]
pub fn same_image(a: &str, b: &str) -> bool {
    normalize_image_ref(a) == normalize_image_ref(b)
}

/// Derive a machine name from an image reference (`alpine:latest` →
/// `alpine-latest`); non-`[a-z0-9]` characters become `-` so the result
/// always passes [`validate_name`].
pub fn default_machine_name(image: &str) -> String {
    image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split('@')
        .next()
        .unwrap_or(image)
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_dns_style() {
        assert!(validate_name("ubuntu-24").is_ok());
        for bad in ["", "Ubuntu", "a_b", "-a", "a-", "a.b"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn users_are_flag_safe() {
        for ok in ["root", "daphne", "501", "501:20", "a.b-c_d"] {
            assert!(validate_user(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-u", "--root", "a b", "x;y", "a/b", "x=y"] {
            assert!(validate_user(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn image_refs_normalize() {
        assert!(same_image("alpine", "docker.io/library/alpine:latest"));
        assert!(same_image(
            "docker.io/alpine:3",
            "docker.io/library/alpine:3"
        ));
        assert!(same_image("quay.io/x/y:1", "quay.io/x/y:1"));
        assert!(same_image("localhost:5000/x", "localhost:5000/x:latest"));
        assert!(same_image("a@sha256:ff", "a:latest@sha256:ff"));
        assert!(!same_image("alpine:3", "alpine:latest"));
        assert!(!same_image("alpine", "quay.io/library/alpine:latest"));
    }

    #[test]
    fn machine_name_from_image() {
        assert_eq!(default_machine_name("alpine:latest"), "alpine-latest");
        assert_eq!(default_machine_name("alpine"), "alpine");
        assert_eq!(
            default_machine_name("docker.io/library/ubuntu:24.04"),
            "ubuntu-24-04"
        );
        for bad in ["UBUNTU:24.04", "img@sha256:abc", "-x-", "a_b"] {
            let n = default_machine_name(bad);
            assert!(validate_name(&n).is_ok(), "{bad} → {n}");
        }
    }
}
