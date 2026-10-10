/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` operations, composed from the `container` CLI.
//!
//! ## State machine
//!
//! A distro follows the machine model: its durable state is a *journal*
//! directory at `state_dir()/distros/<name>/` — inside our own
//! Application Support directory rather than `appRoot/plugin-state/`,
//! which lives in another app's data folder (writing there trips
//! macOS's app-data consent prompt for unentitled clients) — holding
//!
//! - `spec.json`: the desired [`DistroSpec`], `uchg`-locked at rest. It
//!   doubles as the per-distro `flock` target: the lock fd is opened
//!   while the flag is cleared, then the file is re-locked. Writes
//!   through an already-open fd are still honored under `uchg`, so the
//!   journal can be rewritten in place without ever being removable.
//! - `rootfs.ext4`: the distro's disk. Moved here when the distro
//!   stops and `uchg`-locked at rest; while running it is mounted via
//!   `rootFsOverride` pointing at this path (the daemon needs it
//!   unlocked then, which is fine — a running guest owns its disk
//!   anyway, and the locked journal dir keeps it from being unlinked).
//! - `sbin.distro/`: the init assets the container mounts read-only at
//!   `/sbin.distro` — the distro's own copy, like a machine bundle's
//!   `sbin.machine`, written for this build and the spec's admin
//!   flavor on every materialize. No shared or plugin-installed copy
//!   can drift out from under it.
//!
//! A *stopped* distro is exactly this directory: its container exists
//! only while running (or transiently between `container create` and
//! the first stop). Starting materializes a fresh container from the
//! journaled spec and attaches the journaled disk, so configuration a
//! guest or a stray `container` command wrote into the runtime config
//! never survives a stop/start — including fields `DistroSpec` doesn't
//! model. Stopping stops the container, moves the disk into the
//! journal, and deletes the container.
//!
//! A leftover stopped container (a bare `container stop`, a crash, a
//! reboot) is detected and dismantled on the next operation — the same
//! converge path an interrupted `set` lands in, so an interrupted `set`
//! and a clean stop are the same state. When materialization fails the
//! distro stays stopped — like `machine boot`, there is no stale
//! recorded configuration to fall back to.
//!
//! Upgrade paths: a pre-journal distro (a container, no journal) is
//! adopted — its journal is reconstructed from the live configuration —
//! on first contact, and a legacy `preserved/<name>.{json,ext4}` pair
//! (the old `set` staging) is folded into the journal.

use std::env;
use std::ffi::CString;
use std::fs;
use std::io::{self, IsTerminal, Seek, SeekFrom, Write};
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

use crate::spec::{
    Automount, DistroOrigin, DistroSpec, HomeMount, HostUser, INIT_DIR, LABEL_DISTRO,
    LABEL_EXPORT_SCRATCH, LABEL_HOME_MOUNT, LABEL_IMPORTED_FROM, LABEL_MACHINE,
    LABEL_ROOTFS_SHA256, MountSpec, OriginSource, PublishSpec, SpecChanges, automounts,
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
    /// Provenance — what the distro was created from (journal `origin`).
    #[serde(default)]
    pub origin: Option<DistroOrigin>,
    /// Display form of the source for imported distros (`file:x.wsl`,
    /// `catalog:Name`) — set when `image` is a generated
    /// `local/distro-*` snapshot that hides where it really came from.
    /// List output prefers it over `image`.
    #[serde(default)]
    pub source: Option<String>,
}

impl DistroSummary {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// Options shared by `create` and `import`.
#[derive(Debug, Clone, Default, PartialEq, clap::Args)]
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

/// Whether `path` carries the user-immutable flag — checked with
/// `lstat` (the flag can't ride a symlink). A guest can't set it through
/// a virtiofs share, so a locked file is host-written: the check doubles
/// as journal provenance.
fn is_locked(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path; `st` a valid out-param.
    let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
    (unsafe { libc::lstat(c.as_ptr(), &mut st) }) == 0 && st.st_flags & libc::UF_IMMUTABLE != 0
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

/// The init-assets dir inside `name`'s journal — every journaled
/// distro mounts its own `sbin.distro`, like a machine bundle's
/// `sbin.machine`. Written fresh at each materialize, it can never
/// drift from this build the way a shared copy could.
fn journal_assets_dir(name: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join("sbin.distro"))
}

/// Refresh both legacy init-assets flavors in the per-user state dir,
/// and re-lock the default-distro marker if one exists (it's only
/// locked when written, so an older build may have left it open). Only
/// containers a pre-journal-assets build materialized still mount these.
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

/// Refresh the init assets the distro will actually mount at boot.
///
/// A per-distro mount source is rewritten in place (upgrading to this
/// build and the spec's admin flavor, and re-locking). Legacy mount
/// sources predate the per-distro assets: a shared per-user copy is
/// refreshed in place; an installed (plugin-dir) copy can't be
/// rewritten from here — a drifted `init` warns once, and `set` moves
/// the distro onto its own assets.
fn refresh_mounted_assets(name: &str, admin: bool) -> Result<()> {
    ensure_state_locked()?;
    let mounted = find(name).ok().and_then(|info| {
        info.configuration
            .mounts
            .iter()
            .find(|m| m.destination == INIT_DIR)
            .map(|m| PathBuf::from(&m.source))
    });
    let own = journal_assets_dir(name)?;
    match mounted {
        Some(src) if src == own => with_journal_dir_unlocked(name, || write_assets(&own, admin)),
        Some(src) if !src.starts_with(state_dir()?) => {
            if fs::read(src.join("init")).ok().as_deref() != Some(INIT_SCRIPT.as_bytes()) {
                static WARN: std::sync::Once = std::sync::Once::new();
                WARN.call_once(|| {
                    eprintln!(
                        "container-distro: warning: `{name}`'s init assets differ from this build \
                         ({}) — `set` the distro to move it onto its journal's assets",
                        src.display()
                    );
                });
            }
            Ok(())
        }
        Some(_) => refresh_assets(),
        None => Ok(()),
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

/// The distro a labeled container belongs to — its *logical* name.
/// `c.id()` is the ephemeral runtime id (`<name>-<suffix>`); the label
/// value is what the journal, `cm`, and the user know the distro by.
fn distro_name(c: &ContainerInfo) -> Option<&str> {
    label_lookup(&c.configuration.labels, LABEL_DISTRO).map(String::as_str)
}

/// The runtime container id for `name`: `<name>-<6 hex>`, like a
/// machine's backing container (`poc-a07fee`). The suffix keeps the
/// ephemeral container out of the logical namespace — a fresh id each
/// materialize, so nothing can address tomorrow's container by
/// yesterday's assumption. Truncated to `container`'s 63-char id
/// budget.
fn runtime_id(name: &str) -> String {
    let mut b = [0u8; 3];
    // SAFETY: arc4random_buf fills the buffer; it cannot fail.
    unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
    let suffix = format!("{:02x}{:02x}{:02x}", b[0], b[1], b[2]);
    let budget = 63usize.saturating_sub(suffix.len() + 1);
    let base: String = name.chars().take(budget).collect();
    format!("{}-{suffix}", base.trim_end_matches('-'))
}

pub fn find(name: &str) -> Result<ContainerInfo> {
    distros()?
        .into_iter()
        .find(|c| distro_name(c) == Some(name))
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
        origin: None,
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

/// Create `spec`'s container under the ephemeral runtime `id`.
fn create_container(spec: &DistroSpec, id: &str) -> Result<()> {
    let assets = journal_assets_dir(&spec.name)?;
    let args = spec.create_args(id, &assets.to_string_lossy(), &host_home()?);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container_quiet(&args)
}

/// Boot distro `name`'s container `id` and (idempotently) provision
/// the user account.
fn boot(name: &str, id: &str, lock: &DistroLock) -> Result<()> {
    refresh_mounted_assets(name, lock.spec()?.admin)?;
    run_container_quiet(&["start", id])?;
    let init = format!("{INIT_DIR}/init");
    run_container(&["exec", "--user", "0:0", id, &init, "-u"])
        .with_context(|| format!("user setup failed in `{name}`"))?;
    report_ports(name);
    Ok(())
}

/// Report the ports `name` publishes, now that it is listening —
/// loopback binds as info, anything wider as a LAN-exposure warning.
/// Read from the journal, not the container: the recorded spec is the
/// truth.
fn report_ports(name: &str) {
    let Ok(spec) = read_journal(name) else { return };
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

/// Start `name` under its lock if it isn't running — converge any
/// leftover container, re-resolve automounts, materialize fresh from
/// the journal, boot — and return the running container.
fn ensure_running_locked(name: &str, lock: &mut DistroLock) -> Result<ContainerInfo> {
    if let Some(info) = converge(name, lock)? {
        return Ok(info);
    }
    let spec = refresh_automounts(name, lock)?;
    let id = materialize(name, &spec, lock)?;
    boot(name, &id, lock)?;
    find(name)
}

fn ensure_running(name: &str) -> Result<ContainerInfo> {
    container::warn_unverified_version();
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    ensure_running_locked(name, &mut lock)
}

/// Whether nothing is the default: no default distro among `distros` (a
/// stale one doesn't count) and no default machine.
fn no_default(
    distro_default: Option<&str>,
    distros: &[ContainerInfo],
    machines: &[Machine],
) -> bool {
    let distro_set =
        distro_default.is_some_and(|d| distros.iter().any(|c| distro_name(c) == Some(d)));
    !distro_set && !machines.iter().any(|m| m.default)
}

fn create_from_spec(spec: &DistroSpec, no_boot: bool, set_default: bool) -> Result<String> {
    check_sources(spec)?;
    container::warn_unverified_version();
    ensure_state()?;
    // The name must be fresh in every form a distro can take — journal,
    // materialized container — and a same-named plain container would
    // collide at `create` anyway.
    if journal_dir(&spec.name)?.exists() || distro_exists(&spec.name)? {
        bail!("distro `{}` already exists", spec.name);
    }
    if container::list_containers()?
        .iter()
        .any(|c| c.id() == spec.name)
    {
        bail!(
            "a non-distro container named `{}` already exists",
            spec.name
        );
    }
    bail_if_staged(&spec.name)?;
    write_journal(&spec.name, spec)?;
    let lock = DistroLock::acquire(&spec.name)?;
    let r = (|| -> Result<()> {
        // The create is validated now — `--no-boot` lands the same
        // resting state as a stopped distro: journal only.
        let id = materialize(&spec.name, spec, &lock)?;
        if no_boot {
            dismantle(&spec.name, &id, &lock)?;
        } else {
            boot(&spec.name, &id, &lock)?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        // Leave the journal in place — a stopped distro. `start`
        // retries the materialization, `rm` discards it.
        if let Ok(info) = find(&spec.name)
            && !info.is_running()
        {
            let _ = dismantle(&spec.name, info.id(), &lock);
        }
        return Err(e);
    }
    // On request, or (like `container machine create`) when nothing is the
    // default. Otherwise leave it alone: under `cm`'s unified namespace a
    // distro default overrides the default machine. If machines can't be
    // listed, a distro is the only usable target anyway.
    let set_default = set_default
        || no_default(
            default_name().as_deref(),
            &distros()?,
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
    let mut spec = build_spec(name, image.to_string(), opts)?;
    spec.origin = Some(stamped_origin(OriginSource::Image));
    create_from_spec(&spec, opts.no_boot, opts.set_default)
}

fn summarize(c: &ContainerInfo, default: Option<&str>, app_root: Option<&Path>) -> DistroSummary {
    let cfg = &c.configuration;
    let home = host_home().unwrap_or_default();
    let spec = DistroSpec::from_container(c, &home).ok();
    let name = distro_name(c).unwrap_or_else(|| c.id());
    let journal = read_journal(name).ok();
    let origin = journal.as_ref().and_then(|s| s.origin.clone());
    // The journal's `origin.created` is the distro's real creation
    // date; a recreated container's `creationDate` would say "when it
    // last materialized" instead.
    let created_date = origin
        .as_ref()
        .map(|o| o.created.clone())
        .or_else(|| cfg.creation_date.clone());
    DistroSummary {
        id: name.to_string(),
        status: c.status.state.clone(),
        default: default == Some(name),
        ip_address: c.ipv4().map(str::to_string),
        cpus: cfg.resources.as_ref().and_then(|r| r.cpus),
        memory: cfg.resources.as_ref().and_then(|r| r.memory_in_bytes),
        disk_size: app_root.and_then(|r| container::container_disk_usage(r, c.id())),
        created_date,
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
        source: origin
            .as_ref()
            .and_then(|o| origin_source_display(&o.source)),
        origin,
    }
}

/// A `container` size string (`4096M`, `8G`, …) as bytes, for list
/// output. Unknown spellings degrade to a blank column.
fn size_bytes(s: &str) -> Option<u64> {
    let (num, unit) = s
        .trim()
        .find(|c: char| c.is_ascii_alphabetic())
        .map_or((s.trim(), ""), |i| s.trim().split_at(i));
    let mult = match unit {
        "" | "B" => 1,
        "K" | "KiB" => 1 << 10,
        "M" | "MiB" => 1 << 20,
        "G" | "GiB" => 1 << 30,
        "T" | "TiB" => 1 << 40,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// A `list` row for a journaled (stopped) distro — assembled from its
/// spec, since there is no container to inspect.
fn journal_summary(name: &str, default: Option<&str>) -> Option<DistroSummary> {
    let spec = read_journal(name).ok()?;
    let disk = journal_fs_path(name)
        .ok()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| std::os::unix::fs::MetadataExt::blocks(&m) * 512);
    Some(DistroSummary {
        id: name.to_string(),
        status: if distro_busy(name) {
            "busy".into()
        } else {
            "stopped".into()
        },
        default: default == Some(name),
        cpus: spec.cpus,
        memory: spec.memory.as_deref().and_then(size_bytes),
        disk_size: disk,
        image: Some(spec.image.clone()),
        home_mount: Some(spec.home_mount.as_str().to_string()),
        mounts: spec.mounts.iter().map(ToString::to_string).collect(),
        ports: spec.publish.iter().map(ToString::to_string).collect(),
        restricted: spec.is_restricted(),
        created_date: spec.origin.as_ref().map(|o| o.created.clone()),
        source: spec
            .origin
            .as_ref()
            .and_then(|o| origin_source_display(&o.source)),
        origin: spec.origin.clone(),
        ..Default::default()
    })
}

/// The `list` source string for a journal `origin` — `file:`/`catalog:`
/// prefixes make a replaced image ref self-documenting. `Image`,
/// `Machine`, and `Adopted` keep the image reference (a real ref is
/// the better description).
fn origin_source_display(s: &OriginSource) -> Option<String> {
    match s {
        OriginSource::File { path } => Some(format!("file:{}", source_basename(path))),
        OriginSource::Download { name, .. } => Some(format!("catalog:{name}")),
        _ => None,
    }
}

/// The trailing component of a recorded source — a local path's file
/// name or a URL's last segment; `-` (stdin) reads as `stdin`.
fn source_basename(s: &str) -> String {
    if s == "-" {
        return "stdin".to_string();
    }
    s.rsplit('/')
        .next()
        .filter(|b| !b.is_empty())
        .unwrap_or(s)
        .to_string()
}

/// Whether `image` is one of `import`'s generated refs
/// (`local/distro-<name>:imported-<ts>`) — a name that carries no
/// provenance, so `list` shows the source instead.
fn is_generated_image(image: Option<&str>) -> bool {
    image.is_some_and(|i| i.starts_with("local/distro-") && i.contains(":imported-"))
}

/// Journals predating `origin`: a generated `local/distro-*` image
/// ref hides the real source — the imported image's own
/// `imported-from` label still knows it (a local path or the catalog
/// URL). One `image list` for however many rows need it.
fn fill_legacy_sources(rows: &mut [DistroSummary]) {
    let needs = |r: &DistroSummary| r.source.is_none() && is_generated_image(r.image.as_deref());
    if !rows.iter().any(needs) {
        return;
    }
    let Some(images) = container::list_images() else {
        return;
    };
    for r in rows.iter_mut().filter(|r| needs(r)) {
        if let Some(from) = images
            .iter()
            .find(|i| Some(&i.configuration.name) == r.image.as_ref())
            .and_then(|i| i.labels().get(&label_key(LABEL_IMPORTED_FROM)).cloned())
        {
            r.source = Some(format!("file:{}", source_basename(&from)));
        }
    }
}

/// All distros, sorted by name — running or stopped-with-container from
/// `container list`, stopped ones from their journals.
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
        for name in journal_names()? {
            if !rows.iter().any(|r| r.id == name)
                && let Some(s) = journal_summary(&name, default.as_deref())
            {
                rows.push(s);
            }
        }
        // Filesystems staged by `set`s on pre-journal builds still get
        // pseudo-rows so they can be `rm`'d from the listing.
        let orphans: Vec<DistroSummary> = staged_summaries()
            .into_iter()
            .filter(|s| !rows.iter().any(|r| r.id == s.id))
            .collect();
        rows.extend(orphans);
    }
    fill_legacy_sources(&mut rows);
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}

/// Pseudo-summaries for `preserved/` leftovers whose distro is gone —
/// staging left by an interrupted `set` on a pre-journal build.
/// "interrupted" since nothing can still be holding their lock.
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
            let image = preserved_journal(&id)
                .ok()
                .and_then(|p| fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<DistroSpec>(&b).ok())
                .map(|s| s.image);
            DistroSummary {
                id,
                status: "interrupted".into(),
                image,
                ..Default::default()
            }
        })
        .collect()
}

/// Materialize `name`'s container without booting (for `inspect`), then
/// dismantle it again — a read shouldn't change the resting state.
fn peek(name: &str) -> Result<()> {
    ensure_journal(name)?;
    let lock = DistroLock::acquire(name)?;
    if converge(name, &lock)?.is_some() {
        return Ok(()); // running — inspectable as-is
    }
    let spec = lock.spec()?;
    let id = materialize(name, &spec, &lock)?;
    let r = run_container(&["inspect", &id]);
    dismantle(name, &id, &lock)?;
    r
}

pub fn inspect(names: &[String]) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    for n in names {
        // A running distro inspects as-is; anything else converges to
        // the journal view first so the output shows the configuration
        // the next start will actually apply.
        if let Some(info) = find(n).ok().filter(|i| i.is_running()) {
            run_container(&["inspect", info.id()])?;
        } else if distro_exists(n)? {
            peek(n)?;
        } else {
            bail!("no distro named `{n}`");
        }
    }
    Ok(())
}

pub fn start(name: &str) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    let id = match converge(name, &lock)? {
        Some(info) => info.id().to_string(),
        None => {
            let spec = refresh_automounts(name, &mut lock)?;
            materialize(name, &spec, &lock)?
        }
    };
    // Runs on an already-running distro too — `init -u` and the port
    // report are idempotent.
    boot(name, &id, &lock)
}

/// `container distro stop` / `cm -t`: stop the container and dismantle
/// it — the distro rests as its journal.
pub fn stop(names: &[String]) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    for n in names {
        ensure_journal(n)?;
        let lock = DistroLock::acquire(n)?;
        if let Ok(info) = find(n) {
            if info.is_running() {
                run_container_quiet(&["stop", info.id()])?;
            }
            dismantle(n, info.id(), &lock)?;
        }
    }
    Ok(())
}

/// Remove anything a pre-journal `set` staged for `name`; true when
/// something was. Called without the journal lock (there may be no
/// journal to lock).
fn cleanup_preserved(name: &str) -> bool {
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

/// Remove `name`'s journal (spec, disk, and dir), clearing `uchg` flags
/// first — including a dir that was never fully written or was planted.
fn remove_journal_dir(name: &str) -> Result<()> {
    let dir = journal_dir(name)?;
    if !dir.exists() {
        return Ok(());
    }
    with_journal_root_unlocked(|| {
        // Every entry the journal can hold — `uchg` blocks removal.
        let assets = dir.join("sbin.distro");
        if let Ok(entries) = fs::read_dir(&assets) {
            for e in entries.flatten() {
                unlock(&e.path());
            }
        }
        unlock(&assets);
        for f in ["rootfs.ext4", "spec.json"] {
            unlock(&dir.join(f));
        }
        unlock(&dir);
        fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))
    })
}

pub fn delete(force: bool, names: &[String]) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_state()?;
    for n in names {
        let container = find(n).ok();
        if container.is_none() && !journal_present(n) {
            // Not a distro — but a half-written or unsigned journal dir,
            // or a filesystem staged on a pre-journal build, can still
            // be cleaned up.
            let mut removed = journal_dir(n)?.exists();
            if removed {
                remove_journal_dir(n)?;
            }
            removed |= cleanup_preserved(n);
            if !removed {
                bail!("no distro named `{n}`");
            }
            eprintln!("removed `{n}`'s journal/staging leftovers");
            continue;
        }
        // A journal-less distro container — pre-journal leftovers the
        // migration didn't reach, or planted — is deleted without
        // adoption; there's nothing to lock without a journal.
        let _lock = journal_present(n)
            .then(|| DistroLock::acquire(n))
            .transpose()?;
        if let Some(info) = &container {
            if info.is_running() {
                if !force {
                    bail!("distro `{n}` is running (stop it first or use --force)");
                }
                run_container_quiet(&["stop", info.id()])?;
            }
            run_container_quiet(&["delete", info.id()])?;
        }
        remove_journal_dir(n)?;
        cleanup_preserved(n);
        remove_snapshot_images(n, None);
        if default_name().as_deref() == Some(n) {
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
    let spec = read_journal(&name)?;
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
    cmd.arg(info.id());
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

/// The current time as ISO 8601 UTC — the same `YYYY-MM-DDTHH:MM:SSZ`
/// shape `container` writes as `creationDate`. A libc call rather than
/// a time crate for one formatted string.
fn iso_now() -> String {
    // SAFETY: time() writes through a null pointer (returns the value)
    // and gmtime_r fills a valid out-param.
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::gmtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Journal provenance stamped at creation: when, what build, and from
/// what. `set` rewrites the spec but clones this field through — the
/// recorded version stays the *creating* build's.
fn stamped_origin(source: OriginSource) -> DistroOrigin {
    DistroOrigin {
        created: iso_now(),
        created_with: env!("CARGO_PKG_VERSION").to_string(),
        source,
    }
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

/// Refuse to create `name` while an unsigned journal dir or a legacy
/// `preserved/` pair sits at that name — the user must choose to keep
/// or discard it first.
fn bail_if_staged(name: &str) -> Result<()> {
    if journal_dir(name).is_ok_and(|d| d.exists()) {
        bail!(
            "a journal directory for `{0}` exists but isn't a usable distro — \
             `container distro rm {0}` discards it",
            name
        );
    }
    if let Ok(staged) = preserved_rootfs(name)
        && staged.exists()
    {
        bail!(
            "an interrupted `set`/`migrate` left `{0}`'s filesystem staged — \
             `container distro rm {0}` discards it",
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

/// The journal root — `state_dir()/distros`, inside our own Application
/// Support directory. Upstream keeps plugin state under
/// `appRoot/plugin-state/`, but that path lives inside *another app's*
/// data folder: an unentitled client writing there trips macOS's
/// app-data consent prompt, and resolving `appRoot` costs a
/// `container system status` spawn per journal read. The uchg flags —
/// not the location — are what keep guests from planting trusted
/// state. Every distro's durable self lives here; its container is
/// ephemeral.
fn journal_root() -> Result<PathBuf> {
    Ok(state_dir()?.join("distros"))
}

fn journal_dir(name: &str) -> Result<PathBuf> {
    journal_root().map(|r| r.join(name))
}

fn journal_spec_path(name: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join("spec.json"))
}

fn journal_fs_path(name: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join("rootfs.ext4"))
}

/// Files staged by an in-flight `set` on pre-journal builds, inside our
/// own state directory: `preserved/<name>.ext4` plus a
/// `preserved/<name>.json` journal. Kept only to fold leftovers into
/// the journal — nothing new is staged here.
fn preserved_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("preserved"))
}

fn preserved_rootfs(name: &str) -> Result<PathBuf> {
    Ok(preserved_dir()?.join(format!("{name}.ext4")))
}

fn preserved_journal(name: &str) -> Result<PathBuf> {
    Ok(preserved_dir()?.join(format!("{name}.json")))
}

/// Keep `preserved/` and `distros/` present and `uchg`-locked: through
/// a rw-shared home a guest could otherwise plant journal entries a
/// later read would trust — a confused-deputy path to mounts nobody
/// asked for. Mutations unlock a scope briefly around their file ops
/// instead.
fn ensure_state_locked() -> Result<()> {
    for dir in [preserved_dir()?, journal_root()?] {
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        set_locked(&dir, true)?;
    }
    // The pre-journal per-distro lockfiles are retired — `spec.json`
    // is the lock target now.
    let _ = fs::remove_dir_all(state_dir()?.join("locks"));
    Ok(())
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

/// Run `f` with the journal root unlocked, re-locking it afterwards —
/// even on error, where `f`'s failure is the one reported.
fn with_journal_root_unlocked<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = journal_root()?;
    unlock(&dir);
    let r = f();
    let relock = set_locked(&dir, true);
    match r {
        Err(e) => Err(e),
        Ok(v) => relock.map(|_| v),
    }
}

/// Run `f` with `name`'s journal dir unlocked — adding or removing an
/// entry needs it — re-locking afterwards.
fn with_journal_dir_unlocked<T>(name: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = journal_dir(name)?;
    unlock(&dir);
    let r = f();
    let relock = set_locked(&dir, true);
    match r {
        Err(e) => Err(e),
        Ok(v) => relock.map(|_| v),
    }
}

/// Write `spec` as `name`'s journal — both the create path and the
/// adoption of a pre-journal distro land here. The directory and
/// `spec.json` come out `uchg`-locked.
fn write_journal(name: &str, spec: &DistroSpec) -> Result<()> {
    ensure_state_locked()?;
    let dir = journal_dir(name)?;
    let spec_path = dir.join("spec.json");
    with_journal_root_unlocked(|| {
        if !dir.exists() {
            fs::create_dir(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        unlock(&spec_path);
        write_nofollow(&spec_path, &serde_json::to_vec_pretty(spec)?)?;
        set_locked(&spec_path, true)?;
        set_locked(&dir, true)
    })
}

/// The journaled spec — the distro's trusted desired configuration.
fn read_journal(name: &str) -> Result<DistroSpec> {
    let spec: DistroSpec = serde_json::from_slice(
        &fs::read(journal_spec_path(name)?).context("failed to read the distro journal")?,
    )
    .context("invalid distro journal")?;
    if spec.name != name {
        bail!(
            "journal for `{name}` names `{}` — refusing to trust it",
            spec.name
        );
    }
    Ok(spec)
}

/// Whether `name` has a verified journal: directory and spec exist and
/// carry `uchg`. A guest can't set the flag through a shared mount, so
/// an unsigned directory was planted, not written by us — it must not
/// count as a distro.
fn journal_present(name: &str) -> bool {
    let Ok(dir) = journal_dir(name) else {
        return false;
    };
    is_locked(&dir) && is_locked(&dir.join("spec.json"))
}

/// Names of every journaled distro — stopped distros have no container,
/// so this is the only place they exist. Unsigned dirs are skipped: see
/// [`journal_present`]. Reads run the one-time migration first — it's
/// how pre-journal distros surface in `list`/`cm` on upgrade.
fn journal_names() -> Result<Vec<String>> {
    migrate_once()?;
    let Ok(entries) = fs::read_dir(journal_root()?) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| journal_present(n))
        .collect())
}

/// Whether `name` is a distro — running, stopped-with-container, or
/// journaled.
fn distro_exists(name: &str) -> Result<bool> {
    Ok(journal_present(name) || find(name).is_ok())
}

/// All distro names — journaled and container-backed — sorted. What
/// `cm` resolves names against.
pub fn distro_names() -> Result<Vec<String>> {
    let mut names: Vec<String> = journal_names()?;
    names.extend(
        distros()?
            .iter()
            .filter_map(|c| distro_name(c).map(String::from)),
    );
    names.sort();
    names.dedup();
    Ok(names)
}

/// Open `path` read-write without following a planted symlink, clearing
/// its `uchg` flag around the open and re-locking it after. A file
/// descriptor keeps its write access once open — the flag gates
/// open/unlink/rename, not writes on a live fd — so callers can rewrite
/// the journal through this fd while it stays locked on disk.
///
/// `O_NOFOLLOW` keeps a symlink planted at `path` from being followed
/// (a locked file can't be replaced, so this only fires on never-locked
/// leftovers); the link is removed and the caller gets the error it
/// would have seen for a missing file.
fn open_locked_rw(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    unlock(path);
    match fs::File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => {
            set_locked(path, true)?;
            Ok(f)
        }
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove planted link {}", path.display()))?;
            bail!("{} was a symlink — removed it", path.display());
        }
        Err(e) => Err(e).with_context(|| format!("failed to open {}", path.display())),
    }
}

/// `state_dir()/journalized` — the one-time migration marker. Its
/// presence + `uchg` flag means the pre-journal migration pass already
/// ran; the flag can't be set through a shared home, so an unlocked
/// marker is forged and the pass runs anyway.
fn migrated_marker() -> Result<PathBuf> {
    state_dir().map(|d| d.join("journalized"))
}

/// Auto-adoption sunsets after the 1.1 line — drop the pass in 1.2.0.
/// `CARGO_PKG_VERSION` may carry a `-dev` prerelease.
fn auto_adoption_supported() -> bool {
    auto_adoption_supported_in(env!("CARGO_PKG_VERSION"))
}

fn auto_adoption_supported_in(version: &str) -> bool {
    let mut it = version
        .split('-')
        .next()
        .unwrap_or("0.0.0")
        .split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0));
    match (it.next().unwrap_or(0), it.next().unwrap_or(0)) {
        (0, _) => true,
        (1, m) => m <= 1,
        _ => false,
    }
}

/// Basenames of staged `preserved/<name>.{json,ext4}` files — either
/// half of an interrupted `set`'s staging.
fn staged_preserved_names() -> Result<Vec<String>> {
    let dir = preserved_dir()?;
    let mut names = Vec::new();
    for e in fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "json" || x == "ext4")
            && let Some(n) = p.file_stem().and_then(|s| s.to_str())
            && !names.iter().any(|m| m == n)
        {
            names.push(n.to_string());
        }
    }
    Ok(names)
}

/// The one-time pre-journal migration, marker-gated: while the marker
/// is absent, journal-less distro containers and staged `preserved/`
/// pairs are still legitimately ours — the world pre-journal builds
/// left behind — so they're folded in wholesale and the marker is
/// written `uchg`-locked. Afterwards a journal-less labeled container
/// can only be planted (a guest can write `containers/<name>/` through
/// a shared home, but can't set `uchg` — and can't forge the marker),
/// so [`ensure_journal`] refuses it and [`adopt`] is the explicit
/// opt-in. Past the version sunset no auto-adoption runs at all.
fn migrate_once() -> Result<()> {
    let marker = migrated_marker()?;
    if marker.exists() {
        if is_locked(&marker) {
            return Ok(());
        }
        let _ = fs::remove_file(&marker);
        eprintln!(
            "container-distro: removed a forged migration marker (unlocked — guests can't set uchg)"
        );
    }
    ensure_state_locked()?;
    // The pass needs `container list`; if the daemon can't be queried
    // there's nothing to enumerate — retry next run rather than close
    // the window early.
    let Ok(containers) = distros() else {
        return Ok(());
    };
    if auto_adoption_supported() {
        for name in staged_preserved_names()? {
            migrate_legacy_preserved(&name)?;
        }
        for c in containers {
            let Some(name) = distro_name(&c).map(String::from) else {
                continue;
            };
            if journal_present(&name) {
                continue;
            }
            match DistroSpec::from_container(&c, &host_home()?)
                .map(|spec| write_journal(&name, &spec))
            {
                Ok(Ok(())) => eprintln!("container-distro: adopted pre-journal distro `{name}`"),
                _ => eprintln!(
                    "container-distro: couldn't adopt `{name}` automatically — \
                     `container distro migrate {name}` (or `rm {name}`)"
                ),
            }
        }
    }
    write_nofollow(&marker, b"journal migration done\n")?;
    set_locked(&marker, true)?;
    Ok(())
}

/// Lock the state directories and run the one-time migration — what
/// ops call before trusting journal state.
fn ensure_state() -> Result<()> {
    ensure_state_locked()?;
    migrate_once()
}

/// Fold a legacy `preserved/<name>.{json,ext4}` pair — the pre-journal
/// `set` staging — into `name`'s journal. The `.json` holds the spec
/// the interrupted recreate was heading for. No-op when nothing is
/// staged; a staged `.json` without `uchg` can't be ours (the dir is
/// locked at rest and guests can't set the flag) — dropped, not read.
fn migrate_legacy_preserved(name: &str) -> Result<()> {
    let staged = preserved_rootfs(name)?;
    let journal = preserved_journal(name)?;
    if !staged.exists() && !journal.exists() {
        return Ok(());
    }
    // The `.json` is the spec the interrupted `set` was heading for.
    // Missing — the `set` died between staging the disk and writing
    // it — or unlocked — planted, guests can't set `uchg` — it can't
    // be trusted. A surviving container supersedes the staged disk
    // (adoption covers it), so the leftovers go; otherwise keep the
    // disk for `list`/`rm` to surface rather than silently deleting
    // someone's filesystem.
    if !journal.exists() || !is_locked(&journal) {
        if journal.exists() {
            eprintln!(
                "container-distro: ignoring `{name}`'s unlocked staged journal — \
                 a guest can't set `uchg`, so it wasn't written by a `set`"
            );
        }
        if find(name).is_ok() {
            cleanup_preserved(name);
        } else {
            with_preserved_unlocked(|| {
                unlock(&journal);
                let _ = fs::remove_file(&journal);
                Ok(())
            })?;
        }
        return Ok(());
    }
    let spec: DistroSpec = serde_json::from_slice(
        &fs::read(&journal)
            .with_context(|| format!("`{name}`'s interrupted `set` journal is unreadable"))?,
    )
    .context("invalid `set` journal")?;
    if spec.name != name {
        bail!(
            "`set` journal for `{name}` names `{}` — refusing to trust it",
            spec.name
        );
    }
    write_journal(name, &spec)?;
    with_preserved_unlocked(|| {
        unlock(&journal);
        let _ = fs::remove_file(&journal);
        if staged.exists() {
            unlock(&staged);
            let dst = journal_fs_path(name)?;
            with_journal_dir_unlocked(name, || {
                fs::rename(&staged, &dst)
                    .with_context(|| format!("failed to move {}", staged.display()))?;
                set_locked(&dst, true)
            })?;
        }
        Ok(())
    })?;
    eprintln!("container-distro: folded `{name}`'s staged filesystem into its journal");
    Ok(())
}

/// Bring `name`'s journal into existence when the distro predates it.
/// The one-time migration ([`migrate_once`]) folded everything ours
/// already; anything still journal-less here is foreign — a planted
/// "old-style" distro would be trusted verbatim, so it isn't.
/// [`adopt`] is the explicit opt-in. A journal dir that exists but
/// isn't `uchg`-locked is refused — the flag can't be set through a
/// shared mount, so it wasn't written by us. Errors when `name` is no
/// distro at all.
fn ensure_journal(name: &str) -> Result<()> {
    ensure_state()?;
    if journal_present(name) {
        return Ok(());
    }
    if find(name).is_ok() {
        bail!(
            "`{name}` predates journaled state — `container distro migrate {name}` \
             records its current config, `container distro rm {name}` deletes it"
        );
    }
    let dir = journal_dir(name)?;
    if dir.exists() {
        bail!(
            "`{name}`'s journal at {} is unusable ({}); `container distro rm {name}` removes it",
            dir.display(),
            if is_locked(&dir) {
                "never finished being written"
            } else {
                "not host-locked"
            }
        );
    }
    bail!("no distro named `{name}`")
}

/// Journal a pre-journal distro's live configuration under the user's
/// explicit say-so (`container distro migrate NAME`, `cm --migrate`).
/// This is the only trust-the-container path past the one-time
/// migration — running it on a name is blessing whatever that
/// container's recorded config says, same trust posture as
/// `--import`-ing a tarball.
pub fn adopt(name: &str) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_state()?;
    if journal_present(name) {
        println!("`{name}` is already journaled");
        return Ok(());
    }
    let info = find(name)?;
    let mut spec = DistroSpec::from_container(&info, &host_home()?)?;
    spec.origin = Some(stamped_origin(OriginSource::Adopted));
    write_journal(name, &spec)?;
    println!("adopted `{name}` — its live config is now the journal");
    Ok(())
}

/// Proof the per-distro `flock` is held. Only obtainable via
/// [`DistroLock::acquire`], so functions that must run under the lock
/// take `&DistroLock` — the compiler enforces it. The lock releases
/// when the wrapped descriptor closes (drop or process death), so an
/// interrupted operation can never leave a stale lock.
///
/// The lockfile *is* the journal's `spec.json`: the fd is opened
/// `O_RDWR|O_NOFOLLOW` while the `uchg` flag is cleared and the file is
/// re-locked immediately — the flag gates open/unlink/rename, not
/// writes on a live fd, so the same descriptor serializes operations
/// and rewrites the journal while it stays locked and unlinkable. A
/// guest could unlink a freestanding `locks/` file mid-operation and
/// silently void exclusion; it can't unlink this one.
struct DistroLock {
    file: fs::File,
    name: String,
}

impl DistroLock {
    /// Serialize operations on `name`. The journal must already exist —
    /// [`ensure_journal`] or [`write_journal`] ran first.
    fn acquire(name: &str) -> Result<Self> {
        let file = open_locked_rw(&journal_spec_path(name)?)
            .with_context(|| format!("no distro named `{name}`"))?;
        file.lock().context("failed to lock the distro journal")?;
        Ok(Self {
            file,
            name: name.to_string(),
        })
    }

    /// The journaled spec — the trusted desired configuration.
    fn spec(&self) -> Result<DistroSpec> {
        read_journal(&self.name)
    }

    /// Rewrite the spec in place through the lock fd: the journal file
    /// stays `uchg`-locked — and the same inode, so the `flock` isn't
    /// disturbed — the whole time.
    fn write_spec(&mut self, spec: &DistroSpec) -> Result<()> {
        let data = serde_json::to_vec_pretty(spec)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&data)?;
        self.file.set_len(data.len() as u64)?;
        self.file.sync_all()?;
        Ok(())
    }
}

/// Whether another process holds `name`'s journal lock — a mutation is
/// in flight. Probe-only; `O_RDONLY` opens a locked journal fine.
fn distro_busy(name: &str) -> bool {
    let Ok(path) = journal_spec_path(name) else {
        return false;
    };
    let Ok(f) = fs::File::open(path) else {
        return false;
    };
    matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock))
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

/// Create `name`'s container fresh from `spec` under a new randomized
/// runtime id and attach the journaled disk — the machine model's
/// materialization (`<name>-<6hex>`, like `poc-a07fee`). The disk is
/// unlocked for as long as the container exists (the daemon mounts it
/// read-write); the journaled spec and journal dir never become
/// mutable. Returns the container's runtime id.
///
/// A failure leaves the distro stopped with journal and disk intact:
/// like `machine boot`, there is no stale recorded configuration to
/// fall back to — the next start retries a fresh materialization.
fn materialize(name: &str, spec: &DistroSpec, _lock: &DistroLock) -> Result<String> {
    // The init assets a distro mounts live in its own journal dir —
    // written fresh for this build and the spec's admin flavor, so no
    // shared or plugin-installed copy can drift out from under it.
    let assets = journal_assets_dir(name)?;
    with_journal_dir_unlocked(name, || write_assets(&assets, spec.admin))?;
    let id = runtime_id(name);
    create_container(spec, &id)?;
    let fs_path = journal_fs_path(name)?;
    if fs_path.is_file() {
        set_rootfs_override(&container_dir(&id)?, &fs_path)?;
        set_locked(&fs_path, false)?;
    }
    Ok(id)
}

/// Dismantle `name`'s stopped container `id`: move the disk into the
/// journal when it still lives under `containers/` (first boot, or a
/// distro that predates the journal), then delete the container.
/// Whatever the runtime configuration recorded — fields `DistroSpec`
/// models and ones it doesn't — is discarded with the container.
/// Afterwards the distro is its journal alone.
fn dismantle(name: &str, id: &str, _lock: &DistroLock) -> Result<()> {
    let dir = container_dir(id)?;
    let jfs = journal_fs_path(name)?;
    // The journal path is the disk's home once the container mounted it
    // via override — nothing to move. An un-overridden container's disk
    // still lives at the conventional path.
    let mounted_journal = fs::read_to_string(dir.join("runtime-configuration.json"))
        .ok()
        .and_then(|s| rootfs_override_source(&s))
        .is_some_and(|s| s == jfs);
    let conv = dir.join("rootfs.ext4");
    if !mounted_journal && conv.is_file() {
        with_journal_dir_unlocked(name, || {
            unlock(&jfs);
            if jfs.exists() {
                // A journal disk can't have been mounted while the
                // container carried its own — the container's is the
                // truth; replace.
                fs::remove_file(&jfs)
                    .with_context(|| format!("failed to remove stale {}", jfs.display()))?;
            }
            fs::rename(&conv, &jfs)
                .with_context(|| format!("failed to move {} into the journal", conv.display()))
        })?;
    }
    run_container_quiet(&["delete", id])?;
    // The disk rests locked while nothing mounts it.
    if jfs.is_file() {
        set_locked(&jfs, true)?;
    }
    Ok(())
}

/// Field-level differences between the journaled `want` and `actual`,
/// a spec reconstructed from a live container — one human-readable line
/// each, empty when they agree. List fields compare as sets (`inspect`
/// ordering isn't stable); memory compares as bytes so `4G` and `4096M`
/// don't read as drift.
fn spec_diff(want: &DistroSpec, actual: &DistroSpec) -> Vec<String> {
    let mut out = Vec::new();
    macro_rules! field {
        ($name:literal, $w:expr, $a:expr) => {
            if $w != $a {
                out.push(format!("{}: journal {}, running {}", $name, $w, $a))
            }
        };
    }
    fn opt(o: &Option<String>) -> &str {
        o.as_deref().unwrap_or("default")
    }
    let user = |u: &HostUser| format!("{}({}:{})", u.name, u.uid, u.gid);
    field!("image", want.image, actual.image);
    field!(
        "cpus",
        want.cpus.map_or("default".to_string(), |c| c.to_string()),
        actual.cpus.map_or("default".to_string(), |c| c.to_string())
    );
    // Both sides of a memory compare go through `size_bytes` so unit
    // spelling doesn't matter; an unparseable side falls back to a
    // string compare.
    let mem = |o: &Option<String>| o.as_deref().and_then(size_bytes);
    if match (mem(&want.memory), mem(&actual.memory)) {
        (Some(w), Some(a)) => w != a,
        _ => want.memory != actual.memory,
    } {
        out.push(format!(
            "memory: journal {}, running {}",
            opt(&want.memory),
            opt(&actual.memory)
        ));
    }
    field!(
        "home-mount",
        want.home_mount.as_str(),
        actual.home_mount.as_str()
    );
    field!(
        "automount",
        want.automount.as_str(),
        actual.automount.as_str()
    );
    field!("network", opt(&want.network), opt(&actual.network));
    field!("ssh", want.ssh, actual.ssh);
    field!("admin", want.admin, actual.admin);
    field!("admin-grant", want.admin_grant, actual.admin_grant);
    field!("user", user(&want.user), user(&actual.user));
    let mut list = |name: &str, w: Vec<String>, a: Vec<String>| {
        for m in w.iter().filter(|m| !a.contains(m)) {
            out.push(format!("{name}: journal has `{m}`, running doesn't"));
        }
        for m in a.iter().filter(|m| !w.contains(m)) {
            out.push(format!("{name}: running has `{m}` the journal doesn't"));
        }
    };
    list(
        "mounts",
        want.mounts.iter().map(ToString::to_string).collect(),
        actual.mounts.iter().map(ToString::to_string).collect(),
    );
    list(
        "publish",
        want.publish.iter().map(ToString::to_string).collect(),
        actual.publish.iter().map(ToString::to_string).collect(),
    );
    out
}

/// A container already running when the lock was taken may predate the
/// journal's last write — an interrupted `set`, or tampering with the
/// recorded config. Reconstruct its spec and diff it against the
/// journal so drift is loud rather than silently adopted.
fn warn_spec_drift(name: &str, info: &ContainerInfo, lock: &DistroLock) {
    let want = lock.spec();
    let actual = host_home().and_then(|home| DistroSpec::from_container(info, &home));
    match (want, actual) {
        (Ok(want), Ok(actual)) => {
            for d in spec_diff(&want, &actual) {
                eprintln!("container-distro: warning: `{name}` drifted from its journal: {d}");
            }
        }
        (Err(e), _) => {
            eprintln!("container-distro: warning: can't check `{name}` against its journal: {e:#}")
        }
        (_, Err(e)) => eprintln!(
            "container-distro: warning: `{name}`'s running container can't be \
             verified against its journal ({e:#}) — its config isn't trusted"
        ),
    }
}

/// Converge `name` to its canonical resting state under the lock: a
/// leftover stopped container — a bare `container stop`, a crash, an
/// interrupted `set` — is dismantled, its disk rejoining the journal.
/// Returns the container only when it is *running*.
fn converge(name: &str, lock: &DistroLock) -> Result<Option<ContainerInfo>> {
    match find(name).ok() {
        Some(info) if info.is_running() => {
            warn_spec_drift(name, &info, lock);
            Ok(Some(info))
        }
        Some(info) => {
            eprintln!(
                "container-distro: `{name}` has a leftover container — \
                 regenerating it from the journal"
            );
            dismantle(name, info.id(), lock)?;
            Ok(None)
        }
        None => Ok(None),
    }
}

/// Re-resolve an enabled `--automount` against the currently attached
/// `/Volumes`, journaling the result when it changed — a plugged or
/// ejected volume shouldn't need a manual `set`. Returns the spec to
/// materialize.
fn refresh_automounts(name: &str, lock: &mut DistroLock) -> Result<DistroSpec> {
    let mut spec = lock.spec()?;
    if !spec.automount.enabled() {
        return Ok(spec);
    }
    let before = spec.mounts.clone();
    reconcile_automounts(&mut spec)?;
    if spec.mounts != before {
        eprintln!("container-distro: refreshing `{name}`'s automounts for the attached /Volumes");
        lock.write_spec(&spec)?;
    }
    Ok(spec)
}

/// `container distro set`: update `name`'s journaled spec. A running
/// distro is stopped and rebooted under the new settings; a stopped one
/// is just re-journaled — its container is fresh-created at the next
/// start either way. Conceptually stop → update → start, so an
/// interruption lands in the same resting state a normal stop does.
/// Like `machine boot`, a failed materialize leaves the distro stopped
/// with the new spec committed; there is no stale config to fall back
/// to.
pub fn set(name: &str, changes: &SpecChanges) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    if changes.is_empty() {
        bail!("nothing to change (see `container distro set --help`)");
    }
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    let old = lock.spec()?;
    let mut new = changes.apply(&old)?;
    warn_public_publish(&changes.add_publish);
    // An enabled automount is always reconciled, not just when
    // `--automount` was passed.
    if new.automount.enabled() {
        reconcile_automounts(&mut new)?;
    }
    check_sources(&new)?;
    let info = find(name).ok();
    let was_running = info.as_ref().is_some_and(|i| i.is_running());
    if let Some(info) = &info {
        if was_running {
            run_container_quiet(&["stop", info.id()])?;
        }
        dismantle(name, info.id(), &lock)?;
    }
    lock.write_spec(&new)?;
    if was_running {
        let id = materialize(name, &new, &lock)?;
        boot(name, &id, &lock)?;
    }
    remove_snapshot_images(name, Some(new.image.as_str()));
    Ok(())
}

pub fn set_default(name: Option<&str>) -> Result<()> {
    if let Some(n) = name {
        ensure_started()?;
        if !distro_exists(n)? {
            bail!("no distro named `{n}`");
        }
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

/// `container distro export`. `container export` snapshots
/// `containers/<id>/rootfs.ext4` literally — a distro mounts its disk
/// from the journal via `rootFsOverride`, so export can't see it (the
/// same gap `export_machine` works around). The workaround is the same
/// too: clone the journaled disk into a scratch container that exports
/// normally, then delete it. A running distro is stopped for a
/// crash-consistent clone, then restarted.
pub fn export(name: &str, output: Option<&Path>) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    if let Some(o) = output {
        check_export_output(o)?;
    }
    let scratch = ScratchContainer::create(
        &export_scratch_name(name),
        &scratch_image(lock.spec()?.image.as_str())?,
        name,
    )?;
    // Stop + converge before cloning — a running disk's clone would
    // only be crash-consistent.
    let was_running = if let Some(info) = find(name).ok().filter(|i| i.is_running()) {
        run_container_quiet(&["stop", info.id()])?;
        true
    } else {
        false
    };
    converge(name, &lock)?;
    let cloned = (|| -> Result<()> {
        let src = journal_fs_path(name)?;
        if !src.is_file() {
            bail!("distro `{name}` has no root filesystem to export (it has never been started)");
        }
        // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
        fs::copy(&src, container_dir(&scratch.0)?.join("rootfs.ext4"))
            .with_context(|| format!("failed to clone {}", src.display()))?;
        Ok(())
    })();
    if was_running && let Err(e) = ensure_running_locked(name, &mut lock) {
        eprintln!("container-distro: warning: failed to restart distro `{name}`: {e:#}");
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

/// Where an imported rootfs came from, recorded in the loaded image's
/// labels (`imported-from`, plus `rootfs-sha256` for downloads) and the
/// journal's `origin` — a local path for `Local`, the download itself
/// for `Download`.
#[derive(Debug, Clone, Copy)]
pub enum ImportSource<'a> {
    /// A local file path (or `-` for stdin).
    Local,
    /// A catalog `.wsl` download: the URL, the SHA-256 it was verified
    /// against, and the catalog entry's name.
    Download {
        url: &'a str,
        sha256: &'a str,
        name: &'a str,
    },
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
    if distro_exists(name)? {
        bail!("distro `{name}` already exists");
    }
    // Checked here too so leftover state doesn't orphan the image
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
        ImportSource::Download { url, sha256, .. } => {
            labels.push((label_key(LABEL_IMPORTED_FROM), url.to_string()));
            labels.push((label_key(LABEL_ROOTFS_SHA256), sha256.to_string()));
        }
    }
    oci::load_rootfs(rootfs, &reference, &labels)?;
    let mut spec = build_spec(name.to_string(), reference, opts)?;
    spec.origin = Some(stamped_origin(match source {
        ImportSource::Local => OriginSource::File {
            path: file.to_string_lossy().into_owned(),
        },
        ImportSource::Download { url, sha256, name } => OriginSource::Download {
            url: url.to_string(),
            sha256: sha256.to_string(),
            name: name.to_string(),
        },
    }));
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

/// The scratch container's name: the export target's (truncated to fit
/// the 63-char `container` limit with the `-export-<timestamp>` suffix)
/// so a leftover is recognizable.
fn export_scratch_name(of: &str) -> String {
    let base: String = of.chars().take(44).collect();
    format!("{}-export-{}", base.trim_end_matches('-'), timestamp())
}

/// The image a scratch export container is created from: `prefer` — the
/// target's own image — when still local, else any local image. The
/// container is never booted, so the image only has to exist. Fails
/// when the daemon holds no images at all.
fn scratch_image(prefer: &str) -> Result<String> {
    let images = container::list_images().unwrap_or_default();
    let is = |n: &str| {
        n == prefer
            || n.strip_prefix("docker.io/") == Some(prefer)
            || n.strip_prefix("docker.io/library/") == Some(prefer)
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
        &scratch_image(
            detail
                .image
                .as_ref()
                .map(|i| i.reference.as_str())
                .unwrap_or_default(),
        )?,
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
    if distro_exists(name)? {
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
    // A journaled distro is already home; a journal-less labeled
    // container is pre-journal — adopted, not migrated. Naming it is
    // the explicit opt-in the journal gate asks for; `-n`/create
    // options don't apply since its live config is recorded as-is.
    // The journal check comes first: a stopped distro has no container
    // for `find` to see.
    if journal_present(machine) {
        bail!("`{machine}` is already a distro");
    }
    if find(machine).is_ok() {
        if target.is_some() || *opts != CreateOptions::default() {
            bail!(
                "`{machine}` is a pre-journal distro — `migrate` adopts its \
                 live config as-is; name and create options don't apply"
            );
        }
        adopt(machine)?;
        return Ok(format!(
            "`{machine}` adopted — its live config is now the journal"
        ));
    }
    if !container::list_machines()?.iter().any(|m| m.id == machine) {
        bail!("no machine or distro named `{machine}`");
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
        origin: None,
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

/// Machine → distro: clone the machine's rootfs into the new distro's
/// journal — its resting state — then materialize and boot.
fn migrate_to_distro(
    machine: &str,
    name: &str,
    keep: bool,
    opts: &CreateOptions,
) -> Result<String> {
    validate_name(name)?;
    // Fail fast, before the machine is stopped or its disk cloned.
    bail_if_staged(name)?;
    if distro_exists(name)? {
        bail!("distro `{name}` already exists");
    }
    let detail = container::inspect_machine(machine)?;
    let mut spec = spec_from_machine(name, &detail, opts)?;
    spec.origin = Some(stamped_origin(OriginSource::Machine {
        name: machine.to_string(),
    }));
    check_sources(&spec)?;
    let was_running = detail.is_running();
    if was_running {
        run_container_quiet(&["machine", "stop", machine])?;
    }
    container::warn_unverified_version();
    write_journal(name, &spec)?;
    let lock = DistroLock::acquire(name)?;
    match machine_rootfs(&detail)? {
        Some(src) => {
            let dst = journal_fs_path(name)?;
            with_journal_dir_unlocked(name, || {
                // `fs::copy` is a clonefile on APFS — instant and
                // copy-on-write.
                fs::copy(&src, &dst)
                    .with_context(|| format!("failed to clone {}", src.display()))?;
                set_locked(&dst, true)
            })?;
        }
        None => {
            eprintln!(
                "container-distro: machine `{machine}` has no rootfs to carry — the distro starts from its image"
            );
        }
    }
    if let Err(e) = materialize(name, &spec, &lock).and_then(|id| {
        if opts.no_boot {
            dismantle(name, &id, &lock)
        } else {
            boot(name, &id, &lock)
        }
    }) {
        if let Ok(info) = find(name) {
            let _ = run_container_quiet(&["delete", info.id()]);
        }
        let _ = remove_journal_dir(name);
        if was_running {
            let _ = boot_machine(machine);
        }
        return Err(e);
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

/// A distro's ext4 rootfs: the journaled `rootfs.ext4` (its resting
/// home) first, then — for a first boot or a pre-journal distro — the
/// conventional `containers/<id>/rootfs.ext4` or an override source.
/// `None` before the first boot: the filesystem is still the pristine
/// image.
fn distro_rootfs(name: &str) -> Result<Option<PathBuf>> {
    let jfs = journal_fs_path(name)?;
    if jfs.is_file() {
        return Ok(Some(jfs));
    }
    let Some(dir) = find(name).ok().and_then(|i| container_dir(i.id()).ok()) else {
        return Ok(None);
    };
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
    // A pre-journal distro named for `--out` is explicit opt-in —
    // adopt its live config first, then carry the disk out.
    if !journal_present(distro) && find(distro).is_ok() {
        adopt(distro)?;
    } else {
        ensure_journal(distro)?;
    }
    // Serialize against a concurrent `set`/`start`/`rm`, then converge
    // the distro to its resting state before cloning its disk.
    let mut lock = DistroLock::acquire(distro)?;
    container::warn_unverified_version();
    let spec = lock.spec()?;
    let was_running = if let Some(info) = find(distro).ok().filter(|i| i.is_running()) {
        run_container_quiet(&["stop", info.id()])?;
        true
    } else {
        false
    };
    converge(distro, &lock)?;
    let src = distro_rootfs(distro)?;
    if src.is_none() {
        eprintln!(
            "container-distro: distro `{distro}` has no rootfs to carry — the machine starts from its image"
        );
    }
    let image = machine_create_image(&spec.image, src.is_some())?;
    if let Err(e) = create_machine(&name, &image, &spec, opts) {
        if was_running {
            let _ = ensure_running_locked(distro, &mut lock);
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
            let _ = ensure_running_locked(distro, &mut lock);
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
                let _ = ensure_running_locked(distro, &mut lock);
            }
            bail!(
                "machine `{name}` holds the migrated filesystem but failed to boot; \
                 distro `{distro}` left in place (see `container machine logs {name}`)"
            );
        }
    }
    if !keep {
        // The distro is already converged (container dismantled);
        // remove the journal, its images, and the default marker.
        remove_journal_dir(distro)?;
        cleanup_preserved(distro);
        remove_snapshot_images(distro, None);
        if default_name().as_deref() == Some(distro) {
            write_default(None)?;
        }
    } else {
        if name == distro {
            eprintln!(
                "container-distro: `{name}` is now both a distro and a machine; `cm` resolves the name to the distro"
            );
        }
        if was_running {
            let _ = ensure_running_locked(distro, &mut lock);
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

    /// A distro container: `name` is its logical name — the `distro`
    /// label — while `id` is the ephemeral runtime id.
    fn distro_id(id: &str, name: &str) -> ContainerInfo {
        serde_json::from_str(&format!(
            r#"{{"configuration":{{"id":"{id}","labels":{{"{}":"{name}"}}}}}}"#,
            label_key(LABEL_DISTRO)
        ))
        .unwrap()
    }

    fn distro(name: &str) -> ContainerInfo {
        distro_id(&format!("{name}-a1b2c3"), name)
    }

    fn machine(id: &str, default: bool) -> Machine {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","status":"running","default":{default}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn runtime_id_shape() {
        let id = runtime_id("alpine");
        let (base, suffix) = id.rsplit_once('-').unwrap();
        assert_eq!(base, "alpine");
        assert_eq!(suffix.len(), 6);
        assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()));
        // Long names truncate to `container`'s 63-char budget, never
        // leaving a dangling `-` before the suffix.
        let long = runtime_id(&"x".repeat(63));
        assert_eq!(long.len(), 63);
        assert!(long.chars().nth(56) == Some('-'));
        // Two ids for the same name differ.
        assert_ne!(runtime_id("alpine"), runtime_id("alpine"));
    }

    #[test]
    fn source_display() {
        let disp = |s: &OriginSource| origin_source_display(s).unwrap();
        assert_eq!(
            disp(&OriginSource::File {
                path: "/tmp/Rocky-9.wsl".into()
            }),
            "file:Rocky-9.wsl"
        );
        assert_eq!(disp(&OriginSource::File { path: "-".into() }), "file:stdin");
        assert_eq!(
            disp(&OriginSource::Download {
                url: "https://x/d.wsl".into(),
                sha256: "ab".into(),
                name: "RLCPlus-9".into()
            }),
            "catalog:RLCPlus-9"
        );
        // Real references stay in the image column.
        for s in [
            OriginSource::Image,
            OriginSource::Machine { name: "poc".into() },
            OriginSource::Adopted,
        ] {
            assert!(origin_source_display(&s).is_none());
        }
        // Basenames: URLs and paths alike.
        assert_eq!(
            source_basename("https://x/y/RLCPlus-9.aarch64.wsl"),
            "RLCPlus-9.aarch64.wsl"
        );
        assert_eq!(source_basename("/a/b/rootfs.tar.gz"), "rootfs.tar.gz");
        assert!(is_generated_image(Some("local/distro-r9:imported-1")));
        assert!(!is_generated_image(Some("local/distro-r9:snap-1")));
        assert!(!is_generated_image(Some("docker.io/library/alpine:latest")));
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
    fn size_bytes_parses_container_sizes() {
        assert_eq!(size_bytes("4096M"), Some(4096 << 20));
        assert_eq!(size_bytes("8G"), Some(8 << 30));
        assert_eq!(size_bytes("512K"), Some(512 << 10));
        assert_eq!(size_bytes("1024"), Some(1024));
        assert_eq!(size_bytes("garbage"), None);
        assert_eq!(size_bytes(""), None);
    }

    fn spec() -> DistroSpec {
        DistroSpec {
            name: "d1".into(),
            image: "alpine:latest".into(),
            cpus: Some(2),
            memory: Some("4G".into()),
            home_mount: HomeMount::Rw,
            mounts: vec!["/a:/b:ro".parse().unwrap()],
            automount: Automount::None,
            publish: vec!["8080:80".parse().unwrap()],
            network: None,
            ssh: true,
            admin: true,
            admin_grant: false,
            user: HostUser {
                name: "dp".into(),
                uid: 501,
                gid: 20,
            },
            origin: None,
        }
    }

    /// The start-time integrity check: identical specs produce no diff;
    /// drifted fields name themselves; `4G` vs `4096M` is not drift.
    #[test]
    fn spec_diff_names_drifted_fields() {
        let want = spec();
        // `from_container` normalizes memory to `NM` — same bytes is
        // not drift.
        let mut actual = spec();
        actual.memory = Some("4096M".into());
        assert!(spec_diff(&want, &actual).is_empty());

        let mut actual = spec();
        actual.cpus = Some(8);
        actual.ssh = false;
        actual.home_mount = HomeMount::Ro;
        let diff = spec_diff(&want, &actual);
        assert!(diff.iter().any(|d| d.starts_with("cpus:")), "{diff:?}");
        assert!(diff.iter().any(|d| d.starts_with("ssh:")), "{diff:?}");
        assert!(
            diff.iter().any(|d| d.starts_with("home-mount:")),
            "{diff:?}"
        );

        // List fields diff per-entry, unordered.
        let mut actual = spec();
        actual.mounts.pop();
        actual.mounts.push("/x:/y".parse().unwrap());
        let diff = spec_diff(&want, &actual);
        assert!(
            diff.iter()
                .any(|d| d == "mounts: journal has `/a:/b:ro`, running doesn't"),
            "{diff:?}"
        );
        assert!(
            diff.iter()
                .any(|d| d == "mounts: running has `/x:/y` the journal doesn't"),
            "{diff:?}"
        );
    }

    /// The journal lock contract: `spec.json` is `uchg`-locked at rest
    /// yet rewritable through the fd the flock is held on — verified
    /// behavior on macOS (the flag gates open/unlink/rename, not writes
    /// on a live descriptor).
    #[test]
    fn journal_lock_writes_through_uchg() {
        let dir = tempfile::tempdir().unwrap();
        let spec_p = dir.path().join("spec.json");
        fs::write(&spec_p, "{}").unwrap();
        set_locked(&spec_p, true).unwrap();
        assert!(is_locked(&spec_p));
        // The boundary a guest sees: no write, no unlink.
        assert!(fs::write(&spec_p, "tampered").is_err());
        assert!(fs::remove_file(&spec_p).is_err());
        assert!(fs::File::options().write(true).open(&spec_p).is_err());

        let spec = DistroSpec {
            name: "d1".into(),
            image: "alpine:latest".into(),
            cpus: Some(2),
            memory: Some("4G".into()),
            home_mount: HomeMount::Rw,
            mounts: vec![],
            automount: Automount::None,
            publish: vec![],
            network: None,
            ssh: true,
            admin: true,
            admin_grant: true,
            user: HostUser {
                name: "dp".into(),
                uid: 501,
                gid: 20,
            },
            origin: None,
        };
        let mut lock = DistroLock {
            file: open_locked_rw(&spec_p).unwrap(),
            name: "d1".into(),
        };
        lock.file.lock().unwrap();
        // The file is re-locked under the open fd, and a second locker
        // blocks — exclusion survives the unlock window.
        assert!(is_locked(&spec_p));
        assert!(matches!(
            fs::File::open(&spec_p).unwrap().try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        // Rewrites through the held fd are honored under `uchg`.
        lock.write_spec(&spec).unwrap();
        assert_eq!(
            serde_json::from_slice::<DistroSpec>(&fs::read(&spec_p).unwrap()).unwrap(),
            spec
        );
        drop(lock);
        assert!(fs::File::open(&spec_p).unwrap().try_lock().is_ok());
        unlock(&spec_p);
    }

    /// Auto-adoption of pre-journal distros is a sunsetted migration
    /// path — supported through the 1.1 line, gone in 1.2.
    #[test]
    fn adoption_sunset_version_gate() {
        for v in ["0.4.2", "0.5.0-dev", "1.0.0", "1.1.0", "1.1.9"] {
            assert!(auto_adoption_supported_in(v), "{v}");
        }
        for v in ["1.2.0", "2.0.0"] {
            assert!(!auto_adoption_supported_in(v), "{v}");
        }
    }

    /// A symlink planted where the journal spec belongs is removed
    /// rather than followed — the file can't have been locked, so it
    /// was never ours.
    #[test]
    fn open_locked_rw_rejects_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let spec_p = dir.path().join("spec.json");
        std::os::unix::fs::symlink("/etc/passwd", &spec_p).unwrap();
        assert!(open_locked_rw(&spec_p).is_err());
        assert!(!spec_p.exists());
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
