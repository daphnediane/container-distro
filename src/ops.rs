/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` operations, composed from the `container` CLI.

use std::env;
use std::ffi::CString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::container::{
    self, ArgvMode, ContainerInfo, Machine, MachineDetail, container_cmd, default_machine_name,
    ensure_started, validate_name,
};
use crate::naming::{label_key, label_lookup, state_dir};
use crate::oci;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::plugin;

use crate::spec::{
    Automount, DistroSpec, HomeMount, HostUser, INIT_DIR, LABEL_DISTRO, LABEL_EXPORT_SCRATCH,
    LABEL_HOME_MOUNT, LABEL_IMPORTED_FROM, LABEL_MACHINE, LABEL_ROOTFS_SHA256, MountSpec,
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
    /// /mnt/<drive>); `--automount ro` mounts read-only. Hidden,
    /// Apple-private (com.apple.*), and unreadable volumes are skipped.
    /// Re-resolved on `set` and when a stopped distro starts, so
    /// attaching or ejecting a volume needs no manual fix-up
    #[arg(long, value_enum, num_args = 0..=1, require_equals = true,
          default_missing_value = "rw", value_name = "rw|ro|none")]
    pub automount: Option<Automount>,

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

/// The init-assets flavor's directory name. The restricted flavor lacks
/// `grant-admin.sh`, so restricted guests have no privilege-granting
/// code at all.
pub(crate) fn assets_flavor(admin: bool) -> &'static str {
    if admin {
        "sbin.distro"
    } else {
        "sbin.distro.restricted"
    }
}

/// The files an `admin` flavor carries: `(name, embedded body)` pairs.
fn asset_files(admin: bool) -> Vec<(&'static str, &'static str)> {
    let mut files = vec![
        ("init", INIT_SCRIPT),
        ("create-user.sh", CREATE_USER_SCRIPT),
    ];
    if admin {
        files.push(("grant-admin.sh", GRANT_ADMIN_SCRIPT));
    }
    files
}

/// Set or clear the macOS user-immutable flag on `path`.
///
/// virtiofs exposes no flag operations, so `uchg` is a real boundary
/// against a guest rewriting these files through a rw-shared home:
/// writes, unlinks, and renames all fail with EPERM until a host
/// process clears the flag (verified against a running distro).
fn set_locked(path: &Path, locked: bool) -> Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c` is a valid NUL-terminated path; chflags only reads it.
    let rc = unsafe { libc::chflags(c.as_ptr(), if locked { libc::UF_IMMUTABLE } else { 0 }) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
            .with_context(|| format!("failed to update flags on {}", path.display()))
    }
}

/// Clear the immutable flag on `path` if it exists — a no-op on
/// symlinks (lchmod, which chflags maps to here, won't follow them) and
/// anything already unlocked. Ignores errors: callers treat it as
/// best-effort prep before a write or remove that will surface real
/// failures itself.
pub(crate) fn unlock(path: &Path) {
    if fs::symlink_metadata(path).is_ok() {
        let _ = set_locked(path, false);
    }
}

/// Write `body` to `path` via `O_NOFOLLOW`, so a symlink planted
/// through a shared-home mount is removed rather than followed — the
/// alternative is a refresh clobbering an arbitrary file the user owns.
fn write_nofollow(path: &Path, body: &[u8]) -> Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    for _ in 0..2 {
        // SAFETY: `c` is a valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW,
                0o755,
            )
        };
        if fd >= 0 {
            // SAFETY: fd is a live descriptor open() just returned.
            let mut f = unsafe { fs::File::from_raw_fd(fd) };
            return f
                .write_all(body)
                .with_context(|| format!("failed to write {}", path.display()));
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ELOOP)
            && fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
        {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove planted link {}", path.display()))?;
            continue;
        }
        return Err(e).with_context(|| format!("failed to write {}", path.display()));
    }
    unreachable!("the ELOOP retry runs at most once")
}

/// Write the init assets into `dir`, refreshing them if this build's
/// copies differ, then lock the directory and its files (`uchg`).
/// `admin` selects the flavor: only admin distros get `grant-admin.sh`.
///
/// The lock is what keeps a guest from rewriting these files through a
/// rw-shared home (`~/Library/Application Support` is inside it), so a
/// re-run unlocks the dir and each file before writing and re-locks
/// after. A failure mid-write leaves the dir unlocked — no worse than
/// never locking.
pub(crate) fn write_assets(dir: &Path, admin: bool) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    unlock(dir);
    if !admin {
        // No stray grant script may linger in the restricted flavor.
        let stray = dir.join("grant-admin.sh");
        unlock(&stray);
        match fs::remove_file(&stray) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            r => r.with_context(|| format!("failed to clean {}", dir.display()))?,
        }
    }
    for (name, body) in asset_files(admin) {
        let path = dir.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove planted link {}", path.display()))?;
        }
        unlock(&path);
        if fs::read(&path).ok().as_deref() != Some(body.as_bytes()) {
            write_nofollow(&path, body.as_bytes())?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        set_locked(&path, true)?;
    }
    set_locked(dir, true)
}

/// Refresh both init-assets flavors in the per-user state dir, and
/// re-lock the default-distro marker if one exists (it's only locked
/// when written, so an older build may have left it open).
fn refresh_assets() -> Result<()> {
    let state = state_dir()?;
    write_assets(&state.join(assets_flavor(true)), true)?;
    write_assets(&state.join(assets_flavor(false)), false)?;
    let default = state.join("default-distro");
    if default.exists() {
        set_locked(&default, true)?;
    }
    Ok(())
}

/// Whether `dir` holds exactly this build's assets — installed copies
/// are only trusted when they match what this binary would write.
fn assets_match(dir: &Path, admin: bool) -> bool {
    dir.is_dir()
        && asset_files(admin)
            .iter()
            .all(|(n, b)| fs::read(dir.join(n)).ok().as_deref() == Some(b.as_bytes()))
        && (admin || !dir.join("grant-admin.sh").exists())
}

/// The init assets `install-plugin` wrote next to the plugin binary
/// (`<prefix>/libexec/container-plugins/distro/`), if they match this
/// build byte-for-byte. They live outside `$HOME` — root-owned for a
/// standard install — so no home share can reach them. A stale copy
/// warns once and falls back to the per-user assets: a newer build's
/// fixes should land rather than keep running an old init.
fn installed_assets(admin: bool) -> Option<PathBuf> {
    let dir = plugin::plugin_dir(None).ok()?.join(assets_flavor(admin));
    if !dir.is_dir() {
        return None;
    }
    if assets_match(&dir, admin) {
        return Some(dir);
    }
    static WARN: std::sync::Once = std::sync::Once::new();
    WARN.call_once(|| {
        eprintln!(
            "container-distro: warning: the installed init assets differ from this build — \
             using per-user copies; re-run `container-distro install-plugin` to update"
        );
    });
    None
}

/// The host directory a distro mounts at `/sbin.distro`: the installed
/// copy when it is current, else the refreshed per-user copy.
fn assets_dir(admin: bool) -> Result<PathBuf> {
    if let Some(dir) = installed_assets(admin) {
        return Ok(dir);
    }
    refresh_assets()?;
    Ok(state_dir()?.join(assets_flavor(admin)))
}

/// Refresh the init assets the distro will actually mount at boot.
///
/// A per-user mount source is rewritten in place (upgrading to this
/// build and re-locking). An installed mount source can't be rewritten
/// from here — it is only ever stale, so a drifted `init` warns once;
/// `set` recreates onto per-user assets, or `install-plugin` refreshes
/// the installed copy.
fn refresh_mounted_assets(name: &str) -> Result<()> {
    ensure_preserved_locked()?;
    let mounted = find(name).ok().and_then(|info| {
        info.configuration
            .mounts
            .iter()
            .find(|m| m.destination == INIT_DIR)
            .map(|m| PathBuf::from(&m.source))
    });
    match mounted {
        Some(src) if !src.starts_with(state_dir()?) => {
            if fs::read(src.join("init")).ok().as_deref() != Some(INIT_SCRIPT.as_bytes()) {
                static WARN: std::sync::Once = std::sync::Once::new();
                WARN.call_once(|| {
                    eprintln!(
                        "container-distro: warning: `{name}`'s init assets differ from this build \
                         ({}) — re-run install-plugin or `set` the distro",
                        src.display()
                    );
                });
            }
            Ok(())
        }
        _ => refresh_assets(),
    }
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
            // Locked at rest like the init assets: this file picks which
            // distro a bare `cm` enters, so it shouldn't be guest-writable.
            unlock(&path);
            write_nofollow(&path, format!("{n}\n").as_bytes())?;
            set_locked(&path, true)?;
        }
        None => {
            unlock(&path);
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

/// The mounts `--automount <mode>` would add right now: every readable
/// `/Volumes` entry at its canonical `/mnt` target, skipping targets
/// already in `taken` (e.g. user mounts).
fn scan_automounts(
    mode: Automount,
    taken: &std::collections::HashSet<String>,
) -> Result<Vec<MountSpec>> {
    let entries: Vec<(String, bool)> = fs::read_dir("/Volumes")
        .context("failed to read /Volumes")?
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_link = e.file_type().is_ok_and(|t| t.is_symlink());
            (e.file_name().to_string_lossy().into_owned(), is_link)
        })
        .collect();
    let mut taken = taken.clone();
    let mut mounts = Vec::new();
    for m in automounts(&entries, mode.read_only()) {
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
    Ok(mounts)
}

/// Sync `spec`'s automounts with the currently attached `/Volumes`:
/// canonical automounts are dropped and re-resolved; everything else —
/// including a user's `ro` or differently-targeted `/Volumes` mount —
/// is kept. Used by `set` and by start-time refresh.
fn reconcile_automounts(spec: &mut DistroSpec) -> Result<()> {
    spec.mounts.retain(|m| !crate::spec::is_automount(m));
    let taken = spec.mounts.iter().map(|m| m.target.clone()).collect();
    spec.mounts.extend(scan_automounts(spec.automount, &taken)?);
    Ok(())
}

/// The mounts requested by create options: explicit `-v`s plus
/// `--automount` resolutions (readable volumes only, no target
/// collisions).
fn resolve_mounts(opts: &CreateOptions) -> Result<Vec<MountSpec>> {
    let mut mounts = opts.volumes.clone();
    let automount = opts.automount.unwrap_or_default();
    if automount.enabled() {
        let taken = mounts.iter().map(|x| x.target.clone()).collect();
        mounts.extend(scan_automounts(automount, &taken)?);
    }
    Ok(mounts)
}

fn build_spec(name: String, image: String, opts: &CreateOptions) -> Result<DistroSpec> {
    validate_name(&name)?;
    let mounts = resolve_mounts(opts)?;
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
        automount: opts.automount.unwrap_or_default(),
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
    warn_public_publish(&spec.publish);
    Ok(spec)
}

/// Warn once per publish spec that binds a non-loopback address — the
/// flag that changes exposure for *other* machines on the LAN.
fn warn_public_publish(specs: &[PublishSpec]) {
    for p in specs.iter().filter(|p| !p.is_loopback()) {
        eprintln!(
            "container-distro: warning: publishing on {}:{} — reachable beyond localhost (LAN); use 127.0.0.1 to bind locally",
            p.host_ip, p.host_port
        );
    }
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
    refresh_mounted_assets(name)?;
    run_container_quiet(&["start", name])?;
    let init = format!("{INIT_DIR}/init");
    run_container(&["exec", "--user", "0:0", name, &init, "-u"])
        .with_context(|| format!("user setup failed in `{name}`"))?;
    report_ports(name);
    Ok(())
}

/// Report the ports `name` publishes, now that it is listening —
/// loopback binds as info, anything wider as a LAN-exposure warning.
fn report_ports(name: &str) {
    let Ok(info) = find(name) else { return };
    let Ok(spec) = DistroSpec::from_container(&info, &host_home().unwrap_or_default()) else {
        return;
    };
    for p in &spec.publish {
        if p.is_loopback() {
            println!("`{name}` listening on {}:{}", p.host_ip, p.host_port);
        } else {
            eprintln!(
                "container-distro: warning: `{name}` listening on {}:{} — reachable beyond localhost (LAN)",
                p.host_ip, p.host_port
            );
        }
    }
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
    refresh_automounts(&info, &lock)?;
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
    let info = find(name)?;
    if !info.is_running() {
        refresh_automounts(&info, &lock)?;
    }
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
    if preserved_dir().is_err() {
        return false;
    }
    let mut removed = false;
    let _ = with_preserved_unlocked(|| {
        for p in [preserved_rootfs(name), preserved_journal(name)]
            .into_iter()
            .flatten()
        {
            unlock(&p);
            removed |= fs::remove_file(&p).is_ok();
        }
        Ok(())
    });
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
        delete_stopped_locked(n, &lock)?;
    }
    Ok(())
}

/// Delete a stopped distro under its lock: the container, its snapshot
/// images, anything `set` staged, and the default marker.
fn delete_stopped_locked(n: &str, lock: &DistroLock) -> Result<()> {
    run_container_quiet(&["delete", n])?;
    remove_snapshot_images(n, None);
    cleanup_preserved(n, lock);
    if default_name().as_deref() == Some(n) {
        write_default(None)?;
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

/// Whether the image belongs to distro `name`: the `distro` label
/// `import` sets (C11 — older unlabeled images are left for
/// `container image prune`).
fn is_our_image(image: &container::ImageListEntry, name: &str) -> bool {
    label_lookup(&image.labels(), LABEL_DISTRO).is_some_and(|v| v == name)
}

/// Best-effort removal of `name`'s imported images, except `keep`.
fn remove_snapshot_images(name: &str, keep: Option<&str>) {
    let Some(images) = container::list_images() else {
        return;
    };
    for image in &images {
        let short = image
            .configuration
            .name
            .strip_prefix("docker.io/")
            .unwrap_or(&image.configuration.name);
        if is_our_image(image, name) && Some(short) != keep {
            let _ = container_cmd()
                .args(["image", "delete", &image.configuration.name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Refuse to create `name` while an interrupted `set`/`migrate` has its
/// filesystem staged — the user must choose to recover (`set`) or
/// discard (`rm`) it first.
fn bail_if_staged(name: &str) -> Result<()> {
    if let Ok(staged) = preserved_rootfs(name)
        && staged.exists()
    {
        bail!(
            "an interrupted `set`/`migrate` left `{0}`'s filesystem staged — \
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

/// Keep `preserved/` present and `uchg`-locked. Through a rw-shared
/// home a guest could otherwise plant a staged filesystem + journal
/// pair that `recover_interrupted` would then trust and recreate from
/// — a confused-deputy path to mounts nobody asked for. Staging and
/// restore ops unlock the dir briefly around their file ops instead.
fn ensure_preserved_locked() -> Result<()> {
    let dir = preserved_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    set_locked(&dir, true)
}

/// Run `f` with `preserved/` unlocked, re-locking it afterwards — even
/// on error, where the operation's own failure is the one reported.
fn with_preserved_unlocked<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = preserved_dir()?;
    unlock(&dir);
    let r = f();
    let relock = set_locked(&dir, true);
    match r {
        Err(e) => Err(e),
        Ok(v) => relock.map(|_| v),
    }
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

/// Journal `spec` and clone `src` (a container's or machine's ext4
/// rootfs) into `preserved/`, for a later [`restore_rootfs`] to move
/// into a fresh container.
///
/// The journal is written before the filesystem so a staged filesystem
/// is never left without the spec needed to recreate its distro. Both
/// are `uchg`-locked once written — through a shared home, a guest
/// could otherwise rewrite the spec `recover_interrupted` recreates
/// from or the filesystem it puts back.
fn stage_rootfs(id: &str, spec: &DistroSpec, src: &Path, _lock: &DistroLock) -> Result<PathBuf> {
    let dir = preserved_dir()?;
    fs::create_dir_all(&dir)?;
    let staged = dir.join(format!("{id}.ext4"));
    if staged.exists() {
        bail!(
            "an earlier interrupted operation left {} — remove it (or `container distro rm {id}`) before retrying",
            staged.display()
        );
    }
    let journal = dir.join(format!("{id}.json"));
    with_preserved_unlocked(|| {
        unlock(&journal);
        write_nofollow(&journal, &serde_json::to_vec(spec)?)?;
        set_locked(&journal, true)?;
        // Copy (`fs::copy` uses clonefile on APFS) rather than move: the
        // original stays in place, so an interruption leaves a bootable
        // source plus a spare.
        fs::copy(src, &staged).with_context(|| format!("failed to preserve {}", src.display()))?;
        set_locked(&staged, true)
    })?;
    Ok(staged)
}

/// Move a container's `rootfs.ext4` out of its data directory so
/// `container delete` doesn't take it with it, and write a journal of
/// `spec` next to it. The file only exists once the container has booted
/// — `None` for a never-started distro, whose filesystem is still the
/// pristine image (and which `container export` cannot snapshot for the
/// same reason).
fn preserve_rootfs(id: &str, spec: &DistroSpec, lock: &DistroLock) -> Result<Option<PathBuf>> {
    let rootfs = container_dir(id)?.join("rootfs.ext4");
    if !rootfs.exists() {
        return Ok(None);
    }
    stage_rootfs(id, spec, &rootfs, lock).map(Some)
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
    let journal = preserved_journal(id)?;
    with_preserved_unlocked(|| {
        unlock(staged);
        fs::rename(staged, &rootfs).with_context(|| {
            format!(
                "failed to restore {} — the preserved copy is at {}",
                rootfs.display(),
                staged.display()
            )
        })?;
        unlock(&journal);
        let _ = fs::remove_file(&journal);
        Ok(())
    })?;
    set_rootfs_override(&dir, &rootfs)?;
    Ok(())
}

/// Complete an interrupted `set` for `name`, if one left a filesystem
/// staged under `preserved/`. The container missing means the recreate
/// never happened, so the journaled spec rebuilds it; the container
/// present just needs its rootfs back — unless it already has one, in
/// which case the staged copy is kept (it may be someone's only data).
fn recover_interrupted(name: &str, _lock: &DistroLock) -> Result<()> {
    ensure_preserved_locked()?;
    let staged = preserved_rootfs(name)?;
    let journal = preserved_journal(name)?;
    if !staged.exists() {
        // A journal with no filesystem is a stale marker.
        let _ = with_preserved_unlocked(|| {
            unlock(&journal);
            fs::remove_file(&journal).map_err(Into::into)
        });
        return Ok(());
    }
    eprintln!(
        "warning: an interrupted `set`/`migrate` staged `{name}`'s filesystem — restoring it"
    );
    if !container_dir(name)?.is_dir() {
        let spec: DistroSpec = serde_json::from_slice(&fs::read(&journal).with_context(|| {
            format!(
                "`{name}` was deleted by an interrupted `set` and its journal is unreadable; \
                 its filesystem is preserved at {}",
                staged.display()
            )
        })?)
        .context("invalid `set` journal")?;
        if spec.name != name {
            bail!(
                "`set` journal for `{name}` names `{}` — refusing to recreate",
                spec.name
            );
        }
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
        with_preserved_unlocked(|| {
            unlock(&staged);
            fs::rename(&staged, &rootfs)
                .with_context(|| format!("failed to restore {}", staged.display()))?;
            unlock(&journal);
            let _ = fs::remove_file(&journal);
            Ok(())
        })?;
        set_rootfs_override(&dir, &rootfs)?;
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

/// Recreate `name`'s container with `new` settings, keeping its
/// filesystem: the ext4 rootfs is cloned aside, the container deleted
/// and recreated, then the filesystem moved back via `rootFsOverride`.
/// A running distro is stopped first and rebooted. On failure the old
/// settings are restored onto the preserved filesystem. Shared by `set`
/// and the start-time automount refresh.
fn recreate_locked(
    name: &str,
    old: &DistroSpec,
    new: &DistroSpec,
    was_running: bool,
    lock: &DistroLock,
) -> Result<()> {
    if was_running {
        run_container_quiet(&["stop", name])?;
    }
    let preserved = preserve_rootfs(name, old, lock)?;
    run_container_quiet(&["delete", name])?;
    if let Err(e) =
        create_container(new).and_then(|()| restore_rootfs(name, preserved.as_deref(), lock))
    {
        // Restore the old settings on the preserved rootfs; nothing is lost.
        let _ = run_container_quiet(&["delete", name]);
        let rollback =
            create_container(old).and_then(|()| restore_rootfs(name, preserved.as_deref(), lock));
        if let Err(r) = rollback {
            return Err(e.context(format!(
                "failed to recreate the distro; restoring the previous settings failed too: {r:#}"
            )));
        }
        if was_running {
            boot(name, lock)?;
        }
        return Err(e.context("failed to recreate the distro; previous settings restored"));
    }
    if was_running {
        boot(name, lock)?;
    }
    remove_snapshot_images(name, Some(new.image.as_str()));
    Ok(())
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
    let mut new = changes.apply(&old)?;
    warn_public_publish(&changes.add_publish);
    // An enabled automount is always reconciled, not just when
    // `--automount` was passed — the recreate happens anyway.
    if new.automount.enabled() {
        reconcile_automounts(&mut new)?;
    }
    check_sources(&new)?;
    recreate_locked(name, &old, &new, info.is_running(), &lock)
}

/// Re-resolve `info`'s automounts against the currently attached
/// `/Volumes`, recreating the distro when they changed — an attached or
/// ejected volume shouldn't need a manual `set` first. Only runs for
/// stopped distros that opted in; no-op for anything else.
fn refresh_automounts(info: &ContainerInfo, lock: &DistroLock) -> Result<()> {
    let Ok(home) = host_home() else { return Ok(()) };
    let Ok(spec) = DistroSpec::from_container(info, &home) else {
        return Ok(());
    };
    if !spec.automount.enabled() {
        return Ok(());
    }
    let mut new = spec.clone();
    reconcile_automounts(&mut new)?;
    if new.mounts == spec.mounts {
        return Ok(());
    }
    eprintln!(
        "container-distro: refreshing `{}`'s automounts for the attached /Volumes",
        spec.name
    );
    recreate_locked(&spec.name, &spec, &new, false, lock)
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

/// Where an imported rootfs came from, recorded in the loaded image's
/// labels (`imported-from`, plus `rootfs-sha256` for downloads).
#[derive(Debug, Clone, Copy)]
pub enum ImportSource<'a> {
    /// A local file path (or `-` for stdin).
    Local,
    /// A catalog `.wsl` download: the URL and the SHA-256 it was
    /// verified against.
    Download { url: &'a str, sha256: &'a str },
}

/// Create a distro from a rootfs tar; returns its name.
pub fn import(
    name: &str,
    file: &Path,
    opts: &CreateOptions,
    source: ImportSource<'_>,
) -> Result<String> {
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
    // Labels mark the image as ours and record where it came from —
    // cleanup and debugging don't have to guess from the name. For a
    // catalog download that's the URL plus the verified hash; the cache
    // path would be noise.
    let mut labels = vec![(label_key(LABEL_DISTRO), name.to_string())];
    match source {
        ImportSource::Local => labels.push((
            label_key(LABEL_IMPORTED_FROM),
            if file == Path::new("-") {
                "-".to_string()
            } else {
                file.to_string_lossy().into_owned()
            },
        )),
        ImportSource::Download { url, sha256 } => {
            labels.push((label_key(LABEL_IMPORTED_FROM), url.to_string()));
            labels.push((label_key(LABEL_ROOTFS_SHA256), sha256.to_string()));
        }
    }
    oci::load_rootfs(rootfs, &reference, &labels)?;
    let spec = build_spec(name.to_string(), reference, opts)?;
    create_from_spec(&spec, opts.no_boot, opts.set_default)
}

/// A scratch container created for a machine export — deleted on drop
/// so a failed export can't leak it. (A SIGKILL still can; the
/// `export-scratch` label identifies leftovers in `container list`.)
struct ScratchContainer(String);

impl ScratchContainer {
    fn create(name: &str, image: &str, machine: &str) -> Result<Self> {
        let label = format!("{}={machine}", label_key(LABEL_EXPORT_SCRATCH));
        // --entrypoint only satisfies `create`'s requirement that a
        // command be specified even for images with no Cmd — the
        // scratch container is never started.
        run_container_quiet(&[
            "create",
            "--name",
            name,
            "--label",
            &label,
            "--entrypoint",
            "/bin/sh",
            image,
        ])
        .context("failed to create the export scratch container")?;
        Ok(Self(name.to_string()))
    }
}

impl Drop for ScratchContainer {
    fn drop(&mut self) {
        let _ = container_cmd()
            .args(["delete", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The scratch container's name: the machine's (truncated to fit the
/// 63-char `container` limit with the `-export-<timestamp>` suffix) so
/// a leftover is recognizable.
fn export_scratch_name(machine: &str) -> String {
    let base: String = machine.chars().take(44).collect();
    format!("{}-export-{}", base.trim_end_matches('-'), timestamp())
}

/// The image a scratch export container is created from: the machine's
/// own image when still local, else any local image — the container is
/// never booted, so the image only has to exist. Fails when the daemon
/// holds no images at all.
fn scratch_image(detail: &MachineDetail) -> Result<String> {
    let images = container::list_images().unwrap_or_default();
    let want = detail.image.as_ref().map(|i| i.reference.as_str());
    let is = |n: &str| {
        want.is_some_and(|w| {
            n == w
                || n.strip_prefix("docker.io/") == Some(w)
                || n.strip_prefix("docker.io/library/") == Some(w)
        })
    };
    if let Some(i) = images.iter().find(|i| is(&i.configuration.name)) {
        return Ok(i.configuration.name.clone());
    }
    images
        .first()
        .map(|i| i.configuration.name.clone())
        .context("no local images to create the export scratch container from")
}

/// `cm --export` for a machine. `container export` snapshots
/// `containers/<id>/rootfs.ext4` literally — a machine's backing
/// container mounts its disk from plugin state instead, so export
/// can't see it. The workaround: clone the machine's ext4 into a
/// scratch container that exports normally, then delete it (the
/// machine → distro `migrate` path uses the same clone mechanism).
/// The machine is stopped while its disk is cloned — a running
/// machine's clone would only be crash-consistent — and restarted if
/// it was running.
pub fn export_machine(machine: &str, output: Option<&Path>) -> Result<()> {
    ensure_started()?;
    // Reads and writes the daemon's containers/ directory internals.
    container::warn_unverified_version();
    let detail = container::inspect_machine(machine)?;
    if let Some(o) = output {
        check_export_output(o)?;
    }
    let scratch = ScratchContainer::create(
        &export_scratch_name(machine),
        &scratch_image(&detail)?,
        machine,
    )?;
    let was_running = detail.is_running();
    let cloned = (|| -> Result<()> {
        if was_running {
            run_container_quiet(&["machine", "stop", machine])?;
        }
        let src = machine_rootfs(&detail)?
            .with_context(|| format!("machine `{machine}` has no root filesystem to export"))?;
        // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
        fs::copy(&src, container_dir(&scratch.0)?.join("rootfs.ext4"))
            .with_context(|| format!("failed to clone {}", src.display()))?;
        Ok(())
    })();
    if was_running && let Err(e) = boot_machine(machine) {
        eprintln!("container-distro: warning: failed to restart machine `{machine}`: {e:#}");
    }
    cloned?;
    let mut args = vec!["export".to_string(), scratch.0.clone()];
    if let Some(o) = output {
        args.push("--output".into());
        args.push(o.to_string_lossy().into_owned());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container(&args)
}

/// `cm --import` for a machine: wrap the rootfs tar as an OCI image
/// (`oci::build_layout`), `image load` it, and `machine create` from
/// it. A boot probe then retries the documented first-boot flake — the
/// first `machine run` after `machine create` can lose a race while the
/// VM restarts after provisioning, for stock and imported images alike
/// — and leaves the machine stopped, like `wsl --import`.
pub fn import_machine(name: &str, file: &Path) -> Result<()> {
    validate_name(name)?;
    ensure_started()?;
    if container::list_machines()?.iter().any(|m| m.id == name) {
        bail!("a machine named `{name}` already exists");
    }
    // A same-named distro would shadow the machine everywhere in `cm`.
    if find(name).is_ok() {
        bail!(
            "`{name}` is a distro — `cm` resolves that name to the distro; \
             pick another name, or use `cm --import --distro`"
        );
    }
    let stdin_copy;
    let rootfs = if file == Path::new("-") {
        stdin_copy = oci::stdin_to_tempfile()?;
        stdin_copy.path()
    } else {
        file
    };
    let reference = format!("local/machine-{name}:imported-{}", timestamp());
    // Labels mark the image as ours without `distro`, which would make
    // `distro rm`/`set` image cleanup claim it.
    let labels = [
        (label_key(LABEL_MACHINE), name.to_string()),
        (
            label_key(LABEL_IMPORTED_FROM),
            if file == Path::new("-") {
                "-".to_string()
            } else {
                file.to_string_lossy().into_owned()
            },
        ),
    ];
    oci::load_rootfs(rootfs, &reference, &labels)?;
    // `machine create` boots the machine; its internal boot absorbs the
    // first-boot provisioning reboot. `--no-boot` is avoided on purpose:
    // a `machine run` on a never-booted machine races the boot and fails
    // every time (upstream #2024-adjacent).
    let created = run_container(&["machine", "create", "--name", name, &reference]);
    if created.is_err() {
        // Don't leave the just-loaded image orphaned behind the failed
        // create.
        let _ = container_cmd()
            .args(["image", "delete", &reference])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return created;
    }
    // Boot probe: the first `machine run` after `machine create` can
    // still lose the race while the VM settles — retry with a pause.
    let booted = (0..5).any(|attempt| {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        container::run_command(
            Some(name),
            None,
            None,
            &[],
            None,
            &["true".to_string()],
            ArgvMode::Shell,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    });
    if booted {
        // `wsl --import` registers without starting; leave it stopped.
        let _ = run_container_quiet(&["machine", "stop", name]);
    } else {
        eprintln!(
            "warning: `{name}` was created but did not boot; see `container machine logs {name}`"
        );
    }
    Ok(())
}

/// Migrate a `container machine` into a distro (`migrate`, `--in`),
/// carrying its root filesystem across with it. Unless `keep`, the
/// machine is removed once the distro exists. `target` renames the
/// result. Returns the distro's name.
///
/// The reverse — distro → machine — is [`migrate_to_machine`]
/// (`migrate --out`).
pub fn migrate(
    machine: &str,
    target: Option<String>,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    ensure_started()?;
    if !container::list_machines()?.iter().any(|m| m.id == machine) {
        bail!("no machine named `{machine}`");
    }
    let target = target.unwrap_or_else(|| machine.to_string());
    migrate_to_distro(machine, &target, keep, opts)
}

/// A distro spec reproducing a machine: its image, resources, home
/// mount, and provisioned account, with `opts` overrides on top
/// (`--restricted` remains a defaults preset, like at `create`).
fn spec_from_machine(name: &str, m: &MachineDetail, opts: &CreateOptions) -> Result<DistroSpec> {
    let r = opts.restricted;
    let admin = if opts.sudo {
        true
    } else if opts.no_sudo {
        false
    } else {
        !r
    };
    let user = match &m.user_setup {
        Some(u) => HostUser {
            name: u.username.clone(),
            uid: u.uid,
            gid: u.gid,
        },
        None => host_user()?,
    };
    let home_mount = match opts.home_mount {
        Some(h) => h,
        None if r => HomeMount::None,
        None => m.home_mount.as_deref().unwrap_or("rw").parse()?,
    };
    let image = m
        .image
        .as_ref()
        .map(|i| i.reference.clone())
        .with_context(|| "machine has no image reference".to_string())?;
    let spec = DistroSpec {
        name: name.to_string(),
        image,
        cpus: opts.cpus.or(m.cpus),
        memory: opts
            .memory
            .clone()
            .or_else(|| m.memory.map(|b| format!("{}M", b / (1024 * 1024)))),
        home_mount,
        mounts: resolve_mounts(opts)?,
        automount: opts.automount.unwrap_or_default(),
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
        user,
    };
    warn_public_publish(&spec.publish);
    Ok(spec)
}

/// The machine plugin's per-machine state directory.
fn machine_state_dir(name: &str) -> Result<PathBuf> {
    Ok(container::app_root()
        .context("`container system status` did not report an appRoot")?
        .join("plugin-state/machine-apiserver/machines")
        .join(name))
}

/// The `source` of a `rootFsOverride` record in a container's
/// runtime-configuration.json — where the daemon mounts its disk from.
fn rootfs_override_source(json: &str) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .pointer("/options/rootFsOverride/source")?
        .as_str()
        .map(PathBuf::from)
}

/// The `source` of a `rootfs.json`-shaped mount record.
fn rootfs_source(json: &str) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("source")?
        .as_str()
        .map(PathBuf::from)
}

/// A machine's ext4 rootfs: what its backing container mounts when
/// there is one (a machine gets a backing container only once booted),
/// else the path the machine plugin state records. `None` means no
/// materialized rootfs — unusual, and treated like a never-booted
/// distro's missing rootfs: the target starts from the image.
fn machine_rootfs(m: &MachineDetail) -> Result<Option<PathBuf>> {
    let app =
        container::app_root().context("`container system status` did not report an appRoot")?;
    if let Some(cid) = &m.container_id
        && let Some(src) = fs::read_to_string(
            app.join("containers")
                .join(cid)
                .join("runtime-configuration.json"),
        )
        .ok()
        .and_then(|s| rootfs_override_source(&s))
        && src.is_file()
    {
        return Ok(Some(src));
    }
    let dir = machine_state_dir(&m.id)?;
    if let Some(src) = fs::read_to_string(dir.join("rootfs.json"))
        .ok()
        .and_then(|s| rootfs_source(&s))
        && src.is_file()
    {
        return Ok(Some(src));
    }
    let ext4 = dir.join("rootfs.ext4");
    Ok(ext4.is_file().then_some(ext4))
}

/// Boot a machine (`machine run` is the boot primitive; there is no
/// `machine start`).
fn boot_machine(name: &str) -> Result<()> {
    run_container_quiet(&["machine", "run", "-i", "-n", name, "--", "true"])
        .with_context(|| format!("failed to boot machine `{name}`"))
}

/// Machine → distro: clone the machine's rootfs into a new distro
/// container through the same `preserved/` staging `set` uses, so an
/// interruption is finished by the usual recovery path.
fn migrate_to_distro(
    machine: &str,
    name: &str,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    validate_name(name)?;
    // Fail fast, before the machine is stopped or its disk cloned.
    bail_if_staged(name)?;
    if distros()?.iter().any(|c| c.id() == name) {
        bail!("distro `{name}` already exists");
    }
    let detail = container::inspect_machine(machine)?;
    let spec = spec_from_machine(name, &detail, opts)?;
    check_sources(&spec)?;
    let was_running = detail.is_running();
    if was_running {
        run_container_quiet(&["machine", "stop", machine])?;
    }
    let lock = DistroLock::acquire(name)?;
    container::warn_unverified_version();
    let staged = match machine_rootfs(&detail)? {
        Some(src) => Some(stage_rootfs(name, &spec, &src, &lock)?),
        None => {
            eprintln!(
                "container-distro: machine `{machine}` has no rootfs to carry — the distro starts from its image"
            );
            None
        }
    };
    if let Err(e) =
        create_container(&spec).and_then(|()| restore_rootfs(name, staged.as_deref(), &lock))
    {
        let _ = run_container_quiet(&["delete", name]);
        cleanup_preserved(name, &lock);
        if was_running {
            let _ = boot_machine(machine);
        }
        return Err(e);
    }
    if !opts.no_boot
        && let Err(e) = boot(name, &lock)
    {
        return Err(e.context(format!(
            "distro `{name}` holds the migrated filesystem but failed to boot; machine `{machine}` left stopped"
        )));
    }
    if !keep {
        if let Err(e) = run_container_quiet(&["machine", "rm", machine]) {
            eprintln!("container-distro: migrated, but removing machine `{machine}` failed: {e:#}");
        }
    } else {
        if name == machine {
            eprintln!(
                "container-distro: `{name}` is now both a machine and a distro; `cm` resolves it to the distro"
            );
        }
        if was_running {
            let _ = boot_machine(machine);
        }
    }
    if opts.set_default
        || no_default(
            default_name().as_deref(),
            &distros()?,
            &container::list_machines().unwrap_or_default(),
        )
    {
        write_default(Some(name))?;
    }
    Ok(name.to_string())
}

/// A distro's ext4 rootfs: the `rootFsOverride` source when the runtime
/// configuration records one, else the conventional
/// `containers/<id>/rootfs.ext4`. `None` before the first boot — the
/// filesystem is still the pristine image (and `container export` has
/// nothing to snapshot either).
fn distro_rootfs(id: &str) -> Result<Option<PathBuf>> {
    let dir = container_dir(id)?;
    if let Some(src) = fs::read_to_string(dir.join("runtime-configuration.json"))
        .ok()
        .and_then(|s| rootfs_override_source(&s))
        && src.is_file()
    {
        return Ok(Some(src));
    }
    let ext4 = dir.join("rootfs.ext4");
    Ok(ext4.is_file().then_some(ext4))
}

/// The `CreateOptions` `machine create` can't honor — `--out` rejects
/// them rather than silently dropping user intent.
fn reject_machine_opts(opts: &CreateOptions) -> Result<()> {
    let mut bad: Vec<&str> = Vec::new();
    if !opts.volumes.is_empty() {
        bad.push("--volume");
    }
    if opts.automount.is_some() {
        bad.push("--automount");
    }
    if !opts.publish.is_empty() {
        bad.push("--publish");
    }
    if opts.network.is_some() {
        bad.push("--network");
    }
    if opts.ssh || opts.no_ssh {
        bad.push("--ssh/--no-ssh");
    }
    if opts.sudo || opts.no_sudo {
        bad.push("--sudo/--no-sudo");
    }
    if opts.restricted {
        bad.push("--restricted");
    }
    if !bad.is_empty() {
        bail!(
            "{} cannot be applied to a machine (supported overrides: --cpus, --memory, --home-mount)",
            bad.join(", ")
        );
    }
    Ok(())
}

/// The image to `machine create` from when migrating a distro out: the
/// distro's own when it is still local. When it is gone and a
/// filesystem is being carried, any local image does — the rootfs is
/// replaced right after the first boot. Without a filesystem to carry
/// the image is the machine's whole content, so the distro's reference
/// is passed through for `machine create` to pull.
fn machine_create_image(spec_image: &str, carrying: bool) -> Result<String> {
    let images = container::list_images().unwrap_or_default();
    let is = |n: &str| {
        n == spec_image
            || n.strip_prefix("docker.io/") == Some(spec_image)
            || n.strip_prefix("docker.io/library/") == Some(spec_image)
    };
    if let Some(i) = images.iter().find(|i| is(&i.configuration.name)) {
        return Ok(i.configuration.name.clone());
    }
    if !carrying {
        return Ok(spec_image.to_string());
    }
    images
        .first()
        .map(|i| {
            eprintln!(
                "container-distro: image `{spec_image}` is no longer local; creating the machine from {}",
                i.configuration.name
            );
            i.configuration.name.clone()
        })
        .context("no local images to create the machine from")
}

/// `container machine create` for a distro migrating out: its
/// cpus/memory/home-mount with `opts` overrides on top. The machine
/// boots once — its plugin-state rootfs only materializes then — and is
/// stopped by the caller for the filesystem swap.
fn create_machine(name: &str, image: &str, spec: &DistroSpec, opts: &CreateOptions) -> Result<()> {
    let mut a: Vec<String> = ["machine", "create", "--name", name]
        .map(String::from)
        .to_vec();
    if let Some(c) = opts.cpus.or(spec.cpus) {
        a.extend(["--cpus".into(), c.to_string()]);
    }
    if let Some(m) = opts.memory.clone().or_else(|| spec.memory.clone()) {
        a.extend(["--memory".into(), m]);
    }
    a.extend([
        "--home-mount".into(),
        opts.home_mount.unwrap_or(spec.home_mount).as_str().into(),
    ]);
    a.push(image.to_string());
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    run_container(&a)
}

/// Migrate a distro into a `container machine` (`migrate --out`), the
/// mirror of [`migrate_to_distro`]: create the machine from the distro's
/// image — its internal boot materializes the plugin-state disk — stop
/// it, then clone the distro's `rootfs.ext4` over the machine's. No
/// tar/image round-trip. Unless `keep`, the distro is removed once the
/// machine exists. `target` renames the result. Returns the machine's
/// name.
pub fn migrate_to_machine(
    distro: &str,
    target: Option<String>,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    ensure_started()?;
    reject_machine_opts(opts)?;
    let name = target.unwrap_or_else(|| distro.to_string());
    validate_name(&name)?;
    if container::list_machines()?.iter().any(|m| m.id == name) {
        bail!("a machine named `{name}` already exists (use `-n` to pick another)");
    }
    // Serialize against a concurrent `set`/`start`/`rm` on the distro,
    // then finish any interrupted `set` before reading its filesystem.
    let lock = DistroLock::acquire(distro)?;
    container::warn_unverified_version();
    recover_interrupted(distro, &lock)?;
    let info = find(distro)?;
    let spec = DistroSpec::from_container(&info, &host_home()?)?;
    let was_running = info.is_running();
    if was_running {
        run_container_quiet(&["stop", distro])?;
    }
    let src = distro_rootfs(info.id())?;
    if src.is_none() {
        eprintln!(
            "container-distro: distro `{distro}` has no rootfs to carry — the machine starts from its image"
        );
    }
    let image = machine_create_image(&spec.image, src.is_some())?;
    if let Err(e) = create_machine(&name, &image, &spec, opts) {
        if was_running {
            let _ = boot(distro, &lock);
        }
        return Err(e);
    }
    // Replace the machine's filesystem while it is stopped: clone to a
    // sibling then rename over its disk, so an interruption leaves the
    // old disk or the new one, never a partial file.
    let swap = (|| -> Result<()> {
        run_container_quiet(&["machine", "stop", &name])?;
        let Some(src) = src else { return Ok(()) };
        let detail = container::inspect_machine(&name)?;
        let dst = machine_rootfs(&detail)?.context("the new machine has no rootfs to replace")?;
        let mut tmp = dst.clone().into_os_string();
        tmp.push(".new");
        let tmp = PathBuf::from(tmp);
        let _ = fs::remove_file(&tmp);
        // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
        fs::copy(&src, &tmp).with_context(|| format!("failed to clone {}", src.display()))?;
        fs::rename(&tmp, &dst).with_context(|| format!("failed to install {}", dst.display()))?;
        Ok(())
    })();
    if let Err(e) = swap {
        // The machine holds only its image filesystem; remove it so a
        // retry isn't blocked, and restart the distro.
        if let Err(r) = run_container_quiet(&["machine", "rm", &name]) {
            eprintln!("container-distro: removing incomplete machine `{name}` failed: {r:#}");
        }
        if was_running {
            let _ = boot(distro, &lock);
        }
        return Err(e.context(format!(
            "failed to move `{distro}`'s filesystem into machine `{name}`"
        )));
    }
    // `machine create`'s internal boot already absorbed the first-boot
    // provisioning flake; this run verifies the swapped filesystem and
    // leaves the machine running, like `migrate --in` leaves the distro.
    if !opts.no_boot {
        let booted = (0..3).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            boot_machine(&name).is_ok()
        });
        if !booted {
            if was_running {
                let _ = boot(distro, &lock);
            }
            bail!(
                "machine `{name}` holds the migrated filesystem but failed to boot; \
                 distro `{distro}` left in place (see `container machine logs {name}`)"
            );
        }
    }
    if !keep {
        delete_stopped_locked(distro, &lock)?;
    } else {
        if name == distro {
            eprintln!(
                "container-distro: `{name}` is now both a distro and a machine; `cm` resolves the name to the distro"
            );
        }
        if was_running {
            let _ = boot(distro, &lock);
        }
    }
    if opts.set_default
        || no_default(
            default_name().as_deref(),
            &distros()?,
            &container::list_machines().unwrap_or_default(),
        )
    {
        run_container_quiet(&["machine", "set-default", &name])?;
    }
    Ok(name)
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

    #[test]
    fn locked_assets_resist_writes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("sbin.distro");
        write_assets(&d, true).unwrap();
        let init = d.join("init");
        // The boundary a guest sees through virtiofs, applied to the
        // owner too: no write, no unlink, no dir-entry churn, no rename
        // of the locked dir itself.
        assert!(fs::write(&init, "tampered").is_err());
        assert!(fs::remove_file(&init).is_err());
        assert!(fs::write(d.join("planted"), "x").is_err());
        assert!(fs::rename(&d, dir.path().join("moved")).is_err());
        assert!(fs::read_to_string(&init).unwrap() == INIT_SCRIPT);
        // Re-refreshing works through the lock, and the restricted
        // flavor drops grant-admin.sh even though it is locked.
        write_assets(&d, false).unwrap();
        assert!(!d.join("grant-admin.sh").exists());
        write_assets(&d, true).unwrap();
        assert!(d.join("grant-admin.sh").is_file());
        // Unlock so tempdir cleanup can remove the tree.
        for n in ["init", "create-user.sh", "grant-admin.sh"] {
            unlock(&d.join(n));
        }
        unlock(&d);
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

    // ---- migrate ----

    fn detail_json() -> MachineDetail {
        serde_json::from_str(
            r#"{"id":"m","status":"stopped","cpus":4,"memory":4294967296,
                "homeMount":"ro","containerId":"m-1a2b3c",
                "userSetup":{"uid":501,"gid":20,"username":"dp"},
                "image":{"reference":"docker.io/library/alpine:latest"},
                "platform":{"os":"linux","architecture":"arm64"}}"#,
        )
        .unwrap()
    }

    #[test]
    fn machine_detail_parses_resources() {
        let d = detail_json();
        assert_eq!(d.cpus, Some(4));
        assert_eq!(d.memory, Some(4294967296));
    }

    #[test]
    fn spec_from_machine_carries_settings() {
        let spec = spec_from_machine("d1", &detail_json(), &CreateOptions::default()).unwrap();
        assert_eq!(spec.name, "d1");
        assert_eq!(spec.image, "docker.io/library/alpine:latest");
        assert_eq!(spec.cpus, Some(4));
        assert_eq!(spec.memory.as_deref(), Some("4096M"));
        assert_eq!(spec.home_mount, HomeMount::Ro);
        assert_eq!(
            spec.user,
            HostUser {
                name: "dp".into(),
                uid: 501,
                gid: 20
            }
        );
        assert!(spec.ssh && spec.admin && spec.admin_grant);
        assert!(!spec.is_restricted());
    }

    #[test]
    fn spec_from_machine_honors_overrides() {
        let opts = CreateOptions {
            cpus: Some(2),
            memory: Some("1G".into()),
            home_mount: Some(HomeMount::None),
            restricted: true,
            ..CreateOptions::default()
        };
        let spec = spec_from_machine("d1", &detail_json(), &opts).unwrap();
        assert_eq!(spec.cpus, Some(2));
        assert_eq!(spec.memory.as_deref(), Some("1G"));
        assert_eq!(spec.home_mount, HomeMount::None);
        // --restricted still defaults network/ssh/admin.
        assert_eq!(spec.network.as_deref(), Some("none"));
        assert!(!spec.ssh && !spec.admin);
    }

    #[test]
    fn our_image_by_label() {
        let img = |name: &str, labels: &[(&str, &str)]| container::ImageListEntry {
            configuration: container::ImageListConfig { name: name.into() },
            variants: vec![container::ImageVariant {
                config: Some(container::ImageVariantConfig {
                    config: Some(container::ImageUserConfig {
                        labels: labels
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    }),
                }),
            }],
        };
        let distro_key = label_key(LABEL_DISTRO);

        // The label decides regardless of the image's name.
        let labeled = img("whatever/tag:x", &[(distro_key.as_str(), "d1")]);
        assert!(is_our_image(&labeled, "d1"));
        let other = img("local/distro-d2:imported-1", &[(distro_key.as_str(), "d2")]);
        assert!(!is_our_image(&other, "d1"));

        // Unlabeled images are never touched — even under our repo.
        let unlabeled = img("local/distro-d1:imported-1", &[]);
        assert!(!is_our_image(&unlabeled, "d1"));
    }

    #[test]
    fn machine_opts_rejection() {
        // The `machine create` subset passes.
        assert!(
            reject_machine_opts(&CreateOptions {
                cpus: Some(2),
                memory: Some("1G".into()),
                home_mount: Some(HomeMount::Ro),
                no_boot: true,
                set_default: true,
                ..CreateOptions::default()
            })
            .is_ok()
        );
        // Distro-only options are refused, named in the error.
        let err = reject_machine_opts(&CreateOptions {
            volumes: vec!["/a:/b".parse().unwrap()],
            publish: vec!["8080".parse().unwrap()],
            network: Some("none".into()),
            no_ssh: true,
            sudo: true,
            restricted: true,
            ..CreateOptions::default()
        })
        .unwrap_err()
        .to_string();
        for f in [
            "--volume",
            "--publish",
            "--network",
            "--ssh/--no-ssh",
            "--sudo/--no-sudo",
            "--restricted",
        ] {
            assert!(err.contains(f), "{err}");
        }
        // --automount (even bare) and an explicit --network are refused.
        assert!(
            reject_machine_opts(&CreateOptions {
                automount: Some(Automount::Rw),
                ..CreateOptions::default()
            })
            .is_err()
        );
    }

    #[test]
    fn rootfs_source_parses_mount_record() {
        let json = r#"{"options":[],"destination":"/","type":{"block":{"format":"ext4"}},
            "source":"/x/plugin-state/machine-apiserver/machines/m/rootfs.ext4"}"#;
        assert_eq!(
            rootfs_source(json),
            Some(PathBuf::from(
                "/x/plugin-state/machine-apiserver/machines/m/rootfs.ext4"
            ))
        );
        assert_eq!(rootfs_source("not json"), None);
        assert_eq!(rootfs_source("{}"), None);
    }
}
