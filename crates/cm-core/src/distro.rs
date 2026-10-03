/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The contract between `cm` and the `container distro` plugin.
//!
//! `cm` talks to distros only through the plugin's CLI — the plugin is
//! optional and separately installed — so the JSON shape of
//! `container distro list --format json` lives here, shared by both sides.

use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::container::container_cmd;

/// Label marking a container as a distro; the value is the distro name.
pub const LABEL_DISTRO: &str = "org.wsl-compat.distro";
/// Label recording the home-directory mount mode (`rw`/`ro`/`none`).
pub const LABEL_HOME_MOUNT: &str = "org.wsl-compat.home-mount";

/// One distro as reported by `container distro list --format json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistroSummary {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(default)]
    pub cpus: Option<u64>,
    #[serde(default)]
    pub memory: Option<u64>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub home_mount: Option<String>,
    /// User mounts as `SRC:DST[:ro]`.
    #[serde(default)]
    pub mounts: Vec<String>,
    /// Published ports as `[IP:]HOST:GUEST[/PROTO]`.
    #[serde(default)]
    pub ports: Vec<String>,
}

impl DistroSummary {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// A `container distro` invocation.
///
/// `CM_DISTRO_CLI` points at a `container-distro` binary directly, which
/// is handy before the plugin is installed (or for testing).
pub fn distro_cmd() -> Command {
    match env::var_os("CM_DISTRO_CLI") {
        Some(path) => Command::new(path),
        None => {
            let mut cmd = container_cmd();
            cmd.arg("distro");
            cmd
        }
    }
}

/// Whether the `container distro` plugin can be used.
///
/// `CM_BACKEND=machine` turns distro support off.
pub fn distro_available() -> bool {
    if env::var("CM_BACKEND").is_ok_and(|b| b == "machine") {
        return false;
    }
    distro_cmd()
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// All distros, via `container distro list --format json`.
pub fn list_distros() -> Result<Vec<DistroSummary>> {
    let out = distro_cmd()
        .args(["list", "--format", "json"])
        .output()
        .context("failed to run `container distro list`")?;
    if !out.status.success() {
        anyhow::bail!(
            "`container distro list` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).context("failed to parse `container distro list` output")
}

/// Per-user state directory for wsl-compat
/// (`~/Library/Application Support/wsl-compat`).
pub fn state_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/wsl-compat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distro_summary_round_trip() {
        let d = DistroSummary {
            id: "d1".into(),
            status: "running".into(),
            default: true,
            ip_address: Some("192.168.64.13".into()),
            mounts: vec!["/Volumes/X:/mnt/x:ro".into()],
            ports: vec!["127.0.0.1:8080:80/tcp".into()],
            ..DistroSummary::default()
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"ipAddress\""));
        let back: DistroSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn test_distro_summary_minimal() {
        let d: DistroSummary = serde_json::from_str(r#"{"id":"x","status":"stopped"}"#).unwrap();
        assert!(!d.is_running() && d.mounts.is_empty());
    }
}
