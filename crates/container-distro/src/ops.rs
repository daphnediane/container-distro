/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` operations, composed from the `container` CLI.

use std::env;
use std::fs;
use std::io::{self, IsTerminal};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use cm_core::container::{
    self, ContainerInfo, Machine, container_cmd, default_machine_name, ensure_started,
    validate_name,
};
use cm_core::naming::{label_lookup, state_dir};
use cm_core::oci;
use serde::{Deserialize, Serialize};

use crate::spec::{
    DistroSpec, HomeMount, HostUser, INIT_DIR, LABEL_DISTRO, LABEL_HOME_MOUNT, MountSpec,
    PublishSpec, SpecChanges, automounts,
};

/// One distro, as reported by [`summaries`] (and `list --format json`).
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
    /// Host disk space used by the root filesystem, in bytes.
    #[serde(default)]
    pub disk_size: Option<u64>,
    /// ISO 8601 UTC.
    #[serde(default)]
    pub created_date: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub home_mount: Option<String>,
    /// User mounts as `SRC:DST[:ro]`.
    #[serde(default)]
    pub mounts: Vec<String>,
    /// Published ports as `IP:HOST:GUEST/PROTO`.
    #[serde(default)]
    pub ports: Vec<String>,
}

impl DistroSummary {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// Options shared by `create` and `import`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CreateOptions {
    /// Share a host directory: SRC:DST[:ro] (repeatable)
    #[arg(short = 'v', long = "volume", value_name = "SRC:DST[:ro]")]
    pub volumes: Vec<MountSpec>,

    /// Share every /Volumes/<name> at /mnt/<name> (like WSL's /mnt/<drive>)
    #[arg(long)]
    pub automount: bool,

    /// Publish a port: [HOST_IP:]HOST_PORT[:GUEST_PORT][/PROTO]; HOST_IP
    /// defaults to 127.0.0.1 (repeatable)
    #[arg(short = 'p', long = "publish", value_name = "SPEC")]
    pub publish: Vec<PublishSpec>,

    /// Number of virtual CPUs (default: half the host's)
    #[arg(long)]
    pub cpus: Option<u64>,

    /// Memory, e.g. 8G (default: half the host's)
    #[arg(long)]
    pub memory: Option<String>,

    /// How to share your macOS home directory
    #[arg(long, value_enum, default_value_t = HomeMount::Rw)]
    pub home_mount: HomeMount,

    /// Create without booting
    #[arg(long)]
    pub no_boot: bool,

    /// Make this the default distro (automatic when nothing is the default)
    #[arg(long)]
    pub set_default: bool,
}

const INIT_SCRIPT: &str = include_str!("../assets/init");
const CREATE_USER_SCRIPT: &str = include_str!("../assets/create-user.sh");

/// Run a `container` command with inherited stdio; fail on non-zero exit.
fn run_container(args: &[&str]) -> Result<()> {
    let status = container_cmd()
        .args(args)
        .status()
        .with_context(|| format!("failed to run `container {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`container {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

/// Like [`run_container`] but with stdout discarded (for commands that
/// echo the container ID).
fn run_container_quiet(args: &[&str]) -> Result<()> {
    let status = container_cmd()
        .args(args)
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("failed to run `container {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`container {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

fn host_home() -> Result<String> {
    env::var("HOME").context("HOME is not set")
}

/// The login name for `uid`, via `getpwuid_r` — no subprocess for a
/// PATH-hijackable `id` lookup.
fn passwd_name(uid: libc::uid_t) -> Option<String> {
    // SAFETY: `pwd` is zeroed, `buf` is valid, and `result` is checked
    // before `pwd.pw_name` (which points into `buf`) is read.
    let mut pwd = unsafe { std::mem::zeroed::<libc::passwd>() };
    let mut result = std::ptr::null_mut();
    let mut buf = vec![0u8; 1024];
    loop {
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };
        if rc == 0 && !result.is_null() {
            // SAFETY: on success pwd.pw_name is a valid C string in `buf`.
            return unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
                .to_str()
                .ok()
                .map(str::to_string);
        }
        if rc == libc::ERANGE {
            buf.resize(buf.len() * 2, 0);
        } else {
            return None;
        }
    }
}

fn host_user() -> Result<HostUser> {
    // SAFETY: getuid/getgid take no arguments and cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let name = passwd_name(uid).with_context(|| format!("no passwd entry for uid {uid}"))?;
    // The name lands in container labels, CONTAINER_* env, and — inside
    // the guest — /etc/passwd and a sudoers.d path written by root. Hold
    // it to the charset create-user.sh accepts.
    if !safe_user_name(&name) {
        bail!("refusing to provision unsafe username `{name}`");
    }
    Ok(HostUser { name, uid, gid })
}

/// The username charset `create-user.sh` accepts: alphanumerics plus
/// `.`/`_`/`-`, never leading with `.` or `-`.
fn safe_user_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Host RAM in bytes via `sysctlbyname` — no `sysctl` subprocess.
fn host_memsize() -> Option<u64> {
    let mut mem: u64 = 0;
    let mut len = std::mem::size_of_val(&mem);
    // SAFETY: the name is a NUL-terminated literal; out pointers are valid.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut mem).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<u64>()).then_some(mem)
}

/// Defaults matching `container machine`: half the host's CPUs and memory.
///
/// These one-off libc calls (getpwuid_r/getuid/getgid, sysctlbyname,
/// available_parallelism) stay dependency-free on purpose; if we ever
/// need live process/network/disk introspection — e.g. watching guest
/// listeners for WSL-style automatic port forwarding — the `sysinfo`
/// crate (CPU count, memory, processes, users) is the switch point.
fn default_resources() -> (u64, String) {
    let cpus = std::thread::available_parallelism().map_or(2, |n| n.get() as u64);
    let mem = host_memsize().map_or_else(
        || "4G".to_string(),
        |b| format!("{}M", b / 2 / (1024 * 1024)),
    );
    ((cpus / 2).max(1), mem)
}

/// Write the init assets to the host directory mounted at `/sbin.distro`,
/// refreshing them if this build's copies differ.
fn assets_dir() -> Result<PathBuf> {
    let dir = state_dir()?.join("sbin.distro");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    for (name, body) in [
        ("init", INIT_SCRIPT),
        ("create-user.sh", CREATE_USER_SCRIPT),
    ] {
        let path = dir.join(name);
        if fs::read_to_string(&path).ok().as_deref() != Some(body) {
            fs::write(&path, body)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(dir)
}

fn default_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("default-distro"))
}

/// The default distro's name, if one is set.
pub fn default_name() -> Option<String> {
    let s = fs::read_to_string(default_file().ok()?).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn write_default(name: Option<&str>) -> Result<()> {
    let path = default_file()?;
    match name {
        Some(n) => {
            fs::create_dir_all(state_dir()?)?;
            fs::write(&path, format!("{n}\n"))?;
        }
        None => {
            if path.exists() {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// All distro containers.
pub fn distros() -> Result<Vec<ContainerInfo>> {
    Ok(container::list_containers()?
        .into_iter()
        .filter(|c| label_lookup(&c.configuration.labels, LABEL_DISTRO).is_some())
        .collect())
}

pub fn find(name: &str) -> Result<ContainerInfo> {
    distros()?
        .into_iter()
        .find(|c| c.id() == name)
        .with_context(|| format!("no distro named `{name}`"))
}

pub fn resolve(name: Option<String>) -> Result<String> {
    name.or_else(default_name).context(
        "no distro given and no default set (use -n NAME or `container distro set-default`)",
    )
}

fn build_spec(name: String, image: String, opts: &CreateOptions) -> Result<DistroSpec> {
    validate_name(&name)?;
    let mut mounts = opts.volumes.clone();
    if opts.automount {
        let entries: Vec<(String, bool)> = fs::read_dir("/Volumes")
            .context("failed to read /Volumes")?
            .filter_map(|e| e.ok())
            .map(|e| {
                let is_link = e.file_type().is_ok_and(|t| t.is_symlink());
                (e.file_name().to_string_lossy().into_owned(), is_link)
            })
            .collect();
        for m in automounts(&entries) {
            if !mounts.iter().any(|x| x.target == m.target) {
                mounts.push(m);
            }
        }
    }
    let (def_cpus, def_mem) = default_resources();
    Ok(DistroSpec {
        name,
        image,
        cpus: Some(opts.cpus.unwrap_or(def_cpus)),
        memory: Some(opts.memory.clone().unwrap_or(def_mem)),
        home_mount: opts.home_mount,
        mounts,
        publish: opts.publish.clone(),
        user: host_user()?,
    })
}

fn check_sources(spec: &DistroSpec) -> Result<()> {
    for m in &spec.mounts {
        if !Path::new(&m.source).is_dir() {
            bail!("mount source `{}` is not a directory", m.source);
        }
    }
    Ok(())
}

fn create_container(spec: &DistroSpec) -> Result<()> {
    let assets = assets_dir()?;
    let args = spec.create_args(&assets.to_string_lossy(), &host_home()?);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container_quiet(&args)
}

/// Boot a distro and (idempotently) provision the user account.
fn boot(name: &str) -> Result<()> {
    assets_dir()?;
    run_container_quiet(&["start", name])?;
    let init = format!("{INIT_DIR}/init");
    run_container(&["exec", "--user", "0:0", name, &init, "-u"])
        .with_context(|| format!("user setup failed in `{name}`"))
}

fn ensure_running(name: &str) -> Result<ContainerInfo> {
    let info = find(name)?;
    if info.is_running() {
        return Ok(info);
    }
    boot(name)?;
    find(name)
}

/// Whether nothing is the default: no default distro among `distros` (a
/// stale one doesn't count) and no default machine.
fn no_default(
    distro_default: Option<&str>,
    distros: &[ContainerInfo],
    machines: &[Machine],
) -> bool {
    let distro_set = distro_default.is_some_and(|d| distros.iter().any(|c| c.id() == d));
    !distro_set && !machines.iter().any(|m| m.default)
}

fn create_from_spec(spec: &DistroSpec, no_boot: bool, set_default: bool) -> Result<String> {
    check_sources(spec)?;
    let existing = distros()?;
    if existing.iter().any(|c| c.id() == spec.name) {
        bail!("distro `{}` already exists", spec.name);
    }
    create_container(spec)?;
    if !no_boot {
        boot(&spec.name)?;
    }
    // On request, or (like `container machine create`) when nothing is the
    // default. Otherwise leave it alone: under `cm`'s unified namespace a
    // distro default overrides the default machine. If machines can't be
    // listed, a distro is the only usable target anyway.
    let set_default = set_default
        || no_default(
            default_name().as_deref(),
            &existing,
            &container::list_machines().unwrap_or_default(),
        );
    if set_default {
        write_default(Some(&spec.name))?;
    }
    Ok(spec.name.clone())
}

/// Create (and unless `no_boot`, boot) a distro; returns its name.
pub fn create(name: Option<String>, opts: &CreateOptions, image: &str) -> Result<String> {
    ensure_started()?;
    let name = name.unwrap_or_else(|| default_machine_name(image));
    let spec = build_spec(name, image.to_string(), opts)?;
    create_from_spec(&spec, opts.no_boot, opts.set_default)
}

fn summarize(c: &ContainerInfo, default: Option<&str>, app_root: Option<&Path>) -> DistroSummary {
    let cfg = &c.configuration;
    let home = host_home().unwrap_or_default();
    let spec = DistroSpec::from_container(c, &home).ok();
    DistroSummary {
        id: c.id().to_string(),
        status: c.status.state.clone(),
        default: default == Some(c.id()),
        ip_address: c.ipv4().map(str::to_string),
        cpus: cfg.resources.as_ref().and_then(|r| r.cpus),
        memory: cfg.resources.as_ref().and_then(|r| r.memory_in_bytes),
        disk_size: app_root.and_then(|r| container::container_disk_usage(r, c.id())),
        created_date: cfg.creation_date.clone(),
        image: cfg.image.as_ref().map(|i| i.reference.clone()),
        platform: cfg.platform.as_ref().map(ToString::to_string),
        home_mount: label_lookup(&cfg.labels, LABEL_HOME_MOUNT).cloned(),
        mounts: spec
            .as_ref()
            .map(|s| s.mounts.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        ports: spec
            .as_ref()
            .map(|s| s.publish.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
    }
}

/// All distros, sorted by name.
pub fn summaries(running_only: bool) -> Result<Vec<DistroSummary>> {
    let default = default_name();
    let app_root = container::app_root();
    let mut rows: Vec<DistroSummary> = distros()?
        .iter()
        .filter(|c| !running_only || c.is_running())
        .map(|c| summarize(c, default.as_deref(), app_root.as_deref()))
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}

pub fn inspect(names: &[String]) -> Result<()> {
    ensure_started()?;
    for n in names {
        find(n)?;
    }
    let mut args = vec!["inspect"];
    args.extend(names.iter().map(String::as_str));
    run_container(&args)?;
    Ok(())
}

pub fn start(name: &str) -> Result<()> {
    ensure_started()?;
    find(name)?;
    boot(name)?;
    Ok(())
}

pub fn stop(names: &[String]) -> Result<()> {
    ensure_started()?;
    for n in names {
        if find(n)?.is_running() {
            run_container_quiet(&["stop", n])?;
        }
    }
    Ok(())
}

pub fn delete(force: bool, names: &[String]) -> Result<()> {
    ensure_started()?;
    for n in names {
        let info = find(n)?;
        if info.is_running() {
            if !force {
                bail!("distro `{n}` is running (stop it first or use --force)");
            }
            run_container_quiet(&["stop", n])?;
        }
        run_container_quiet(&["delete", n])?;
        remove_snapshot_images(n, None);
        if default_name().as_deref() == Some(n.as_str()) {
            write_default(None)?;
        }
    }
    Ok(())
}

/// Default working directory: the host cwd's guest path if it is shared
/// (via the home mount or an extra mount), else the guest home.
fn default_workdir(spec: &DistroSpec, home: &str) -> String {
    env::current_dir()
        .ok()
        .and_then(|cwd| spec.guest_path(&cwd, home))
        .unwrap_or_else(|| spec.user.guest_home())
}

#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    pub name: Option<String>,
    pub user: Option<String>,
    pub root: bool,
    pub workdir: Option<String>,
    pub env: Vec<String>,
    pub shell: bool,
    pub no_login: bool,
    pub command: Vec<String>,
}

/// Build the `container exec` command that runs `o` in its distro,
/// booting the distro first if needed. The caller execs it.
///
/// Unlike `container machine run`, `container exec` takes an argv vector,
/// so commands arrive exactly as given (no shell re-evaluation).
pub fn run_command(o: RunOpts) -> Result<Command> {
    ensure_started()?;
    if let Some(u) = &o.user {
        container::validate_user(u)?;
    }
    let name = resolve(o.name)?;
    let info = ensure_running(&name)?;
    let home = host_home()?;
    let spec = DistroSpec::from_container(&info, &home)?;
    let me = &spec.user;

    let mut cmd = container_cmd();
    cmd.args(["exec", "-i"]);
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        cmd.arg("-t");
    }
    if o.root {
        cmd.args(["--user", "0:0"]);
    } else if let Some(u) = &o.user {
        cmd.args(["--user", u]);
    } else {
        cmd.arg("--user").arg(format!("{}:{}", me.uid, me.gid));
        for (k, v) in [
            ("HOME", me.guest_home()),
            ("USER", me.name.clone()),
            ("LOGNAME", me.name.clone()),
        ] {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
    }
    let workdir = o.workdir.unwrap_or_else(|| default_workdir(&spec, &home));
    cmd.args(["--workdir", &workdir]);
    if let Ok(term) = env::var("TERM") {
        cmd.arg("--env").arg(format!("TERM={term}"));
    }
    for e in &o.env {
        cmd.args(["--env", e]);
    }
    cmd.arg(&name);
    let init = format!("{INIT_DIR}/init");
    if o.command.is_empty() {
        cmd.arg(&init).arg(if o.no_login { "-s" } else { "-l" });
    } else if o.shell {
        cmd.arg(&init).arg("-c").args(&o.command);
    } else {
        cmd.args(&o.command);
    }
    Ok(cmd)
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn snapshot_repo(name: &str) -> String {
    format!("local/distro-{name}")
}

/// Best-effort removal of `set`/`import` snapshot images for `name`,
/// except `keep`.
fn remove_snapshot_images(name: &str, keep: Option<&str>) {
    let prefix = format!("{}:", snapshot_repo(name));
    let Ok(out) = container_cmd()
        .args(["image", "list", "--quiet"])
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    for r in String::from_utf8_lossy(&out.stdout).lines() {
        let r = r.trim();
        let short = r.strip_prefix("docker.io/").unwrap_or(r);
        if short.starts_with(&prefix) && Some(short) != keep {
            let _ = container_cmd()
                .args(["image", "delete", r])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// `container distro set`: recreate the container with new settings on a
/// snapshot of its current filesystem.
pub fn set(name: &str, changes: &SpecChanges) -> Result<()> {
    ensure_started()?;
    if changes.is_empty() {
        bail!("nothing to change (see `container distro set --help`)");
    }
    let info = find(name)?;
    let old = DistroSpec::from_container(&info, &host_home()?)?;
    let mut new = changes.apply(&old)?;
    check_sources(&new)?;
    let was_running = info.is_running();
    if was_running {
        run_container_quiet(&["stop", name])?;
    }

    // Snapshot the current filesystem into an image.
    let tar = tempfile::Builder::new()
        .prefix("distro-set-")
        .suffix(".tar")
        .tempfile()?;
    let tar_path = tar.path().to_string_lossy().into_owned();
    run_container(&["export", name, "--output", &tar_path])?;
    let snapshot = format!("{}:{}", snapshot_repo(name), timestamp());
    oci::load_rootfs(tar.path(), &snapshot)?;
    drop(tar);

    new.image = snapshot.clone();
    run_container_quiet(&["delete", name])?;
    if let Err(e) = create_container(&new) {
        // Restore the old settings on the same snapshot; nothing is lost.
        let rollback = DistroSpec {
            image: snapshot.clone(),
            ..old
        };
        create_container(&rollback).context("rollback after a failed `set` also failed")?;
        if was_running {
            boot(name)?;
        }
        return Err(e.context("failed to recreate the distro; previous settings restored"));
    }
    if was_running {
        boot(name)?;
    }
    remove_snapshot_images(name, Some(&snapshot));
    Ok(())
}

pub fn set_default(name: Option<&str>) -> Result<()> {
    if let Some(n) = name {
        ensure_started()?;
        find(n)?;
    }
    write_default(name)?;
    Ok(())
}

pub fn export(name: &str, output: Option<&Path>) -> Result<()> {
    ensure_started()?;
    find(name)?;
    let mut args = vec!["export".to_string(), name.to_string()];
    if let Some(o) = output {
        args.push("--output".into());
        args.push(o.to_string_lossy().into_owned());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container(&args)?;
    Ok(())
}

/// Create a distro from a rootfs tar; returns its name.
pub fn import(name: &str, file: &Path, opts: &CreateOptions) -> Result<String> {
    validate_name(name)?;
    ensure_started()?;
    if distros()?.iter().any(|c| c.id() == name) {
        bail!("distro `{name}` already exists");
    }
    let stdin_copy;
    let rootfs = if file == Path::new("-") {
        stdin_copy = oci::stdin_to_tempfile()?;
        stdin_copy.path()
    } else {
        file
    };
    let reference = format!("{}:imported-{}", snapshot_repo(name), timestamp());
    oci::load_rootfs(rootfs, &reference)?;
    let spec = build_spec(name.to_string(), reference, opts)?;
    create_from_spec(&spec, opts.no_boot, opts.set_default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distro(id: &str) -> ContainerInfo {
        serde_json::from_str(&format!(
            r#"{{"configuration":{{"id":"{id}"}},"id":"{id}"}}"#
        ))
        .unwrap()
    }

    fn machine(id: &str, default: bool) -> Machine {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","status":"running","default":{default}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn safe_user_name_charset() {
        for ok in ["daphne", "a.b-c_d", "x", "User1", "0root"] {
            assert!(safe_user_name(ok), "{ok}");
        }
        for bad in ["", "-evil", ".hidden", "a b", "x:y", "a/b", "u$", "u!"] {
            assert!(!safe_user_name(bad), "{bad}");
        }
    }

    /// Run create-user.sh in validate-only mode; exits 0 when the env
    /// passes the sanitization block and never touches the filesystem.
    fn validate_env(user: &str, uid: &str, gid: &str, home: &str) -> bool {
        Command::new("sh")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/create-user.sh"
            ))
            .env("CONTAINER_USER", user)
            .env("CONTAINER_UID", uid)
            .env("CONTAINER_GID", gid)
            .env("CONTAINER_HOME", home)
            .env("DISTRO_VALIDATE_ONLY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn create_user_rejects_unsafe_env() {
        for bad in ["", "-evil", ".hidden", "bad name", "x:y", "a/b", "u$"] {
            assert!(!validate_env(bad, "501", "20", "/home/x"), "user {bad}");
        }
        for bad in ["", "x", "1.2", "501;rm", " 1"] {
            assert!(!validate_env("u", bad, "20", "/home/u"), "uid {bad}");
            assert!(!validate_env("u", "501", bad, "/home/u"), "gid {bad}");
        }
        for bad in [
            "",
            "relative",
            "/home/../etc",
            "/x/..",
            "../y",
            "/a b",
            "/x:y",
        ] {
            assert!(!validate_env("u", "501", "20", bad), "home {bad}");
        }
        // Legitimate shapes still pass.
        assert!(validate_env("daphne", "501", "20", "/home/daphne"));
        assert!(validate_env("a.b-c_d", "0", "0", "/home/a.b-c_d"));
    }

    #[test]
    fn no_default_rule() {
        let d = [distro("d1")];
        assert!(no_default(None, &d, &[]));
        assert!(no_default(None, &d, &[machine("m", false)]));
        assert!(!no_default(Some("d1"), &d, &[]));
        // A stale default (distro deleted) doesn't count.
        assert!(no_default(Some("gone"), &d, &[]));
        assert!(!no_default(Some("gone"), &d, &[machine("m", true)]));
        assert!(!no_default(None, &[], &[machine("m", true)]));
    }
}
