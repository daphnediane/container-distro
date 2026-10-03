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
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use cm_core::container::{
    self, ContainerInfo, container_cmd, default_machine_name, ensure_started, validate_name,
};
use cm_core::distro::{DistroSummary, LABEL_DISTRO, LABEL_HOME_MOUNT, state_dir};
use cm_core::oci;
use cm_core::table::{columns, human_bytes};

use crate::cli::{CreateOpts, Format};
use crate::spec::{DistroSpec, HostUser, INIT_DIR, SpecChanges, automounts};

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

fn command_stdout(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{cmd}`"))?;
    if !out.status.success() {
        bail!("`{cmd} {}` failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn host_user() -> Result<HostUser> {
    Ok(HostUser {
        name: command_stdout("id", &["-un"])?,
        uid: command_stdout("id", &["-u"])?.parse()?,
        gid: command_stdout("id", &["-g"])?.parse()?,
    })
}

/// Defaults matching `container machine`: half the host's CPUs and memory.
fn default_resources() -> (u64, String) {
    let cpus = std::thread::available_parallelism().map_or(2, |n| n.get() as u64);
    let mem = command_stdout("sysctl", &["-n", "hw.memsize"])
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or_else(
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
fn distros() -> Result<Vec<ContainerInfo>> {
    Ok(container::list_containers()?
        .into_iter()
        .filter(|c| c.configuration.labels.contains_key(LABEL_DISTRO))
        .collect())
}

fn find(name: &str) -> Result<ContainerInfo> {
    distros()?
        .into_iter()
        .find(|c| c.id() == name)
        .with_context(|| format!("no distro named `{name}`"))
}

fn resolve(name: Option<String>) -> Result<String> {
    name.or_else(default_name).context(
        "no distro given and no default set (use -n NAME or `container distro set-default`)",
    )
}

fn build_spec(name: String, image: String, opts: &CreateOpts) -> Result<DistroSpec> {
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

fn create_from_spec(spec: &DistroSpec, no_boot: bool, set_default: bool) -> Result<ExitCode> {
    check_sources(spec)?;
    if distros()?.iter().any(|c| c.id() == spec.name) {
        bail!("distro `{}` already exists", spec.name);
    }
    create_container(spec)?;
    if !no_boot {
        boot(&spec.name)?;
    }
    if set_default || default_name().is_none() {
        write_default(Some(&spec.name))?;
    }
    println!("{}", spec.name);
    Ok(ExitCode::SUCCESS)
}

pub fn create(name: Option<String>, opts: &CreateOpts, image: &str) -> Result<ExitCode> {
    ensure_started()?;
    let name = name.unwrap_or_else(|| default_machine_name(image));
    let spec = build_spec(name, image.to_string(), opts)?;
    create_from_spec(&spec, opts.no_boot, opts.set_default)
}

fn summarize(c: &ContainerInfo, default: Option<&str>) -> DistroSummary {
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
        image: cfg.image.as_ref().map(|i| i.reference.clone()),
        platform: cfg.platform.as_ref().map(ToString::to_string),
        home_mount: cfg.labels.get(LABEL_HOME_MOUNT).cloned(),
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

pub fn list(quiet: bool, running: bool, format: Format) -> Result<ExitCode> {
    ensure_started()?;
    let default = default_name();
    let mut rows: Vec<DistroSummary> = distros()?
        .iter()
        .filter(|c| !running || c.is_running())
        .map(|c| summarize(c, default.as_deref()))
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    if quiet {
        for r in &rows {
            println!("{}", r.id);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if format == Format::Json {
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(ExitCode::SUCCESS);
    }
    let dash = || "-".to_string();
    let mut table = vec![
        [
            "NAME", "IMAGE", "STATE", "IP", "CPUS", "MEMORY", "MOUNTS", "PORTS", "DEFAULT",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for r in &rows {
        table.push(vec![
            r.id.clone(),
            r.image.clone().unwrap_or_else(dash),
            r.status.clone(),
            r.ip_address.clone().unwrap_or_else(dash),
            r.cpus.map_or_else(dash, |c| c.to_string()),
            r.memory.map_or_else(dash, human_bytes),
            if r.mounts.is_empty() {
                dash()
            } else {
                r.mounts.join(",")
            },
            if r.ports.is_empty() {
                dash()
            } else {
                r.ports.join(",")
            },
            if r.default { "*".into() } else { String::new() },
        ]);
    }
    for line in columns(&table, 2) {
        println!("{}", line.trim_end());
    }
    Ok(ExitCode::SUCCESS)
}

pub fn inspect(names: &[String]) -> Result<ExitCode> {
    ensure_started()?;
    for n in names {
        find(n)?;
    }
    let mut args = vec!["inspect"];
    args.extend(names.iter().map(String::as_str));
    run_container(&args)?;
    Ok(ExitCode::SUCCESS)
}

pub fn start(name: &str) -> Result<ExitCode> {
    ensure_started()?;
    find(name)?;
    boot(name)?;
    Ok(ExitCode::SUCCESS)
}

pub fn stop(names: &[String]) -> Result<ExitCode> {
    ensure_started()?;
    for n in names {
        if find(n)?.is_running() {
            run_container_quiet(&["stop", n])?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn delete(force: bool, names: &[String]) -> Result<ExitCode> {
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
    Ok(ExitCode::SUCCESS)
}

/// Default working directory: the host cwd's guest path if it is shared
/// (via the home mount or an extra mount), else the guest home.
fn default_workdir(spec: &DistroSpec, home: &str) -> String {
    env::current_dir()
        .ok()
        .and_then(|cwd| spec.guest_path(&cwd, home))
        .unwrap_or_else(|| spec.user.guest_home())
}

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

/// Exec into the distro via `container exec`.
///
/// Unlike `container machine run`, `container exec` takes an argv vector,
/// so commands arrive exactly as given (no shell re-evaluation).
pub fn run(o: RunOpts) -> Result<ExitCode> {
    ensure_started()?;
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
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"))
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
pub fn set(name: &str, changes: &SpecChanges) -> Result<ExitCode> {
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
    Ok(ExitCode::SUCCESS)
}

pub fn set_default(name: Option<&str>) -> Result<ExitCode> {
    if let Some(n) = name {
        ensure_started()?;
        find(n)?;
    }
    write_default(name)?;
    Ok(ExitCode::SUCCESS)
}

pub fn export(name: &str, output: Option<&Path>) -> Result<ExitCode> {
    ensure_started()?;
    find(name)?;
    let mut args = vec!["export".to_string(), name.to_string()];
    if let Some(o) = output {
        args.push("--output".into());
        args.push(o.to_string_lossy().into_owned());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container(&args)?;
    Ok(ExitCode::SUCCESS)
}

pub fn import(name: &str, file: &Path, opts: &CreateOpts) -> Result<ExitCode> {
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
