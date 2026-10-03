/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Thin wrappers around the `container` CLI.

use std::env;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A container machine as reported by `container machine list --format json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
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
    pub disk_size: Option<u64>,
}

impl Machine {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// Extra per-machine fields reported only by `container machine inspect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineDetail {
    pub id: String,
    #[serde(default)]
    pub container_id: Option<String>,
    #[serde(default)]
    pub home_mount: Option<String>,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default)]
    pub image: Option<ImageInfo>,
}

/// An OCI platform (`os`/`architecture`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.os, self.architecture)
    }
}

/// The image a machine was created from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageInfo {
    pub reference: String,
}

/// A container as reported by `container list --format json` / `inspect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub configuration: ContainerConfig,
    #[serde(default)]
    pub status: ContainerStatus,
}

impl ContainerInfo {
    pub fn id(&self) -> &str {
        &self.configuration.id
    }

    pub fn is_running(&self) -> bool {
        self.status.state == "running"
    }

    /// The first IPv4 address, without its prefix length.
    pub fn ipv4(&self) -> Option<&str> {
        self.status
            .networks
            .iter()
            .find_map(|n| n.ipv4_address.as_deref())
            .and_then(|a| a.split('/').next())
    }
}

/// The subset of a container's configuration that `cm` cares about.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerConfig {
    pub id: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub mounts: Vec<MountInfo>,
    #[serde(default)]
    pub published_ports: Vec<PublishedPort>,
    #[serde(default)]
    pub resources: Option<Resources>,
    #[serde(default)]
    pub image: Option<ImageInfo>,
    #[serde(default)]
    pub platform: Option<Platform>,
}

/// A filesystem mount in a container's configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountInfo {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub options: Vec<String>,
}

impl MountInfo {
    pub fn read_only(&self) -> bool {
        self.options.iter().any(|o| o == "ro")
    }
}

/// A published (host → container) port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedPort {
    #[serde(default)]
    pub host_address: Option<String>,
    pub host_port: u16,
    pub container_port: u16,
    #[serde(default)]
    pub proto: Option<String>,
}

/// CPU and memory allocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    #[serde(default)]
    pub cpus: Option<u64>,
    #[serde(default)]
    pub memory_in_bytes: Option<u64>,
}

/// Runtime status of a container.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatus {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub networks: Vec<NetworkStatus>,
}

/// One network attachment of a running container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    #[serde(default)]
    pub ipv4_address: Option<String>,
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

/// All machines known to `container`.
pub fn list_machines() -> Result<Vec<Machine>> {
    json_output(&["machine", "list", "--format", "json"])
}

/// All containers, running or not.
pub fn list_containers() -> Result<Vec<ContainerInfo>> {
    json_output(&["list", "--all", "--format", "json"])
}

/// `container machine inspect` for one machine.
pub fn inspect_machine(id: &str) -> Result<MachineDetail> {
    let details: Vec<MachineDetail> = json_output(&["machine", "inspect", id])?;
    details
        .into_iter()
        .next()
        .with_context(|| format!("machine `{id}` not found"))
}

/// Quote `arg` for a POSIX shell: wrap in single quotes, escaping embedded
/// single quotes as `'\''`.
#[must_use]
pub fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// How a command line is handed to the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgvMode {
    /// Joined and evaluated by the guest shell (WSL `--` / bare command).
    Shell,
    /// Each argument delivered verbatim (WSL `-e`).
    Exact,
}

/// Build a `container machine run` command.
///
/// `executable` selects a specific program to run interactively (used for
/// `--shell-type standard`); `command` is a command line appended after
/// `--`. With neither, `container` opens the machine's login shell.
///
/// `machine run` always passes its arguments through the guest's
/// `$SHELL -c "$*"` (apple/container#1954), so [`ArgvMode::Exact`]
/// single-quotes each argument to make that evaluation a no-op. Drop the
/// quoting if upstream grows an argv-preserving mode.
///
/// `-i` is always passed: without it the guest sees EOF on stdin, so
/// piped input would silently vanish.
pub fn run_command(
    machine: Option<&str>,
    user: Option<&str>,
    cd: Option<&str>,
    env_vars: &[String],
    executable: Option<&str>,
    command: &[String],
    mode: ArgvMode,
) -> Command {
    let mut cmd = container_cmd();
    cmd.args(["machine", "run", "-i"]);
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
        cmd.arg("--").arg(shell_quote(exe));
    } else if !command.is_empty() {
        cmd.arg("--");
        match mode {
            ArgvMode::Shell => cmd.args(command),
            ArgvMode::Exact => cmd.args(command.iter().map(|a| shell_quote(a))),
        };
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
    let mut cmd = run_command(machine, user, None, &[], None, &command, ArgvMode::Shell);
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

    const LIST_JSON: &str = r#"[{"id":"alpine","memory":34359738368,"createdDate":"2026-10-01T00:58:57Z","status":"running","diskSize":78872576,"default":true,"cpus":7,"ipAddress":"192.168.64.6"}]"#;

    const INSPECT_JSON: &str = r#"[{"containerId":"alpine-730439","cpus":7,"homeMount":"rw","id":"alpine",
        "image":{"descriptor":{"digest":"sha256:a2","mediaType":"application/vnd.oci.image.index.v1+json","size":9218},
                 "reference":"docker.io/library/alpine:latest"},
        "ipAddress":"192.168.64.6","platform":{"architecture":"arm64","os":"linux"},"status":"running",
        "userSetup":{"gid":20,"uid":1027,"username":"daphnediane"}}]"#;

    #[test]
    fn test_machine_deserialize_list() {
        let machines: Vec<Machine> = serde_json::from_str(LIST_JSON).unwrap();
        assert_eq!(machines.len(), 1);
        let m = &machines[0];
        assert!(m.is_running() && m.default);
        assert_eq!(m.cpus, Some(7));
        assert_eq!(m.disk_size, Some(78872576));
        let round: Machine = serde_json::from_str(&serde_json::to_string(m).unwrap()).unwrap();
        assert_eq!(&round, m);
    }

    #[test]
    fn test_machine_deserialize_minimal() {
        let m: Machine = serde_json::from_str(r#"{"id":"x","status":"stopped"}"#).unwrap();
        assert!(!m.is_running() && !m.default && m.cpus.is_none());
    }

    #[test]
    fn test_machine_detail_deserialize() {
        let d: Vec<MachineDetail> = serde_json::from_str(INSPECT_JSON).unwrap();
        let d = &d[0];
        assert_eq!(d.container_id.as_deref(), Some("alpine-730439"));
        assert_eq!(d.platform.as_ref().unwrap().to_string(), "linux/arm64");
        assert_eq!(
            d.image.as_ref().unwrap().reference,
            "docker.io/library/alpine:latest"
        );
        let round: MachineDetail =
            serde_json::from_str(&serde_json::to_string(d).unwrap()).unwrap();
        assert_eq!(&round, d);
    }

    const CONTAINER_JSON: &str = r#"[{"configuration":{"capAdd":["ALL"],"id":"d1",
        "labels":{"org.wsl-compat.distro":"d1"},
        "mounts":[{"destination":"/mnt/x","options":["ro"],"source":"/Volumes/X","type":{"virtiofs":{}}}],
        "publishedPorts":[{"containerPort":80,"count":1,"hostAddress":"127.0.0.1","hostPort":8080,"proto":"tcp"}],
        "resources":{"cpuOverhead":0,"cpus":4,"memoryInBytes":1073741824},
        "image":{"reference":"docker.io/library/alpine:latest"},
        "platform":{"architecture":"arm64","os":"linux"}},
        "id":"d1",
        "status":{"networks":[{"ipv4Address":"192.168.64.13/24","network":"default"}],"state":"running"}}]"#;

    #[test]
    fn test_container_info_deserialize() {
        let c: Vec<ContainerInfo> = serde_json::from_str(CONTAINER_JSON).unwrap();
        let c = &c[0];
        assert_eq!(c.id(), "d1");
        assert!(c.is_running());
        assert_eq!(c.ipv4(), Some("192.168.64.13"));
        assert!(c.configuration.mounts[0].read_only());
        assert_eq!(c.configuration.published_ports[0].host_port, 8080);
        let round: ContainerInfo =
            serde_json::from_str(&serde_json::to_string(c).unwrap()).unwrap();
        assert_eq!(&round, c);
    }

    #[test]
    fn test_container_info_stopped_minimal() {
        let c: ContainerInfo =
            serde_json::from_str(r#"{"configuration":{"id":"x"},"status":{"state":"stopped"}}"#)
                .unwrap();
        assert!(!c.is_running() && c.ipv4().is_none());
    }

    #[test]
    fn names_are_dns_style() {
        assert!(validate_name("ubuntu-24").is_ok());
        for bad in ["", "Ubuntu", "a_b", "-a", "a-", "a.b"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

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
            ArgvMode::Shell,
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "machine", "run", "-i", "-n", "dev", "-u", "root", "-w", "/tmp", "-e", "FOO=bar",
                "--", "ls", "-la"
            ]
        );
    }

    #[test]
    fn run_command_bare_shell() {
        let cmd = run_command(None, None, None, &[], None, &[], ArgvMode::Shell);
        assert_eq!(args_of(&cmd), ["machine", "run", "-i"]);
    }

    #[test]
    fn run_command_exact_quotes_each_arg() {
        let command = ["printf".to_string(), ":%s:".to_string(), "a b".to_string()];
        let cmd = run_command(None, None, None, &[], None, &command, ArgvMode::Exact);
        assert_eq!(
            args_of(&cmd),
            ["machine", "run", "-i", "--", "'printf'", "':%s:'", "'a b'"]
        );
    }

    #[test]
    fn shell_quote_escapes() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
}
