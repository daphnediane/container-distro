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
    /// No shares, no network, no agent, no privilege grant — what
    /// `--restricted` produces.
    #[serde(default)]
    pub restricted: bool,
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

    /// Share every /Volumes/<name> at /mnt/<name> (like WSL's
    /// /mnt/<drive>); hidden, Apple-private (com.apple.*), and
    /// unreadable volumes are skipped. The list is resolved once here —
    /// volumes attached later need `set --add-volume`, and an ejected
    /// volume will fail `start` until removed with `set --rm-volume`
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
    #[arg(long, value_enum)]
    pub home_mount: Option<HomeMount>,

    /// Attach to a container network by name ("none" for no network)
    #[arg(long, value_name = "NAME")]
    pub network: Option<String>,

    /// Forward the host SSH agent socket into the distro
    #[arg(long, conflicts_with = "no_ssh")]
    pub ssh: bool,

    /// Do not forward the host SSH agent socket
    #[arg(long)]
    pub no_ssh: bool,

    /// Grant the provisioned user passwordless sudo/doas
    #[arg(long, conflicts_with = "no_sudo")]
    pub sudo: bool,

    /// Do not grant the provisioned user sudo/doas
    #[arg(long)]
    pub no_sudo: bool,

    /// Restricted defaults for semi-trusted images: home mount none, no
    /// network, no SSH agent, no sudo/doas. Explicit flags still apply.
    /// A convenience preset, not a sandbox.
    #[arg(long, visible_alias = "untrusted")]
    pub restricted: bool,

    /// Create without booting
    #[arg(long)]
    pub no_boot: bool,

    /// Make this the default distro (automatic when nothing is the default)
    #[arg(long)]
    pub set_default: bool,
}

const INIT_SCRIPT: &str = include_str!("../assets/init");
const CREATE_USER_SCRIPT: &str = include_str!("../assets/create-user.sh");
const GRANT_ADMIN_SCRIPT: &str = include_str!("../assets/grant-admin.sh");

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

/// Write the init assets into `dir`, refreshing them if this build's
/// copies differ. `admin` selects the flavor: only admin distros get
/// `grant-admin.sh`, so restricted guests have no privilege-granting
/// code at all.
fn write_assets(dir: &Path, admin: bool) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let mut files = vec![
        ("init", INIT_SCRIPT),
        ("create-user.sh", CREATE_USER_SCRIPT),
    ];
    if admin {
        files.push(("grant-admin.sh", GRANT_ADMIN_SCRIPT));
    } else {
        // No stray grant script may linger in the restricted flavor.
        match fs::remove_file(dir.join("grant-admin.sh")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            r => r.with_context(|| format!("failed to clean {}", dir.display()))?,
        }
    }
    for (name, body) in files {
        let path = dir.join(name);
        if fs::read_to_string(&path).ok().as_deref() != Some(body) {
            fs::write(&path, body)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Refresh both init-assets flavors (called on every boot).
fn refresh_assets() -> Result<()> {
    let state = state_dir()?;
    write_assets(&state.join("sbin.distro"), true)?;
    write_assets(&state.join("sbin.distro.restricted"), false)
}

/// The host directory a distro mounts at `/sbin.distro`, refreshed to
/// this build's copies.
fn assets_dir(admin: bool) -> Result<PathBuf> {
    refresh_assets()?;
    let flavor = if admin {
        "sbin.distro"
    } else {
        "sbin.distro.restricted"
    };
    Ok(state_dir()?.join(flavor))
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
        let mut taken: std::collections::HashSet<String> =
            mounts.iter().map(|x| x.target.clone()).collect();
        for m in automounts(&entries) {
            // VZ rejects shares the virtiofs helper can't enumerate
            // (TCC/SIP-protected volumes like Time Machine destinations)
            // with "directory sharing device configuration is invalid";
            // probe readability so one bad volume can't sink the boot.
            // `.next()` is needed — readdir errors surface lazily.
            if fs::read_dir(&m.source)
                .and_then(|mut d| d.next().transpose())
                .is_err()
            {
                eprintln!(
                    "container-distro: skipping automount {}: not readable on the host",
                    m.source
                );
                continue;
            }
            if !taken.insert(m.target.clone()) {
                eprintln!(
                    "container-distro: skipping automount {}: {} is already a mount target",
                    m.source, m.target
                );
                continue;
            }
            mounts.push(m);
        }
    }
    let (def_cpus, def_mem) = default_resources();
    // --restricted is a defaults preset; explicit flags still apply.
    let r = opts.restricted;
    let admin = if opts.sudo {
        true
    } else if opts.no_sudo {
        false
    } else {
        !r
    };
    let spec = DistroSpec {
        name,
        image,
        cpus: Some(opts.cpus.unwrap_or(def_cpus)),
        memory: Some(opts.memory.clone().unwrap_or(def_mem)),
        home_mount: opts
            .home_mount
            .unwrap_or(if r { HomeMount::None } else { HomeMount::Rw }),
        mounts,
        publish: opts.publish.clone(),
        network: opts
            .network
            .clone()
            .or_else(|| r.then(|| "none".to_string())),
        ssh: if opts.ssh {
            true
        } else if opts.no_ssh {
            false
        } else {
            !r
        },
        admin,
        admin_grant: admin,
        user: host_user()?,
    };
    if spec.network.as_deref() == Some("none") && !spec.publish.is_empty() {
        eprintln!("container-distro: published ports have no effect without a network");
    }
    Ok(spec)
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
    let assets = assets_dir(spec.admin)?;
    let args = spec.create_args(&assets.to_string_lossy(), &host_home()?);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container_quiet(&args)
}

/// Boot a distro and (idempotently) provision the user account.
fn boot(name: &str, _lock: &DistroLock) -> Result<()> {
    refresh_assets()?;
    run_container_quiet(&["start", name])?;
    let init = format!("{INIT_DIR}/init");
    run_container(&["exec", "--user", "0:0", name, &init, "-u"])
        .with_context(|| format!("user setup failed in `{name}`"))
}

fn ensure_running(name: &str) -> Result<ContainerInfo> {
    let lock = DistroLock::acquire(name)?;
    // Recovery runs before `find` so it can rebuild a distro whose
    // container was deleted by an interrupted `set`.
    container::warn_unverified_version();
    recover_interrupted(name, &lock)?;
    let info = find(name)?;
    if info.is_running() {
        return Ok(info);
    }
    boot(name, &lock)?;
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
    let lock = DistroLock::acquire(&spec.name)?;
    let existing = distros()?;
    if existing.iter().any(|c| c.id() == spec.name) {
        bail!("distro `{}` already exists", spec.name);
    }
    bail_if_staged(&spec.name)?;
    create_container(spec)?;
    if !no_boot {
        boot(&spec.name, &lock)?;
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
        restricted: spec.as_ref().is_some_and(DistroSpec::is_restricted),
    }
}

/// All distros, sorted by name.
pub fn summaries(running_only: bool) -> Result<Vec<DistroSummary>> {
    let default = default_name();
    // DISK sizes are read from `container`'s internal storage layout.
    container::warn_unverified_version();
    let app_root = container::app_root();
    let mut rows: Vec<DistroSummary> = distros()?
        .iter()
        .filter(|c| !running_only || c.is_running())
        .map(|c| summarize(c, default.as_deref(), app_root.as_deref()))
        .collect();
    if !running_only {
        // Filesystems staged by `set`s whose distro is gone get
        // pseudo-rows so they can be `set`/`rm`'d from the listing.
        let orphans: Vec<DistroSummary> = staged_summaries()
            .into_iter()
            .filter(|s| !rows.iter().any(|r| r.id == s.id))
            .collect();
        rows.extend(orphans);
    }
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
    let lock = DistroLock::acquire(name)?;
    container::warn_unverified_version();
    recover_interrupted(name, &lock)?;
    find(name)?;
    boot(name, &lock)?;
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

/// Remove anything `set` staged for `name`; true when something was.
fn cleanup_preserved(name: &str, _lock: &DistroLock) -> bool {
    let mut removed = false;
    for p in [preserved_rootfs(name), preserved_journal(name)]
        .into_iter()
        .flatten()
    {
        removed |= fs::remove_file(p).is_ok();
    }
    removed
}

pub fn delete(force: bool, names: &[String]) -> Result<()> {
    ensure_started()?;
    for n in names {
        let lock = DistroLock::acquire(n)?;
        let info = match find(n) {
            Ok(info) => info,
            // The container is gone but an interrupted `set` may still
            // have its filesystem staged.
            Err(_) if cleanup_preserved(n, &lock) => {
                eprintln!("removed filesystem staged by an interrupted `set` for `{n}`");
                continue;
            }
            Err(e) => return Err(e),
        };
        if info.is_running() {
            if !force {
                bail!("distro `{n}` is running (stop it first or use --force)");
            }
            run_container_quiet(&["stop", n])?;
        }
        run_container_quiet(&["delete", n])?;
        remove_snapshot_images(n, None);
        cleanup_preserved(n, &lock);
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

/// Refuse to create `name` while an interrupted `set` has its
/// filesystem staged — the user must choose to recover (`set`) or
/// discard (`rm`) it first.
fn bail_if_staged(name: &str) -> Result<()> {
    if let Ok(staged) = preserved_rootfs(name)
        && staged.exists()
    {
        bail!(
            "an interrupted `set` left `{0}`'s filesystem staged — \
             `container distro set {0}` recovers it, `container distro rm {0}` discards it",
            name
        );
    }
    Ok(())
}

/// The container's data directory under `container`'s appRoot.
fn container_dir(id: &str) -> Result<PathBuf> {
    container::app_root()
        .map(|r| r.join("containers").join(id))
        .context("`container system status` did not report an appRoot")
}

/// Files staged by an in-flight `set`, inside our own state directory —
/// the daemon owns `containers/`, so orphaned filesystems live where we
/// can scan for them: `preserved/<name>.ext4` plus a
/// `preserved/<name>.json` journal holding the distro's spec.
fn preserved_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("preserved"))
}

fn preserved_rootfs(name: &str) -> Result<PathBuf> {
    Ok(preserved_dir()?.join(format!("{name}.ext4")))
}

fn preserved_journal(name: &str) -> Result<PathBuf> {
    Ok(preserved_dir()?.join(format!("{name}.json")))
}

/// Proof the per-distro `flock` is held. Only obtainable via
/// [`DistroLock::acquire`], so functions that must run under the lock
/// take `&DistroLock` — the compiler enforces it. The lock releases
/// when the wrapped descriptor closes (drop or process death), so an
/// interrupted operation can never leave a stale lock.
struct DistroLock(#[allow(dead_code)] fs::File);

impl DistroLock {
    /// An advisory `flock` on a per-distro lockfile, serializing `set`,
    /// boot, and delete so concurrent invocations can't interleave a
    /// recreate.
    fn acquire(name: &str) -> Result<Self> {
        let dir = state_dir()?.join("locks");
        fs::create_dir_all(&dir)?;
        let f = fs::File::create(dir.join(format!("{name}.lock")))?;
        f.lock()?;
        Ok(Self(f))
    }
}

/// Whether another process holds `name`'s lock — a `set` recreate in
/// flight. Probe-only: never creates a lockfile.
fn distro_busy(name: &str) -> bool {
    let Ok(path) = state_dir().map(|d| d.join("locks").join(format!("{name}.lock"))) else {
        return false;
    };
    let Ok(f) = fs::File::options().read(true).write(true).open(path) else {
        return false;
    };
    matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock))
}

/// Move a container's `rootfs.ext4` out of its data directory so
/// `container delete` doesn't take it with it, and write a journal of
/// `spec` next to it. The file only exists once the container has booted
/// — `None` for a never-started distro, whose filesystem is still the
/// pristine image (and which `container export` cannot snapshot for the
/// same reason).
///
/// The journal is written before the move so a staged filesystem is
/// never left without the spec needed to recreate its distro.
fn preserve_rootfs(id: &str, spec: &DistroSpec, _lock: &DistroLock) -> Result<Option<PathBuf>> {
    let rootfs = container_dir(id)?.join("rootfs.ext4");
    if !rootfs.exists() {
        return Ok(None);
    }
    let dir = preserved_dir()?;
    fs::create_dir_all(&dir)?;
    // Journal before the filesystem so a staged ext4 is never left
    // without the spec needed to recreate its distro.
    fs::write(dir.join(format!("{id}.json")), serde_json::to_vec(spec)?)?;
    let staged = dir.join(format!("{id}.ext4"));
    if staged.exists() {
        bail!(
            "an earlier interrupted `set` left {} — remove it (or `container distro rm {id}`) before running `set` again",
            staged.display()
        );
    }
    // Copy (`fs::copy` uses clonefile on APFS) rather than move: the
    // original stays in place until `container delete`, so an
    // interruption before then leaves a bootable container plus a spare.
    fs::copy(&rootfs, &staged)
        .with_context(|| format!("failed to preserve {}", rootfs.display()))?;
    Ok(Some(staged))
}

/// Mark `rootfs` as the container's root filesystem via `rootFsOverride`
/// in `runtime-configuration.json`, so `container start` mounts it —
/// otherwise the daemon copies the image snapshot over `rootfs.ext4`,
/// which fails once the file already exists. This is the same mechanism
/// `container machine` uses for its plugin-state disks.
fn set_rootfs_override(dir: &Path, rootfs: &Path) -> Result<()> {
    let cfg = dir.join("runtime-configuration.json");
    let mut v: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&cfg).with_context(|| format!("failed to read {}", cfg.display()))?,
    )?;
    v["options"]["rootFsOverride"] = serde_json::json!({
        "type": {"block": {"sync": {"fsync": {}}, "format": "ext4", "cache": {"on": {}}}},
        "source": rootfs,
        "destination": "/",
        "options": [],
    });
    fs::write(&cfg, serde_json::to_string(&v)?)
        .with_context(|| format!("failed to write {}", cfg.display()))
}

/// Move a preserved rootfs back into a freshly created container and mark
/// it as the container's root filesystem. Removes the `set` journal once
/// the filesystem is safely back in place.
fn restore_rootfs(id: &str, staged: Option<&Path>, _lock: &DistroLock) -> Result<()> {
    let Some(staged) = staged else { return Ok(()) };
    let dir = container_dir(id)?;
    let rootfs = dir.join("rootfs.ext4");
    fs::rename(staged, &rootfs).with_context(|| {
        format!(
            "failed to restore {} — the preserved copy is at {}",
            rootfs.display(),
            staged.display()
        )
    })?;
    set_rootfs_override(&dir, &rootfs)?;
    let _ = fs::remove_file(preserved_journal(id)?);
    Ok(())
}

/// Complete an interrupted `set` for `name`, if one left a filesystem
/// staged under `preserved/`. The container missing means the recreate
/// never happened, so the journaled spec rebuilds it; the container
/// present just needs its rootfs back — unless it already has one, in
/// which case the staged copy is kept (it may be someone's only data).
fn recover_interrupted(name: &str, _lock: &DistroLock) -> Result<()> {
    let staged = preserved_rootfs(name)?;
    let journal = preserved_journal(name)?;
    if !staged.exists() {
        // A journal with no filesystem is a stale marker.
        let _ = fs::remove_file(&journal);
        return Ok(());
    }
    eprintln!("warning: an interrupted `set` staged `{name}`'s filesystem — restoring it");
    if !container_dir(name)?.is_dir() {
        let spec: DistroSpec = serde_json::from_slice(&fs::read(&journal).with_context(|| {
            format!(
                "`{name}` was deleted by an interrupted `set` and its journal is unreadable; \
                 its filesystem is preserved at {}",
                staged.display()
            )
        })?)
        .context("invalid `set` journal")?;
        create_container(&spec)
            .with_context(|| format!("failed to recreate `{name}` from its `set` journal"))?;
    }
    let dir = container_dir(name)?;
    let rootfs = dir.join("rootfs.ext4");
    if rootfs.exists() {
        // The container already has a filesystem — the staged copy may
        // be the only copy of someone's data, so keep it rather than
        // silently discarding it.
        eprintln!(
            "warning: staged filesystem kept at {} (`container distro rm` clears it with the distro)",
            staged.display()
        );
    } else {
        fs::rename(&staged, &rootfs)
            .with_context(|| format!("failed to restore {}", staged.display()))?;
        set_rootfs_override(&dir, &rootfs)?;
        let _ = fs::remove_file(&journal);
    }
    Ok(())
}

/// Pseudo-summaries for filesystems staged by `set`s whose distro no
/// longer exists, so `list` can surface them instead of a stderr
/// warning: "recreating" while another process holds the lock (a `set`
/// is in flight), "interrupted" once it is orphaned and needs `set` or
/// `rm` to resolve.
fn staged_summaries() -> Vec<DistroSummary> {
    let Ok(dir) = preserved_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".ext4"))
                .map(String::from)
        })
        .map(|id| {
            let status = if distro_busy(&id) {
                "recreating"
            } else {
                "interrupted"
            };
            let image = preserved_journal(&id)
                .ok()
                .and_then(|p| fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<DistroSpec>(&b).ok())
                .map(|s| s.image);
            DistroSummary {
                id,
                status: status.into(),
                image,
                ..Default::default()
            }
        })
        .collect()
}

/// `container distro set`: recreate the container with new settings,
/// keeping its filesystem.
pub fn set(name: &str, changes: &SpecChanges) -> Result<()> {
    ensure_started()?;
    // Serialize the whole preserve → recreate → restore sequence, then
    // finish any previous `set` interrupted after its filesystem was
    // staged — which may recreate `name` — before anything else.
    let lock = DistroLock::acquire(name)?;
    container::warn_unverified_version();
    recover_interrupted(name, &lock)?;
    if changes.is_empty() {
        bail!("nothing to change (see `container distro set --help`)");
    }
    let info = find(name)?;
    let old = DistroSpec::from_container(&info, &host_home()?)?;
    let new = changes.apply(&old)?;
    check_sources(&new)?;
    let was_running = info.is_running();
    if was_running {
        run_container_quiet(&["stop", name])?;
    }

    // Carry the materialized rootfs over directly rather than
    // round-tripping it through `container export` and a snapshot image.
    // This reads and writes `container`'s internal storage layout.
    let preserved = preserve_rootfs(info.id(), &old, &lock)?;
    run_container_quiet(&["delete", name])?;
    if let Err(e) =
        create_container(&new).and_then(|()| restore_rootfs(info.id(), preserved.as_deref(), &lock))
    {
        // Restore the old settings on the preserved rootfs; nothing is lost.
        let _ = run_container_quiet(&["delete", name]);
        let rollback = create_container(&old)
            .and_then(|()| restore_rootfs(info.id(), preserved.as_deref(), &lock));
        if let Err(r) = rollback {
            return Err(e.context(format!(
                "failed to recreate the distro; restoring the previous settings failed too: {r:#}"
            )));
        }
        if was_running {
            boot(name, &lock)?;
        }
        return Err(e.context("failed to recreate the distro; previous settings restored"));
    }
    if was_running {
        boot(name, &lock)?;
    }
    remove_snapshot_images(name, Some(new.image.as_str()));
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

/// `container export` deletes an existing output directory
/// (apple/container#2325) — refuse before it can.
fn check_export_output(o: &Path) -> Result<()> {
    if o.is_dir() {
        bail!("export output `{}` is an existing directory", o.display());
    }
    Ok(())
}

pub fn export(name: &str, output: Option<&Path>) -> Result<()> {
    ensure_started()?;
    find(name)?;
    if let Some(o) = output {
        check_export_output(o)?;
    }
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
    // Checked here too so a staged distro doesn't orphan the image
    // `load_rootfs` is about to create; `create_from_spec` re-checks
    // under the distro lock.
    bail_if_staged(name)?;
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

    // ---- guest-side script tests: real `container run` invocations ----

    /// libtest captures stdout/stderr and hides it for passing tests, so
    /// a skip notice must bypass it — write straight to /dev/stderr.
    fn skip(test: &str, why: &str) {
        use std::io::Write;
        if let Ok(mut e) = fs::OpenOptions::new().write(true).open("/dev/stderr") {
            let _ = writeln!(e, "SKIP {test}: {why}");
        }
    }

    /// Image for the guest-script tests; `DISTRO_TEST_IMAGE` overrides
    /// the default. None when the service or image isn't available —
    /// tests then skip (with a notice) and never pull.
    fn test_image(test: &str) -> Option<String> {
        if !container::system_running() {
            skip(test, "container service not running");
            return None;
        }
        let image = env::var("DISTRO_TEST_IMAGE").unwrap_or_else(|_| "alpine:latest".to_string());
        let out = container_cmd()
            .args(["image", "list", "--quiet"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let suffix = format!("/{image}");
        if String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .any(|r| r == image || r.ends_with(&suffix))
        {
            Some(image)
        } else {
            skip(
                test,
                &format!("image {image} not present (set DISTRO_TEST_IMAGE)"),
            );
            None
        }
    }

    /// Run `sh ARGV...` in a throwaway container with the init assets
    /// mounted read-only at their production `/sbin.distro` path.
    /// Returns the exit code, or None if the run failed to start.
    fn sh_in_guest(image: &str, argv: &[&str]) -> Option<i32> {
        sh_in_guest_dir(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets")),
            image,
            argv,
        )
    }

    /// Like [`sh_in_guest`] but mounts `dir`, so tests can assemble a
    /// restricted init-assets flavor (no `grant-admin.sh`).
    fn sh_in_guest_dir(dir: &Path, image: &str, argv: &[&str]) -> Option<i32> {
        container_cmd()
            .args([
                "run",
                "--rm",
                "--entrypoint",
                "/bin/sh",
                "--volume",
                &format!("{}:{INIT_DIR}:ro", dir.display()),
            ])
            .arg(image)
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .ok()?
            .code()
    }

    /// Every unsafe CONTAINER_* value must fail validation before any
    /// /etc file is touched.
    #[test]
    fn create_user_rejects_unsafe_env() {
        let Some(image) = test_image("create_user_rejects_unsafe_env") else {
            return;
        };
        let script = r#"
            s=/sbin.distro/create-user.sh
            rc=0
            for u in '' '-evil' '.hidden' 'bad name' 'x:y' 'a/b' 'u$'; do
                if CONTAINER_USER=$u CONTAINER_UID=501 CONTAINER_GID=20 \
                   CONTAINER_HOME=/home/u $s 2>/dev/null; then rc=1; fi
            done
            for v in '' x '1.2' '501;rm' ' 1'; do
                if CONTAINER_USER=u CONTAINER_UID=$v CONTAINER_GID=20 \
                   CONTAINER_HOME=/home/u $s 2>/dev/null; then rc=1; fi
                if CONTAINER_USER=u CONTAINER_UID=501 CONTAINER_GID=$v \
                   CONTAINER_HOME=/home/u $s 2>/dev/null; then rc=1; fi
            done
            for h in '' relative '/home/../etc' '/x/..' '../y' '/a b' '/x:y'; do
                if CONTAINER_USER=u CONTAINER_UID=501 CONTAINER_GID=20 \
                   CONTAINER_HOME=$h $s 2>/dev/null; then rc=1; fi
            done
            exit $rc
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    /// Re-provisioning must converge: admin edits and deletions survive,
    /// no duplicate entries appear, and a changed CONTAINER_USER still
    /// gets provisioned.
    #[test]
    fn create_user_is_idempotent() {
        let Some(image) = test_image("create_user_is_idempotent") else {
            return;
        };
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne CONTAINER_ADMIN=1
            mkdir -p /etc/doas.d
            /sbin.distro/init -u
            grep -q '^daphne:x:501:20::/home/daphne:' /etc/passwd
            grep -q '^daphne ' /etc/sudoers.d/daphne
            grep -q 'daphne' /etc/doas.d/daphne.conf
            [ -f /etc/.distro.user.daphne ]
            [ -f /etc/.distro.admin.daphne ]
            # An admin removal survives re-provisioning (sentinel gate).
            rm /etc/sudoers.d/daphne /etc/doas.d/daphne.conf
            /sbin.distro/init -u
            [ ! -e /etc/sudoers.d/daphne ]
            [ ! -e /etc/doas.d/daphne.conf ]
            [ "$(grep -c '^daphne:' /etc/passwd)" = 1 ]
            # A different CONTAINER_USER is still provisioned.
            CONTAINER_USER=other CONTAINER_UID=502 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/other /sbin.distro/init -u
            grep -q '^other:x:502:20::/home/other:' /etc/passwd
            grep -q '^other ' /etc/sudoers.d/other
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    /// `init -u` runs on every boot; create-user.sh idempotency means
    /// admin edits persist while provisioning still converges.
    #[test]
    fn init_u_converges() {
        let Some(image) = test_image("init_u_converges") else {
            return;
        };
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne CONTAINER_ADMIN=1
            /sbin.distro/init -u
            [ -f /etc/.distro.initialized ]
            rm /etc/sudoers.d/daphne
            /sbin.distro/init -u
            [ ! -e /etc/sudoers.d/daphne ]
            CONTAINER_USER=other CONTAINER_UID=502 /sbin.distro/init -u
            grep -q '^other:' /etc/passwd
            [ -f /etc/.distro.user.other ]
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    /// A restricted distro mounts an assets dir without grant-admin.sh:
    /// `init -u` provisions the account but no privilege files exist.
    #[test]
    fn restricted_boot_grants_nothing() {
        let Some(image) = test_image("restricted_boot_grants_nothing") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        for f in ["init", "create-user.sh"] {
            fs::copy(
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets")).join(f),
                dir.path().join(f),
            )
            .unwrap();
        }
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne
            /sbin.distro/init -u
            grep -q '^daphne:' /etc/passwd
            [ ! -e /etc/sudoers.d/daphne ]
            [ ! -e /etc/doas.d/daphne.conf ]
            [ ! -e /etc/.distro.admin.daphne ]
        "#;
        assert_eq!(
            sh_in_guest_dir(dir.path(), &image, &["-c", script]),
            Some(0)
        );
    }

    /// Without CONTAINER_ADMIN (a container created before the split,
    /// or `admin=never`) grant-admin.sh defers: no grant, no sentinel —
    /// a later armed `set --sudo` still grants.
    #[test]
    fn grant_admin_requires_env() {
        let Some(image) = test_image("grant_admin_requires_env") else {
            return;
        };
        let script = r#"
            set -e
            export CONTAINER_USER=daphne CONTAINER_UID=501 CONTAINER_GID=20 \
                CONTAINER_HOME=/home/daphne
            /sbin.distro/create-user.sh
            /sbin.distro/grant-admin.sh
            [ ! -e /etc/sudoers.d/daphne ]
            [ ! -e /etc/.distro.admin.daphne ]
            CONTAINER_ADMIN=1 /sbin.distro/grant-admin.sh
            grep -q '^daphne ' /etc/sudoers.d/daphne
            [ -f /etc/.distro.admin.daphne ]
        "#;
        assert_eq!(sh_in_guest(&image, &["-c", script]), Some(0));
    }

    #[test]
    fn export_output_rejects_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_export_output(dir.path()).is_err());
        // A missing path and an existing regular file are both fine —
        // `export` refuses only when it would clobber a directory.
        let missing = dir.path().join("out.tar");
        assert!(check_export_output(&missing).is_ok());
        let file = dir.path().join("exists.tar");
        fs::write(&file, b"").unwrap();
        assert!(check_export_output(&file).is_ok());
    }

    #[test]
    fn rootfs_override_patches_runtime_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("runtime-configuration.json"),
            r#"{"options":{"autoRemove":true},"containerConfiguration":{"id":"d1"}}"#,
        )
        .unwrap();
        set_rootfs_override(dir.path(), Path::new("/keep/rootfs.ext4")).unwrap();
        let v: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.path().join("runtime-configuration.json")).unwrap(),
        )
        .unwrap();
        let o = &v["options"]["rootFsOverride"];
        assert_eq!(o["source"], "/keep/rootfs.ext4");
        assert_eq!(o["destination"], "/");
        assert_eq!(o["type"]["block"]["format"], "ext4");
        // Existing options survive the patch.
        assert_eq!(v["options"]["autoRemove"], true);
        assert_eq!(v["containerConfiguration"]["id"], "d1");
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
