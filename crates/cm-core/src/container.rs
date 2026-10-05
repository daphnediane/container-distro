/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Thin wrappers around the `container` CLI.

use std::env;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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
    pub status: Option<String>,
    #[serde(default)]
    pub container_id: Option<String>,
    #[serde(default)]
    pub user_setup: Option<UserSetup>,
    #[serde(default)]
    pub home_mount: Option<String>,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default)]
    pub image: Option<ImageInfo>,
    #[serde(default)]
    pub cpus: Option<u64>,
    /// Bytes.
    #[serde(default)]
    pub memory: Option<u64>,
}

impl MachineDetail {
    pub fn is_running(&self) -> bool {
        self.status.as_deref() == Some("running")
    }
}

/// The host account a machine provisioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSetup {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
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
    /// Network attachments; empty when created with `--network none`.
    #[serde(default)]
    pub networks: Vec<NetworkAttachment>,
    /// Whether the SSH agent socket is forwarded in (`--ssh` at create).
    #[serde(default)]
    pub ssh: bool,
    #[serde(default)]
    pub resources: Option<Resources>,
    #[serde(default)]
    pub image: Option<ImageInfo>,
    #[serde(default)]
    pub platform: Option<Platform>,
    /// ISO 8601 UTC, e.g. `2026-10-03T23:35:14Z`.
    #[serde(default)]
    pub creation_date: Option<String>,
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

/// A network attachment in a container's configuration. Additional
/// per-attachment options (hostname, mtu, …) are not modeled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkAttachment {
    pub network: String,
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

/// A `container` CLI invocation; `CONTAINER_CLI` overrides the binary path.
pub fn container_cmd() -> Command {
    Command::new(env::var("CONTAINER_CLI").unwrap_or_else(|_| container_binary().into()))
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
/// `options.rootFsOverride`.
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
    inspect_machine_or_default(Some(id))
}

/// `container machine inspect` for `id`, or the default machine.
pub fn inspect_machine_or_default(id: Option<&str>) -> Result<MachineDetail> {
    let mut args = vec!["machine", "inspect"];
    args.extend(id);
    let details: Vec<MachineDetail> = json_output(&args)?;
    details
        .into_iter()
        .next()
        .with_context(|| format!("machine `{}` not found", id.unwrap_or("(default)")))
}

/// Starting directory for a machine session, matching `machine run`: the
/// host cwd when it is under a shared `$HOME`, else the guest home.
///
/// Never returns a path that may not exist in the guest — `container
/// exec -w` silently creates missing directories.
pub fn machine_workdir(
    detail: &MachineDetail,
    cwd: Option<&Path>,
    host_home: Option<&str>,
) -> Option<String> {
    let home_shared = detail.home_mount.as_deref() != Some("none");
    if let (true, Some(cwd), Some(home)) = (home_shared, cwd, host_home)
        && cwd.starts_with(home)
    {
        return Some(cwd.to_string_lossy().into_owned());
    }
    detail
        .user_setup
        .as_ref()
        .map(|u| format!("/home/{}", u.username))
}

/// Build `container exec` for an argv-exact command in a machine's
/// backing container — bypassing `machine run`'s `$SHELL -c "$*"`
/// re-evaluation (apple/container#1954) without any quoting.
///
/// Returns `None` when the inspect data lacks what's needed (no backing
/// container ID, or no provisioned user when `user` is unset); callers
/// fall back to [`run_command`] with [`ArgvMode::Exact`]. The machine
/// must already be running.
#[allow(clippy::too_many_arguments)]
pub fn machine_exec_command(
    detail: &MachineDetail,
    user: Option<&str>,
    cd: Option<&str>,
    env_vars: &[String],
    command: &[String],
    tty: bool,
    cwd: Option<&Path>,
    host_home: Option<&str>,
) -> Option<Command> {
    let cid = detail.container_id.as_deref()?;
    let mut cmd = container_cmd();
    cmd.args(["exec", "-i"]);
    if tty {
        cmd.arg("-t");
    }
    match user {
        Some(u) => {
            cmd.args(["--user", u]);
        }
        None => {
            let u = detail.user_setup.as_ref()?;
            cmd.arg("--user").arg(format!("{}:{}", u.uid, u.gid));
            for (k, v) in [
                ("HOME", format!("/home/{}", u.username)),
                ("USER", u.username.clone()),
                ("LOGNAME", u.username.clone()),
            ] {
                cmd.arg("--env").arg(format!("{k}={v}"));
            }
        }
    }
    let workdir = match cd {
        Some(d) => Some(d.to_string()),
        None => machine_workdir(detail, cwd, host_home),
    };
    if let Some(w) = workdir {
        cmd.args(["--workdir", &w]);
    }
    if let Ok(term) = env::var("TERM") {
        cmd.arg("--env").arg(format!("TERM={term}"));
    }
    for e in env_vars {
        cmd.args(["--env", e]);
    }
    cmd.arg(cid).args(command);
    Some(cmd)
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

/// The shell probe run inside the machine to find the login shell.
const SHELL_PROBE: &str = r#"getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7"#;

/// Build the `machine run` command that evaluates [`SHELL_PROBE`].
///
/// `machine run` shell-evaluates its arguments joined into `"$*"`
/// (apple/container#1954), so the probe is one pre-joined string —
/// passing argv-style `["sh", "-c", "..."]` would be re-split and `-c`
/// would swallow only the first word.
fn shell_probe_command(machine: Option<&str>, user: Option<&str>) -> Command {
    let mut cmd = run_command(
        machine,
        user,
        None,
        &[],
        None,
        &[SHELL_PROBE.to_string()],
        ArgvMode::Shell,
    );
    cmd.stdin(Stdio::null());
    cmd
}

/// Resolve the login shell for the given user inside the machine.
///
/// Falls back to `/bin/sh` if the lookup fails for any reason.
pub fn resolve_shell(machine: Option<&str>, user: Option<&str>) -> String {
    let shell = shell_probe_command(machine, user)
        .output()
        .ok()
        .and_then(|o| {
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
        "labels":{"io.github.daphnediane.container-distro.distro":"d1"},
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

    fn detail() -> MachineDetail {
        let d: Vec<MachineDetail> = serde_json::from_str(INSPECT_JSON).unwrap();
        d.into_iter().next().unwrap()
    }

    #[test]
    fn machine_exec_is_argv_exact() {
        let d = detail();
        let command = ["printf".to_string(), ":%s:".to_string(), "a b".to_string()];
        let cmd = machine_exec_command(
            &d,
            None,
            None,
            &["FOO=bar".to_string()],
            &command,
            false,
            Some(Path::new("/tmp")),
            Some("/Users/daphnediane"),
        )
        .unwrap();
        let args = args_of(&cmd);
        let joined = args.join(" ");
        assert!(joined.starts_with("exec -i --user 1027:20 --env HOME=/home/daphnediane"));
        assert!(joined.contains("--workdir /home/daphnediane"));
        assert!(joined.contains("--env FOO=bar"));
        assert_eq!(
            &args[args.len() - 4..],
            ["alpine-730439", "printf", ":%s:", "a b"]
        );
    }

    #[test]
    fn machine_exec_needs_container_id() {
        let mut d = detail();
        d.container_id = None;
        assert!(
            machine_exec_command(&d, None, None, &[], &["ls".into()], false, None, None).is_none()
        );
    }

    #[test]
    fn machine_workdir_only_existing_paths() {
        let mut d = detail();
        let home = Some("/Users/daphnediane");
        let wd = |d: &MachineDetail, p: &str| machine_workdir(d, Some(Path::new(p)), home);
        assert_eq!(
            wd(&d, "/Users/daphnediane/src").as_deref(),
            Some("/Users/daphnediane/src")
        );
        assert_eq!(wd(&d, "/Volumes/X").as_deref(), Some("/home/daphnediane"));
        d.home_mount = Some("none".into());
        assert_eq!(
            wd(&d, "/Users/daphnediane/src").as_deref(),
            Some("/home/daphnediane")
        );
    }

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

    /// The probe must arrive as ONE argument — `machine run` joins and
    /// re-evals its args, so an argv-shaped probe would lose `-c`'s
    /// payload (apple/container#1954; this was the C12 bug).
    #[test]
    fn shell_probe_is_single_arg() {
        let cmd = shell_probe_command(Some("dev"), Some("root"));
        assert_eq!(
            args_of(&cmd),
            [
                "machine",
                "run",
                "-i",
                "-n",
                "dev",
                "-u",
                "root",
                "--",
                SHELL_PROBE
            ]
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
