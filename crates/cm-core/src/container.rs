/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Thin wrappers around the `container` CLI.

use std::env;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// A container machine as reported by `container machine list --format json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub ip_address: Option<String>,
}

impl Machine {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// A `container` CLI invocation; `CONTAINER_CLI` overrides the binary path.
pub fn container_cmd() -> Command {
    Command::new(env::var("CONTAINER_CLI").unwrap_or_else(|_| "container".into()))
}

fn system_status() -> Option<String> {
    let out = container_cmd()
        .args(["system", "status", "--format", "json"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()?
        .get("status")?
        .as_str()
        .map(str::to_owned)
}

/// Whether `container` services are currently running.
pub fn system_running() -> bool {
    system_status().as_deref() == Some("running")
}

/// Start `container` services if they are not already running.
pub fn ensure_started() -> Result<()> {
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
    Ok(())
}

/// All machines known to `container`.
pub fn list_machines() -> Result<Vec<Machine>> {
    let out = container_cmd()
        .args(["machine", "list", "--format", "json"])
        .output()
        .context("failed to run `container machine list`")?;
    if !out.status.success() {
        bail!(
            "`container machine list` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).context("failed to parse `container machine list` output")
}

/// Build a `container machine run` command.
///
/// `executable` selects a specific program to run interactively (used for
/// `--shell-type standard`); `command` is a verbatim command line appended
/// after `--`. With neither, `container` opens the machine's login shell.
pub fn run_command(
    machine: Option<&str>,
    user: Option<&str>,
    cd: Option<&str>,
    env_vars: &[String],
    executable: Option<&str>,
    command: &[String],
) -> Command {
    let mut cmd = container_cmd();
    cmd.arg("machine").arg("run");
    if let Some(m) = machine {
        cmd.args(["-n", m]);
    }
    if let Some(u) = user {
        cmd.args(["-u", u]);
    }
    if let Some(d) = cd {
        cmd.args(["-w", d]);
    }
    for e in env_vars {
        cmd.args(["-e", e]);
    }
    if let Some(exe) = executable {
        cmd.arg("--").arg(exe);
    } else if !command.is_empty() {
        cmd.arg("--").args(command);
    }
    cmd
}

/// Resolve the login shell for the given user inside the machine.
///
/// Falls back to `/bin/sh` if the lookup fails for any reason.
pub fn resolve_shell(machine: Option<&str>, user: Option<&str>) -> String {
    let command = vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7"#.to_string(),
    ];
    let mut cmd = run_command(machine, user, None, &[], None, &command);
    let out = cmd.stdin(Stdio::null()).output();
    let shell = out.ok().and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && s.starts_with('/') {
            Some(s)
        } else {
            None
        }
    });
    shell.unwrap_or_else(|| "/bin/sh".to_string())
}

/// Derive a machine name from an image reference (`alpine:latest` → `alpine-latest`).
pub fn default_machine_name(image: &str) -> String {
    image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split('@')
        .next()
        .unwrap_or(image)
        .replace(':', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_name_from_image() {
        assert_eq!(default_machine_name("alpine:latest"), "alpine-latest");
        assert_eq!(default_machine_name("alpine"), "alpine");
        assert_eq!(
            default_machine_name("docker.io/library/ubuntu:24.04"),
            "ubuntu-24.04"
        );
    }

    #[test]
    fn run_command_builds_args() {
        let cmd = run_command(
            Some("dev"),
            Some("root"),
            Some("/tmp"),
            &["FOO=bar".to_string()],
            None,
            &["ls".to_string(), "-la".to_string()],
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "machine", "run", "-n", "dev", "-u", "root", "-w", "/tmp", "-e", "FOO=bar", "--",
                "ls", "-la"
            ]
        );
    }

    #[test]
    fn run_command_bare_shell() {
        let cmd = run_command(None, None, None, &[], None, &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["machine", "run"]);
    }
}
