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
use crate::wsl;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::spec::{
    Automount, ConfList, DistroOrigin, DistroSpec, HomeMount, HostUser, INIT_DIR, Intent,
    JournalSpec, LABEL_DISTRO, LABEL_EXPORT_SCRATCH, LABEL_HOME_MOUNT, LABEL_IMPORTED_FROM,
    LABEL_ROOTFS_SHA256, MountSpec, OriginSource, Preset, PublishSpec, ResolveCtx, ResolvedSpec,
    SpecChanges, WslConf, WslConfMode, automounts, mode_from_flags, size_bytes,
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
    /// User mounts as `SRC:DST[:ro]` — the journaled shares only.
    #[serde(default)]
    pub mounts: Vec<String>,
    /// The derived shares: the `/Volumes` automounts plus the home
    /// mount — computed, never journaled.
    #[serde(default)]
    pub automounts: Vec<String>,
    /// The full mount set passed to `container create`: `mounts` +
    /// `automounts` with user mounts winning target collisions.
    #[serde(default)]
    pub effective_mounts: Vec<String>,
    /// The `/Volumes` automount mode (`rw`/`ro`/`none`).
    #[serde(default)]
    pub automount: Option<String>,
    /// The defaults tier (`default`/`restricted`/`trusted`).
    #[serde(default)]
    pub preset: Option<String>,
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
    /// Share a host directory: SRC:DST[:opt,...] where opts include
    /// ro, rw, and guest mount flags (nosuid, nodev, noexec, noatime,
    /// relatime, strictatime, sync, dirsync — repeatable)
    #[arg(short = 'v', long = "volume", value_name = "SRC:DST[:opt,...]")]
    pub volumes: Vec<MountSpec>,

    /// Share every /Volumes/<name> at /mnt/<name> (like WSL's
    /// /mnt/<drive>); `--automount ro` mounts read-only. Hidden,
    /// Apple-private (com.apple.*), and unreadable volumes are skipped.
    /// Re-resolved on `set` and when a stopped distro starts, so
    /// attaching or ejecting a volume needs no manual fix-up. With no
    /// --home-mount, your home directory gets the same access (warned)
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

    /// How to share your macOS home directory (default: none —
    /// read-write only for distros installed through a catalog entry,
    /// or when --automount implies the same access)
    #[arg(long, value_enum)]
    pub home_mount: Option<HomeMount>,

    /// Attach to a container network by name ("none" for no network)
    #[arg(long, value_name = "NAME")]
    pub network: Option<String>,

    /// Forward the host SSH agent socket into the distro (default: only
    /// when the effective home-mount is rw — the same trust tier)
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

    /// The defaults tier the distro runs at — `default`,
    /// `restricted`, or `trusted`. Recorded in the journal so `set
    /// --preset` can re-derive every option you haven't pinned
    #[arg(long, value_enum, conflicts_with_all = ["restricted", "trusted"])]
    pub preset: Option<Preset>,

    /// Restricted defaults for semi-trusted images: home mount none, no
    /// network, no SSH agent, no sudo/doas. Explicit flags still apply.
    /// A convenience preset, not a sandbox. (`--preset restricted`)
    #[arg(long, visible_alias = "untrusted")]
    pub restricted: bool,

    /// Trusted defaults for images you fully trust: home mount rw,
    /// /Volumes automounted rw, SSH agent forwarded. Explicit flags
    /// still apply — the mirror of --restricted. (`--preset trusted`)
    #[arg(long, conflicts_with = "restricted")]
    pub trusted: bool,

    /// Honor the guest's /etc/wsl.conf — optionally a comma list of
    /// features: hostname, fstab, init, command, automount, or
    /// `all`/`none` (bare `--use-wsl-conf` is `all`). A bare list pins
    /// exactly those features (`none` honors nothing); `command` runs
    /// the file's [boot] command as root once per boot; `automount`
    /// lowers automount/home-mount for an archive whose config says
    /// enabled=false. Default: `all` for --trusted and catalog
    /// installs, `none` otherwise
    #[arg(long, value_name = "FEATURE,...|all|none", num_args = 0..=1,
          require_equals = true, default_missing_value = "all")]
    pub use_wsl_conf: Option<ConfList>,

    /// Stop honoring /etc/wsl.conf entirely (the same as
    /// `--use-wsl-conf=none`)
    #[arg(long, conflicts_with_all = ["use_wsl_conf", "ignore_wsl_conf"])]
    pub no_wsl_conf: bool,

    /// Deny parts of /etc/wsl.conf — subtracts from what the preset and
    /// source would grant (`all` ignores the file entirely, the same
    /// as --no-wsl-conf)
    #[arg(long, value_name = "FEATURE,...|all|none")]
    pub ignore_wsl_conf: Option<ConfList>,

    /// Create without booting
    #[arg(long)]
    pub no_boot: bool,

    /// Make this the default distro (automatic when nothing is the default)
    #[arg(long)]
    pub set_default: bool,

    /// Create even when the name is taken in the other namespace — a
    /// same-named `container machine`, or a same-named distro when
    /// creating a machine. `cm` resolves a shared name to the distro,
    /// shadowing the machine, so the collision is refused without this
    #[arg(long)]
    pub force: bool,
}

impl CreateOptions {
    /// The defaults preset the flags select. `--preset`,
    /// `--restricted`, and `--trusted` conflict at parse, so at most
    /// one is set.
    pub fn preset(&self) -> Preset {
        self.preset.unwrap_or({
            if self.restricted {
                Preset::Restricted
            } else if self.trusted {
                Preset::Trusted
            } else {
                Preset::Default
            }
        })
    }
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
/// The host-derived resource defaults a distro materializes with
/// when intent leaves them unset (half the host's cpus/memory) —
/// `list` brackets them (`[8]`) to mark the value as derived.
pub fn default_resources() -> (u64, String) {
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

/// The `/Volumes` shares to add at materialize time for `spec` — the
/// automount scan when the mode is enabled, skipping targets the
/// user's own mounts already claim. Derived, never journaled.
fn auto_shares(r: &ResolvedSpec) -> Result<Vec<MountSpec>> {
    if !r.automount.enabled() {
        return Ok(Vec::new());
    }
    let taken = r.mounts.iter().map(|m| m.target.clone()).collect();
    scan_automounts(r.automount, &taken)
}

/// The host-dependent inputs `DistroSpec::resolve` can't see.
fn resolve_ctx() -> ResolveCtx {
    let (cpus, memory) = default_resources();
    ResolveCtx { cpus, memory }
}

/// A clap flag pair (`--ssh`/`--no-ssh`) as an `Option<bool>` pin —
/// they conflict at parse, so at most one is set.
fn flag_pair(on: bool, off: bool) -> Option<bool> {
    if on {
        Some(true)
    } else if off {
        Some(false)
    } else {
        None
    }
}

/// The `Intent` a `CreateOptions`' flags pin — the user side of the
/// spec; the source side comes from the catalog entry or machine.
fn create_intent(opts: &CreateOptions) -> Intent {
    // `--no-wsl-conf` is `--use-wsl-conf=none` by another name; the
    // parse-level conflict keeps the two from arriving together.
    let use_conf = opts
        .use_wsl_conf
        .clone()
        .or_else(|| opts.no_wsl_conf.then(ConfList::none));
    Intent {
        home_mount: opts.home_mount,
        automount: opts.automount,
        ssh: flag_pair(opts.ssh, opts.no_ssh),
        admin: flag_pair(opts.sudo, opts.no_sudo),
        network: opts.network.clone(),
        cpus: opts.cpus,
        memory: opts.memory.clone(),
        wsl_conf: mode_from_flags(use_conf.as_ref(), opts.ignore_wsl_conf.as_ref(), None),
    }
}

/// The options half of a spec: `opts`' flags as `explicit` intent
/// resolved against `source`, before identity (name/image/user)
/// lands — [`build_spec`] finishes those. Shared with the
/// machine-tier checks (`--install --machine`, `migrate --out`) so
/// what they probe is exactly the spec a distro would run.
fn options_spec(opts: &CreateOptions, source: &Intent) -> DistroSpec {
    DistroSpec {
        // `resolve` never reads identity — `build_spec` fills it in.
        name: String::new(),
        image: String::new(),
        mounts: opts.volumes.clone(),
        publish: opts.publish.clone(),
        user: HostUser::default(),
        preset: opts.preset(),
        explicit: create_intent(opts),
        source: source.clone(),
        admin_grant: false,
        origin: None,
    }
}

impl CreateOptions {
    /// The intent-side spec these flags describe for `source` —
    /// identity (name/image/user) is left for the caller; `create`'s
    /// path fills it via `build_spec`, and `create_machine` layers
    /// `spec_changes` on a scratch copy.
    pub fn spec(&self, source: &Intent) -> DistroSpec {
        options_spec(self, source)
    }

    /// The spec these flags resolve to for `source` — computed by
    /// `DistroSpec::resolve` itself, so paths that need a resolved
    /// field without a journal (the machine-tier checks) derive
    /// exactly what a distro would.
    pub fn resolved_spec(&self, source: &Intent) -> ResolvedSpec {
        options_spec(self, source).resolve().0
    }

    /// `resolved_spec(source).home_mount` — for the `--home-mount`
    /// arg the machine checks still pass through.
    pub fn resolve_home_mount(&self, source: &Intent) -> HomeMount {
        self.resolved_spec(source).home_mount
    }

    /// The flags as `SpecChanges` — set-style overrides layered onto
    /// an existing spec (the `migrate --out` scratch spec). Only the
    /// options a machine can honor reach it; `reject_machine_opts`
    /// refuses the rest first.
    pub fn spec_changes(&self) -> SpecChanges {
        SpecChanges {
            preset: self.preset.or(if self.restricted {
                Some(Preset::Restricted)
            } else if self.trusted {
                Some(Preset::Trusted)
            } else {
                None
            }),
            cpus: self.cpus,
            memory: self.memory.clone(),
            home_mount: self.home_mount,
            automount: self.automount,
            network: self.network.clone(),
            ssh: flag_pair(self.ssh, self.no_ssh),
            admin: flag_pair(self.sudo, self.no_sudo),
            add_mounts: self.volumes.clone(),
            add_publish: self.publish.clone(),
            wsl_conf: self
                .use_wsl_conf
                .clone()
                .or_else(|| self.no_wsl_conf.then(ConfList::none)),
            wsl_conf_ignore: self.ignore_wsl_conf.clone(),
            ..SpecChanges::default()
        }
    }
}

fn build_spec(
    name: String,
    image: String,
    opts: &CreateOptions,
    source: &Intent,
) -> Result<DistroSpec> {
    validate_name(&name)?;
    let mut spec = options_spec(opts, source);
    spec.name = name;
    spec.image = image;
    spec.user = host_user()?;
    let (resolved, warnings) = spec.resolve();
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    // The grant env is armed at create — a guest that removes its own
    // privilege files stays unprivileged until an explicit `set
    // --sudo` re-arms it.
    spec.admin_grant = resolved.admin;
    if resolved.network.as_deref() == Some("none") && !spec.publish.is_empty() {
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
/// Automounts are resolved from the live `/Volumes` right here — the
/// journal stays declarative, so an attached or ejected volume is
/// picked up at every materialize, no rewrite needed.
fn create_container(spec: &DistroSpec, id: &str) -> Result<()> {
    let assets = journal_assets_dir(&spec.name)?;
    let (resolved, _) = spec.resolve();
    let auto = auto_shares(&resolved)?;
    let args = resolved.create_args(
        &resolve_ctx(),
        id,
        &assets.to_string_lossy(),
        &host_home()?,
        &auto,
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container_quiet(&args)
}

/// Boot distro `name`'s container `id` and (idempotently) provision
/// the user account.
fn boot(name: &str, id: &str, lock: &DistroLock) -> Result<()> {
    let (r, _) = lock.spec()?.resolve();
    refresh_mounted_assets(name, r.admin)?;
    run_container_quiet(&["start", id])?;
    let init = format!("{INIT_DIR}/init");
    run_container(&["exec", "--user", "0:0", id, &init, "-u"])
        .with_context(|| format!("user setup failed in `{name}`"))?;
    // Snapshot the guest config while the container is up — the only
    // window `container cp` can read it — for host-side consumers now
    // and later.
    if let Ok(spec) = lock.spec() {
        refresh_conf_snapshots(name, id, &spec);
    }
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
/// leftover container, materialize fresh from the journal (which is
/// where the automount scan happens), boot — and return the running
/// container.
fn ensure_running_locked(name: &str, lock: &mut DistroLock) -> Result<ContainerInfo> {
    if let Some(info) = converge(name, lock)? {
        return Ok(info);
    }
    let spec = lock.spec()?;
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

fn create_from_spec(spec: &DistroSpec, opts: &CreateOptions) -> Result<String> {
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
    bail_machine_shadow(&spec.name, opts.force)?;
    write_journal(&spec.name, spec)?;
    let lock = DistroLock::acquire(&spec.name)?;
    let r = (|| -> Result<()> {
        // The create is validated now — `--no-boot` lands the same
        // resting state as a stopped distro: journal only.
        let id = materialize(&spec.name, spec, &lock)?;
        if opts.no_boot {
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
    let set_default = opts.set_default
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
/// `origin` is the provenance stamped into the journal; `requested`
/// is the source side of the spec's intent — what the catalog entry
/// or installer asked for (a catalog's `home-mount` request, for
/// one). It loses to every explicit flag and to the preset.
pub fn create(
    name: Option<String>,
    opts: &CreateOptions,
    image: &str,
    origin: OriginSource,
    requested: &Intent,
) -> Result<String> {
    ensure_started()?;
    let name = name.unwrap_or_else(|| default_machine_name(image));
    let mut spec = build_spec(name, image.to_string(), opts, requested)?;
    spec.origin = Some(stamped_origin(origin));
    create_from_spec(&spec, opts)
}

fn summarize(c: &ContainerInfo, default: Option<&str>, app_root: Option<&Path>) -> DistroSummary {
    let cfg = &c.configuration;
    let home = host_home().unwrap_or_default();
    let spec = DistroSpec::from_container(c, &home).ok();
    let name = distro_name(c).unwrap_or_else(|| c.id());
    let journal = read_journal(name).ok();
    // The journal is the truth; the container is the fallback for a
    // pre-journal distro awaiting adoption.
    let spec = journal.as_ref().or(spec.as_ref());
    let resolved = spec.map(|s| s.resolve().0);
    let origin = journal.as_ref().and_then(|s| s.origin.clone());
    let (automounts_list, effective_mounts) =
        resolved.as_ref().map(share_lists).unwrap_or_default();
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
        mounts: resolved
            .as_ref()
            .map(|r| r.mounts.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        automounts: automounts_list,
        effective_mounts,
        automount: resolved.as_ref().map(|r| r.automount.as_str().to_string()),
        preset: resolved.as_ref().map(|r| r.preset.as_str().to_string()),
        ports: resolved
            .as_ref()
            .map(|r| r.publish.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        restricted: resolved.is_some_and(|r| r.is_restricted()),
        source: origin
            .as_ref()
            .and_then(|o| origin_source_display(&o.source)),
        origin,
    }
}

/// The derived shares for a `list` row: the `/Volumes` automounts the
/// spec's mode would mount now, then the home mount rendered as
/// `HOME:HOME[:ro]`. Everything `container create` mounts beyond the
/// user's own list.
fn share_lists(r: &ResolvedSpec) -> (Vec<String>, Vec<String>) {
    let mut auto: Vec<String> = auto_shares(r)
        .unwrap_or_default()
        .iter()
        .map(ToString::to_string)
        .collect();
    if r.home_mount != HomeMount::None
        && let Ok(home) = host_home()
    {
        let ro = if r.home_mount == HomeMount::Ro {
            ":ro"
        } else {
            ""
        };
        auto.push(format!("{home}:{home}{ro}"));
    }
    let mut effective: Vec<String> = r.mounts.iter().map(ToString::to_string).collect();
    effective.extend(auto.iter().cloned());
    (auto, effective)
}

/// A `list` row for a journaled (stopped) distro — assembled from its
/// spec, since there is no container to inspect.
fn journal_summary(name: &str, default: Option<&str>) -> Option<DistroSummary> {
    let spec = read_journal(name).ok()?;
    let disk = journal_fs_path(name)
        .ok()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| std::os::unix::fs::MetadataExt::blocks(&m) * 512);
    let (r, _) = spec.resolve();
    let (automounts_list, effective_mounts) = share_lists(&r);
    Some(DistroSummary {
        id: name.to_string(),
        status: if distro_busy(name) {
            "busy".into()
        } else {
            "stopped".into()
        },
        default: default == Some(name),
        cpus: r.cpus,
        memory: r.memory.as_deref().and_then(size_bytes),
        disk_size: disk,
        image: Some(spec.image.clone()),
        home_mount: Some(r.home_mount.as_str().to_string()),
        mounts: spec.mounts.iter().map(ToString::to_string).collect(),
        automounts: automounts_list,
        effective_mounts,
        automount: Some(r.automount.as_str().to_string()),
        preset: Some(r.preset.as_str().to_string()),
        ports: spec.publish.iter().map(ToString::to_string).collect(),
        restricted: r.is_restricted(),
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
        OriginSource::Download { name, .. } | OriginSource::CatalogImage { name } => {
            Some(format!("catalog:{name}"))
        }
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
                .and_then(|b| serde_json::from_slice::<JournalSpec>(&b).ok())
                .map(|j| j.into_spec().0.image);
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
    let lock = DistroLock::acquire(name)?;
    let id = match converge(name, &lock)? {
        Some(info) => info.id().to_string(),
        None => {
            let spec = lock.spec()?;
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
                // Last chance to capture guest config — `cp` needs a
                // running container.
                if let Ok(spec) = lock.spec() {
                    refresh_conf_snapshots(n, info.id(), &spec);
                }
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
        for f in [
            "rootfs.ext4",
            "spec.json",
            "wsl.conf",
            "wsl-distribution.conf",
        ] {
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

/// Delete every distro — `delete --all`, the package uninstaller's
/// cleanup path. Names are the union of journaled distros (stopped
/// distros exist only there) and labeled containers (pre-journal
/// leftovers exist only there); `delete` handles both shapes.
pub fn delete_all(force: bool) -> Result<()> {
    let mut names = journal_names()?;
    names.extend(distros()?.iter().filter_map(distro_name).map(String::from));
    names.sort();
    names.dedup();
    delete(force, &names)
}

/// Default working directory: the host cwd's guest path if it is shared
/// (via the home mount or an extra mount), else the guest home.
fn default_workdir(r: &ResolvedSpec, home: &str) -> String {
    env::current_dir()
        .ok()
        .and_then(|cwd| r.guest_path(&cwd, home))
        .unwrap_or_else(|| r.user.guest_home())
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
    let workdir = o
        .workdir
        .unwrap_or_else(|| default_workdir(&spec.resolve().0, &home));
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
/// pre-#32 `import`s set on their per-name snapshot images (C11).
/// Content-keyed shared images carry no name label — they're not any
/// one distro's to delete — so unlabeled images are left for
/// `container image prune`, as before.
fn is_our_image(image: &container::ImageListEntry, name: &str) -> bool {
    label_lookup(&image.labels(), LABEL_DISTRO).is_some_and(|v| v == name)
}

/// Best-effort removal of `name`'s imported-image tags, except `keep`.
/// Two claim shapes: the `local/distro-<name>:` tag prefix (current)
/// and the `distro` label pre-#32 imports set. `image delete` on a tag
/// untags only that reference — the shared `local/rootfs:*` tag and
/// other importers' tags survive; the unused-tag prune frees the
/// content once nothing references it.
fn remove_snapshot_images(name: &str, keep: Option<&str>) {
    let Some(images) = container::list_images() else {
        return;
    };
    let prefix = format!("{}:", snapshot_repo(name));
    for image in &images {
        let reference = &image.configuration.name;
        let short = reference.strip_prefix("docker.io/").unwrap_or(reference);
        let claimed = is_our_image(image, name) || short.starts_with(&prefix);
        if claimed && Some(short) != keep {
            release_imported_image(reference);
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

/// Whether a `container machine` named `name` exists.
fn machine_taken(name: &str) -> Result<bool> {
    Ok(container::list_machines()?.iter().any(|m| m.id == name))
}

/// `cm` resolves a name that exists as both a machine and a distro to
/// the distro — creating a distro on an existing machine name silently
/// shadows it, so the collision is refused unless `force` opts in
/// (and still warns: the shadow is real). Public for `catalog_ops`,
/// which checks a resolved name before staging an import.
pub fn bail_machine_shadow(name: &str, force: bool) -> Result<()> {
    if !machine_taken(name)? {
        return Ok(());
    }
    if !force {
        bail!(
            "a machine named `{name}` already exists — a same-named distro would \
             shadow it in `cm` (pick another name, or use --force)"
        );
    }
    eprintln!("container-distro: `{name}` is also a machine; `cm` resolves the name to the distro");
    Ok(())
}

/// The other direction: a distro hides a same-named machine from every
/// `cm` name lookup, so creating the machine is refused unless `force`
/// opts in (and still warns). Public for `cm`'s argv-built
/// `machine create` path, which never passes through `import_machine`.
pub fn bail_distro_shadow(name: &str, force: bool) -> Result<()> {
    if !distro_exists(name)? {
        return Ok(());
    }
    if !force {
        bail!(
            "a distro named `{name}` already exists — `cm` resolves that name to \
             the distro (pick another name, or use --force)"
        );
    }
    eprintln!("container-distro: `{name}` is also a distro; `cm` resolves the name to the distro");
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
        write_nofollow(&spec_path, &serde_json::to_vec_pretty(&spec.to_journal())?)?;
        set_locked(&spec_path, true)?;
        set_locked(&dir, true)
    })
}

/// The journaled spec — the distro's trusted desired configuration.
/// The journal records intent plus the effective values at write
/// time; the intent is authoritative. Pre-preset journals get tier
/// and intent back-computed, derived automount entries a historic
/// journal recorded are dropped (automounts are computed, not
/// stored), and a recorded value that no longer matches the
/// resolution — a hand-edit, or a change in the rules — is noted
/// rather than silently adopted (the next write heals the record).
fn read_journal(name: &str) -> Result<DistroSpec> {
    let journal: JournalSpec = serde_json::from_slice(
        &fs::read(journal_spec_path(name)?).context("failed to read the distro journal")?,
    )
    .context("invalid distro journal")?;
    if journal.name != name {
        bail!(
            "journal for `{name}` names `{}` — refusing to trust it",
            journal.name
        );
    }
    let (spec, notes) = journal.into_spec();
    for n in notes {
        eprintln!("container-distro: {n}");
    }
    Ok(spec)
}

/// The guest config files snapshotted into a journal:
/// `(guest path, journal path)`.
const CONF_FILES: [(&str, &str); 2] = [
    ("/etc/wsl.conf", "wsl.conf"),
    ("/etc/wsl-distribution.conf", "wsl-distribution.conf"),
];

/// The journaled snapshot of a guest config file.
fn journal_conf_path(name: &str, which: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join(which))
}

/// Copy `guest` out of the running container `id` with `container cp`
/// and return it, capped at 64 KiB. `Ok(None)` means the file doesn't
/// exist in the guest; other failures are errors for the caller to
/// warn about. `cp` is runtime-mediated — the file is read through the
/// container's rootfs without running any guest binary, so an image
/// can't control how its config is extracted (or feed us a `cat` it
/// ships).
fn cp_out(id: &str, guest: &str) -> Result<Option<Vec<u8>>> {
    let tmp = tempfile::tempdir().context("failed to create a snapshot scratch dir")?;
    let dst = tmp.path().join("conf");
    let out = container_cmd()
        .arg("cp")
        .arg(format!("{id}:{guest}"))
        .arg(&dst)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // vminitd reports ENOENT for a missing guest path — treat it
        // as "absent" but leave other failures (container stopped
        // between our check and the copy, a directory at the path)
        // for the caller.
        if stderr.contains("not found") || stderr.contains("No such file") {
            return Ok(None);
        }
        bail!("container cp {guest}: {}", stderr.trim());
    }
    let mut text = fs::read(&dst)?;
    text.truncate(64 * 1024);
    Ok(Some(text))
}

/// Refresh `name`'s guest-config snapshots from its running container
/// `id`. Called at boot (after the container starts) and before every
/// host-initiated stop — the only windows a `cp` can run, since it
/// needs a running container. A distro stopped by bare
/// `container stop` keeps its last snapshot — WSL's restart-to-apply
/// model makes that its effective config anyway. Skipped entirely
/// when the distro doesn't read its guest config. Missing guest files
/// clear any stale snapshot; an ambiguous `cp` failure keeps the old
/// one and warns rather than silently swapping in nothing.
fn refresh_conf_snapshots(name: &str, id: &str, spec: &DistroSpec) {
    if spec.resolve().0.wsl_conf.is_empty() {
        return;
    }
    for (guest, which) in CONF_FILES {
        match cp_out(id, guest) {
            Ok(Some(text)) => {
                let p = journal_conf_path(name, which);
                let r = p.and_then(|p| {
                    with_journal_dir_unlocked(name, || {
                        unlock(&p);
                        write_nofollow(&p, &text)?;
                        set_locked(&p, true)
                    })
                });
                if let Err(e) = r {
                    eprintln!(
                        "container-distro: warning: can't snapshot {guest} for `{name}`: {e:#}"
                    );
                }
            }
            Ok(None) => {
                if let Ok(p) = journal_conf_path(name, which)
                    && p.exists()
                {
                    let r = with_journal_dir_unlocked(name, || {
                        unlock(&p);
                        fs::remove_file(&p)
                            .with_context(|| format!("failed to remove {}", p.display()))
                    });
                    if let Err(e) = r {
                        eprintln!(
                            "container-distro: warning: can't clear `{name}`'s stale {which} snapshot: {e:#}"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("container-distro: warning: can't copy {guest} out of `{name}`: {e:#}")
            }
        }
    }
}

/// Write `name`'s config snapshots from text already extracted —
/// archive imports at create time, where the distro may never boot
/// (`--no-boot`). Entries that are `None` clear a stale file instead.
fn write_conf_snapshots(
    name: &str,
    wsl_conf: Option<&str>,
    distribution_conf: Option<&str>,
) -> Result<()> {
    let texts = [wsl_conf, distribution_conf];
    for (text, (_, which)) in texts.iter().zip(CONF_FILES.iter()) {
        let p = journal_conf_path(name, which)?;
        with_journal_dir_unlocked(name, || {
            unlock(&p);
            match text {
                Some(t) => {
                    write_nofollow(&p, t.as_bytes())?;
                    set_locked(&p, true)
                }
                None => match fs::remove_file(&p) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    r => r.with_context(|| format!("failed to remove {}", p.display())),
                },
            }
        })?;
    }
    Ok(())
}

/// True when `conf` (the text of a `wsl.conf`, if any) declares
/// `[automount] enabled=false` — the only `wsl.conf` key the host
/// reads, and only ever to *reduce* sharing. Kept pure for tests.
fn wsl_automount_disabled(conf: Option<&str>) -> bool {
    conf.and_then(|c| wsl::ini_value(c, "automount", "enabled"))
        .is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

/// Record the package's `automount.enabled=false` as *source* intent —
/// it caps `automount`/`home_mount` at `none` exactly the way the spec
/// model resolves everything else: explicit `--automount`/`--home-mount`
/// always win (they outrank source), preset-derived defaults like
/// `--trusted`'s rw home get lowered (they're defaults, not intent).
fn lower_sharing(spec: &mut DistroSpec) {
    spec.source.automount = Some(Automount::None);
    spec.source.home_mount = Some(HomeMount::None);
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
    let js: JournalSpec = serde_json::from_slice(
        &fs::read(&journal)
            .with_context(|| format!("`{name}`'s interrupted `set` journal is unreadable"))?,
    )
    .context("invalid `set` journal")?;
    let (spec, _) = js.into_spec();
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
        let data = serde_json::to_vec_pretty(&spec.to_journal())?;
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
    let (resolved, _) = spec.resolve();
    with_journal_dir_unlocked(name, || write_assets(&assets, resolved.admin))?;
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
fn spec_diff(want: &ResolvedSpec, actual: &ResolvedSpec) -> Vec<String> {
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
    field!("preset", want.preset.as_str(), actual.preset.as_str());
    // Unpinned resources are "the host picks" — `create` materializes
    // them into the container config, and a reconstructed spec can't
    // tell a host-picked 7 from a pinned one, so any value is
    // consistent with a `None` journal. Only a pin can drift.
    if let Some(w) = want.cpus
        && actual.cpus != Some(w)
    {
        out.push(format!(
            "cpus: journal {w}, running {}",
            actual.cpus.map_or("default".to_string(), |c| c.to_string())
        ));
    }
    // Both sides of a memory compare go through `size_bytes` so unit
    // spelling doesn't matter; an unparseable side falls back to a
    // string compare.
    let mem = |o: &Option<String>| o.as_deref().and_then(size_bytes);
    if let Some(w) = &want.memory {
        let drifts = match (mem(&want.memory), mem(&actual.memory)) {
            (Some(w), Some(a)) => w != a,
            _ => Some(w.as_str()) != actual.memory.as_deref(),
        };
        if drifts {
            out.push(format!(
                "memory: journal {w}, running {}",
                opt(&actual.memory)
            ));
        }
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
    field!("wsl-conf", want.wsl_conf, actual.wsl_conf);
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
            for d in spec_diff(&want.resolve().0, &actual.resolve().0) {
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

/// `container distro set`: update `name`'s journaled spec. A running
/// distro is stopped and rebooted under the new settings; a stopped one
/// is just re-journaled — its container is fresh-created at the next
/// start either way. Conceptually stop → update → start, so an
/// interruption lands in the same resting state a normal stop does.
/// Like `machine boot`, a failed materialize leaves the distro stopped
/// with the new spec committed; there is no stale config to fall back
/// to.
///
/// Tier and resets resolve first (`--preset T` re-derives the
/// unpinned options; `--reset <opt>` un-pins one; `--reset <tier>`
/// clears every pin), then the per-option flags pin on top, so a
/// report of the resulting spec can say what changed and what the
/// user's own flags still pin.
pub fn set(name: &str, changes: &SpecChanges) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    if changes.is_empty() {
        bail!("nothing to change (see `container distro set --help`)");
    }
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    let old = lock.spec()?;
    let (new, warnings) = changes.apply(&old)?;
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    for line in new.change_report(&old) {
        println!("{line}");
    }
    warn_public_publish(&changes.add_publish);
    check_sources(&new)?;
    let info = find(name).ok();
    let was_running = info.as_ref().is_some_and(|i| i.is_running());
    if let Some(info) = &info {
        if was_running {
            refresh_conf_snapshots(name, info.id(), &old);
            run_container_quiet(&["stop", info.id()])?;
        }
        dismantle(name, info.id(), &lock)?;
    }
    // Turning config honoring off also forgets what was captured —
    // a distro that ignores wsl.conf should hold no guest config.
    if new.resolve().0.wsl_conf.is_empty() && !old.resolve().0.wsl_conf.is_empty() {
        let _ = write_conf_snapshots(name, None, None);
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
        if let Ok(spec) = lock.spec() {
            refresh_conf_snapshots(name, info.id(), &spec);
        }
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
/// `imported-from` label and the journal's `origin` — a local path for
/// `Local`, the download itself for `Download`. `Download` also
/// supplies the verified rootfs SHA-256 the shared image is keyed and
/// labeled by; `Local` hashes the file.
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

/// A SHA-256 in cache-filename form: lowercase hex, `0x` stripped —
/// the form the shared image's reference and `rootfs-sha256` label
/// use. (Mirrors `catalog_ops::normalize_sha`.)
fn normalized_sha(s: &str) -> String {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
        .to_lowercase()
}

// Imported rootfs images carry two tags: the shared
// `local/rootfs:<sha256>` dedup tag and a per-importer
// `local/{distro,machine}-<name>:imported-<sha256>` tag the
// journal/machine records and `rm`/`--unregister` untags. 64 hex
// chars fit well under the 128-char tag limit.

/// The shared content-keyed tag: any later import of the same archive
/// — another distro, a machine, a retry after a failed create — reuses
/// it instead of re-running the `.wsl`→OCI wrap and `image load`,
/// which often outlasts the download itself.
fn imported_image_ref(sha256: &str) -> String {
    format!("local/rootfs:{sha256}")
}

/// The per-distro tag on an imported image (`snapshot_repo` keeps the
/// `local/distro-<name>` prefix `is_generated_image` and the rename
/// warning recognize).
fn distro_import_ref(name: &str, sha256: &str) -> String {
    format!("{}:imported-{sha256}", snapshot_repo(name))
}

/// The per-machine tag on an imported image.
fn machine_import_ref(name: &str, sha256: &str) -> String {
    format!("local/machine-{name}:imported-{sha256}")
}

/// The `<sha>` of an `*:imported-<sha>` tag when the suffix parses —
/// `imported-<ts>` refs from pre-#32 imports don't qualify (their
/// content tag didn't exist).
fn imported_ref_sha(reference: &str) -> Option<&str> {
    let sha = reference.rsplit_once(":imported-")?.1;
    (sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha)
}

/// `container image tag` — a new reference on an existing image.
fn tag_image(source: &str, target: &str) -> Result<()> {
    run_container_quiet(&["image", "tag", source, target])
}

/// Untag `reference`: `image delete` removes just that state entry —
/// other tags of the same image, and its blobs while any tag remains,
/// are untouched. Best-effort.
fn untag_image(reference: &str) {
    let _ = container_cmd()
        .args(["image", "delete", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Whether `reference` already names a local image. A listing failure
/// reads as a miss — the cost is a redundant `image load` of identical
/// content, nothing worse.
fn image_cached(reference: &str) -> bool {
    container::list_images().is_some_and(|imgs| {
        imgs.iter()
            .any(|i| container::same_image(&i.configuration.name, reference))
    })
}

/// Cap on shared imported-rootfs images no importer references, the
/// image-side analogue of the `.wsl` download cache's budget — oldest
/// unused tags are evicted once the set is over it.
const MAX_IMPORTED_IMAGE_BYTES: u64 = 8 << 30;

/// Load `rootfs` as an image under the shared content tag unless that
/// tag already exists; returns the shared reference. Shared images
/// carry `imported-from`/`rootfs-sha256` provenance but deliberately
/// no `distro`/`machine` name label: the label is per-image, not
/// per-tag, so a name on it would let one importer's cleanup claim
/// tags another importer still uses.
fn ensure_rootfs_image(rootfs: &Path, sha256: &str, labels: &[(String, String)]) -> Result<String> {
    let reference = imported_image_ref(sha256);
    if image_cached(&reference) {
        eprintln!("  reusing imported image {reference}");
    } else {
        oci::load_rootfs(rootfs, &reference, labels)?;
        // A fresh load may itself push the unused set over budget.
        prune_imported_images();
    }
    Ok(reference)
}

/// Tag the shared `shared` image as `per_ref` (a
/// `distro_import_ref`/`machine_import_ref`) — the recorded, prunable
/// ref. Falls back to the shared ref when `image tag` isn't available.
fn per_importer_ref(shared: &str, per_ref: &str) -> String {
    match tag_image(shared, per_ref) {
        Ok(()) => per_ref.to_string(),
        Err(e) => {
            eprintln!(
                "container-distro: warning: `container image tag` failed ({e:#}); \
                 recording the shared reference {shared}"
            );
            shared.to_string()
        }
    }
}

/// Drop `per_ref` (an `*:imported-<sha>` tag), then run the unused-tag
/// prune — its shared tag may have just become evictable.
fn release_imported_image(per_ref: &str) {
    untag_image(per_ref);
    prune_imported_images();
}

/// Untag `local/machine-<name>:*` image tags — the import bookkeeping
/// `machine rm` doesn't know about — after the machine is gone.
/// Best-effort; a stale tag just keeps its shared image listed as
/// in-use until pruned.
pub fn release_machine_images(name: &str) {
    let Some(images) = container::list_images() else {
        return;
    };
    let prefix = format!("local/machine-{name}:");
    for i in &images {
        if i.configuration.name.starts_with(&prefix) {
            release_imported_image(&i.configuration.name);
        }
    }
}

/// Evict `local/rootfs:*` tags no `*:imported-<sha>` ref still claims,
/// oldest first, until the unused set is under
/// `MAX_IMPORTED_IMAGE_BYTES` — the image-side analogue of the `.wsl`
/// cache's `prune_cache`. Best-effort.
fn prune_imported_images() {
    let Some(images) = container::list_images() else {
        return;
    };
    let mut unused: Vec<(&container::ImageListEntry, u64)> = images
        .iter()
        .filter_map(|i| {
            let sha = i.configuration.name.strip_prefix("local/rootfs:")?;
            let suffix = format!(":imported-{sha}");
            if images
                .iter()
                .any(|o| o.configuration.name.ends_with(&suffix))
            {
                return None;
            }
            let size: u64 = i.variants.iter().map(|v| v.size.max(0) as u64).sum();
            Some((i, size))
        })
        .collect();
    // Oldest tag first; a missing creationDate sorts as epoch.
    unused.sort_by(|a, b| {
        a.0.configuration
            .creation_date
            .cmp(&b.0.configuration.creation_date)
    });
    let mut total: u64 = unused.iter().map(|(_, s)| s).sum();
    for (entry, size) in unused {
        if total <= MAX_IMPORTED_IMAGE_BYTES {
            break;
        }
        untag_image(&entry.configuration.name);
        total = total.saturating_sub(size);
    }
}

/// Create a distro from a rootfs tar; returns its name. `requested`
/// is the source side of the spec's intent, as in [`create`]. `pkg`
/// carries the archive's `wsl.conf`/`wsl-distribution.conf` when the
/// caller already scanned it (catalog imports name the distro from
/// it); otherwise the archive is scanned here — one pass either way.
pub fn import(
    name: &str,
    file: &Path,
    opts: &CreateOptions,
    source: ImportSource<'_>,
    requested: &Intent,
    pkg: Option<&wsl::PackageConf>,
) -> Result<String> {
    validate_name(name)?;
    ensure_started()?;
    if distro_exists(name)? {
        bail!("distro `{name}` already exists");
    }
    // Checked here too so the conversion can't start only to have the
    // create refused on leftover state; `create_from_spec` re-checks
    // under the distro lock.
    bail_if_staged(name)?;
    let stdin_copy;
    let rootfs = if file == Path::new("-") {
        stdin_copy = oci::stdin_to_tempfile()?;
        stdin_copy.path()
    } else {
        file
    };
    // Content key for the shared import image: a catalog download's
    // verified hash is already known; a local file's takes one extra
    // read pass — cheap next to the OCI wrap a hit can skip.
    let sha256 = match &source {
        ImportSource::Download { sha256, .. } => normalized_sha(sha256),
        ImportSource::Local => oci::hash_file(rootfs)?,
    };
    // Provenance labels, no per-distro name label — see
    // `ensure_rootfs_image`. `imported-from` records the first
    // importer's source; the journal's `origin` tracks each distro's.
    let imported_from = match source {
        ImportSource::Local if file == Path::new("-") => "-".to_string(),
        ImportSource::Local => file.to_string_lossy().into_owned(),
        ImportSource::Download { url, .. } => url.to_string(),
    };
    let labels = [
        (label_key(LABEL_ROOTFS_SHA256), sha256.clone()),
        (label_key(LABEL_IMPORTED_FROM), imported_from),
    ];
    let shared = ensure_rootfs_image(rootfs, &sha256, &labels)?;
    // The journal records the per-distro tag — what `rm` untags — on
    // top of the shared content tag the dedup keys off.
    let reference = per_importer_ref(&shared, &distro_import_ref(name, &sha256));
    // The archive's config files are guest input: extract them while
    // the tar is already open, for the automount default below and —
    // when the distro honors guest config — the journal snapshots. An
    // unreadable tar is `load_rootfs`'s error; a scan failure just
    // means no config.
    let scanned;
    let pkg = match pkg {
        Some(p) => Some(p.clone()),
        None => {
            scanned = wsl::package_conf(rootfs).ok();
            scanned
        }
    };
    // A catalog download is a vetted package — its config is trusted
    // like `--trusted`'s, expressed as source intent so an explicit
    // `--use-wsl-conf none` still wins.
    let mut requested = requested.clone();
    if matches!(source, ImportSource::Download { .. }) {
        requested.wsl_conf = Some(WslConfMode::all());
    }
    let mut spec = build_spec(name.to_string(), reference, opts, &requested)?;
    // `[automount] enabled=false` lowers the sharing defaults — only
    // where the distro honors its packaged config at all, and only
    // down (source intent loses to every explicit flag). Image-based
    // `create` can't see the file before first boot; an archive can.
    if spec.resolve().0.wsl_conf.contains(WslConf::AUTOMOUNT)
        && wsl_automount_disabled(pkg.as_ref().and_then(|p| p.wsl_conf.as_deref()))
    {
        lower_sharing(&mut spec);
    }
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
    match create_from_spec(&spec, opts) {
        Ok(name) => {
            // Snapshot the archive's config into the journal — the
            // `--no-boot` import lands with them in place; a booted
            // distro refreshes them from the live guest file anyway.
            if !spec.resolve().0.wsl_conf.is_empty()
                && let Some(p) = &pkg
                && (p.wsl_conf.is_some() || p.distribution_conf.is_some())
                && let Err(e) = write_conf_snapshots(
                    &name,
                    p.wsl_conf.as_deref(),
                    p.distribution_conf.as_deref(),
                )
            {
                eprintln!(
                    "container-distro: warning: can't snapshot `{name}`'s packaged config: {e:#}"
                );
            }
            Ok(name)
        }
        Err(e) => {
            // The shared tag stays — a cache entry a retry reuses —
            // but the per-distro tag belongs to a distro that never
            // landed.
            if spec.image != shared {
                untag_image(&spec.image);
            }
            Err(e)
        }
    }
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
pub fn import_machine(name: &str, file: &Path, force: bool) -> Result<()> {
    validate_name(name)?;
    ensure_started()?;
    if container::list_machines()?.iter().any(|m| m.id == name) {
        bail!("a machine named `{name}` already exists");
    }
    // A same-named distro would shadow the machine everywhere in `cm`.
    bail_distro_shadow(name, force)?;
    let stdin_copy;
    let rootfs = if file == Path::new("-") {
        stdin_copy = oci::stdin_to_tempfile()?;
        stdin_copy.path()
    } else {
        file
    };
    // Content-keyed like distro imports — a machine imported from the
    // same archive a distro used (or a retry after a failed create)
    // skips the wrap+load entirely.
    let sha256 = oci::hash_file(rootfs)?;
    let labels = [
        (label_key(LABEL_ROOTFS_SHA256), sha256.clone()),
        (
            label_key(LABEL_IMPORTED_FROM),
            if file == Path::new("-") {
                "-".to_string()
            } else {
                file.to_string_lossy().into_owned()
            },
        ),
    ];
    let shared = ensure_rootfs_image(rootfs, &sha256, &labels)?;
    // The machine records its own `local/machine-<name>` tag on the
    // shared image — the tag `--unregister` untags.
    let reference = per_importer_ref(&shared, &machine_import_ref(name, &sha256));
    // `machine create` boots the machine; its internal boot absorbs the
    // first-boot provisioning reboot. `--no-boot` is avoided on purpose:
    // a `machine run` on a never-booted machine races the boot and fails
    // every time (upstream apple/container#2024-adjacent). A failed create keeps the
    // shared tag — a cache entry — but drops the per-machine one, which
    // belongs to a machine that never landed.
    if let Err(e) = run_container(&["machine", "create", "--name", name, &reference]) {
        if reference != shared {
            untag_image(&reference);
        }
        return Err(e);
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
    // Keeping the source means the result can't take its name — the
    // same-name machine+distro pair is exactly the shadow the create
    // paths refuse.
    if keep && target.is_none() {
        bail!("`--keep` needs a target name (`-n NAME`) — the source keeps its name");
    }
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
    let user = match &m.user_setup {
        Some(u) => HostUser {
            name: u.username.clone(),
            uid: u.uid,
            gid: u.gid,
        },
        None => host_user()?,
    };
    let image = m
        .image
        .as_ref()
        .map(|i| i.reference.clone())
        .with_context(|| "machine has no image reference".to_string())?;
    // The machine's recorded settings are source intent: they lose to
    // the user's flags and the preset, and resurface on `--reset`.
    let source = Intent {
        home_mount: Some(m.home_mount.as_deref().unwrap_or("rw").parse()?),
        cpus: m.cpus,
        memory: m.memory.map(|b| format!("{}M", b / (1024 * 1024))),
        ..Intent::default()
    };
    let mut spec = DistroSpec {
        name: name.to_string(),
        image,
        mounts: opts.volumes.clone(),
        publish: opts.publish.clone(),
        user,
        preset: opts.preset(),
        explicit: create_intent(opts),
        source,
        admin_grant: false,
        origin: None,
    };
    let (resolved, warnings) = spec.resolve();
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    spec.admin_grant = resolved.admin;
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
    // The source machine's own name is taken over — a plain migration
    // removes it. A *different* machine's name (or the source's, kept)
    // is a shadow the distro would cast over it in `cm`.
    if container::list_machines()?
        .iter()
        .any(|m| m.id == name && (m.id != machine || keep))
    {
        bail_machine_shadow(name, opts.force)?;
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
/// `migrate --out` accepts set-style changes the machine can honor
/// (`--cpus`, `--memory`, `--home-mount`, `--ssh`, `--preset`/
/// `--restricted`/`--trusted` — they shift what the resolution
/// produces for the machine's own options) and refuses the ones it
/// can't, so a flag never silently no-ops.
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
    // `--ssh` is a redundant opt-in the machine honors; `--no-ssh`
    // pins something it can never do.
    if opts.no_ssh {
        bad.push("--no-ssh");
    }
    if opts.sudo || opts.no_sudo {
        bad.push("--sudo/--no-sudo");
    }
    // `--ignore-wsl-conf` is fine — a machine never honors the file
    // anyway, so ignoring it asks for nothing the machine doesn't do.
    if opts.use_wsl_conf.is_some() {
        bad.push("--use-wsl-conf");
    }
    if !bad.is_empty() {
        bail!(
            "{} cannot be applied to a machine (supported overrides: \
             --cpus, --memory, --home-mount, --ssh, --preset/--restricted/--trusted)",
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
/// stopped by the caller for the filesystem swap. A machine forwards
/// SSH_AUTH_SOCK at every boot with no opt-out, so only a distro that
/// resolves to agent-forwarding may migrate out — matching the `cm
/// --install --machine` refusal.
pub fn create_machine(
    name: &str,
    image: &str,
    spec: &DistroSpec,
    opts: &CreateOptions,
) -> Result<()> {
    // `opts` applies as set-style changes to a scratch copy of the
    // spec — the journal is never written, so `--keep` can't leak a
    // machine-shaped tweak back into the distro. A machine forwards
    // SSH_AUTH_SOCK at every boot with no opt-out, so migrating out is
    // itself the opt-in: warn when `ssh` lands off, and refuse only an
    // explicit `--no-ssh` pin (`reject_machine_opts` refuses an
    // `opts` one; a pin recorded on the distro still blocks).
    reject_machine_opts(opts)?;
    let changes = opts.spec_changes();
    let (result, warnings) = changes.apply(spec)?;
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    let (resolved, _) = result.resolve();
    if !resolved.ssh {
        if result.explicit.ssh == Some(false) {
            bail!(
                "`container machine` always forwards your SSH agent — {name} has \
                 agent forwarding pinned off; `distro set --ssh` it first (or \
                 drop the pin with `set --reset ssh`)"
            );
        }
        eprintln!("container-distro: `container machine` forwards your SSH agent at every boot");
    }
    let mut a: Vec<String> = ["machine", "create", "--name", name]
        .map(String::from)
        .to_vec();
    if let Some(c) = resolved.cpus {
        a.extend(["--cpus".into(), c.to_string()]);
    }
    if let Some(m) = &resolved.memory {
        a.extend(["--memory".into(), m.clone()]);
    }
    a.extend(["--home-mount".into(), resolved.home_mount.as_str().into()]);
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
    // Refuse distro-only flags before the distro is stopped — the same
    // check runs inside `create_machine` for the shared entry point.
    reject_machine_opts(opts)?;
    // Keeping the source means the result can't take its name — the
    // same-name distro+machine pair is exactly the shadow the create
    // paths refuse.
    if keep && target.is_none() {
        bail!("`--keep` needs a target name (`-n NAME`) — the source keeps its name");
    }
    let name = target.unwrap_or_else(|| distro.to_string());
    validate_name(&name)?;
    if container::list_machines()?.iter().any(|m| m.id == name) {
        bail!("a machine named `{name}` already exists (use `-n` to pick another)");
    }
    // The source distro's own name is taken over — a plain migration
    // removes it. A *different* distro's name (or the source's, kept)
    // would hide the new machine behind it in `cm`.
    if name != distro || keep {
        bail_distro_shadow(&name, opts.force)?;
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
        refresh_conf_snapshots(distro, info.id(), &spec);
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
    // An imported distro's image ref is the *distro's* tag on shared
    // content — `rm` untags it below. Give the machine its own
    // `local/machine-*` tag on the same image first so its recorded
    // reference stays valid; untag it again if the create fails.
    let machine_tag = imported_ref_sha(&image).map(|sha| machine_import_ref(&name, sha));
    let image = match &machine_tag {
        Some(t) if tag_image(&image, t).is_ok() => t.clone(),
        Some(_) => {
            eprintln!(
                "container-distro: warning: couldn't tag `{image}` for machine `{name}` — \
                 its recorded reference goes stale when `{distro}` is removed"
            );
            image
        }
        None => image,
    };
    if let Err(e) = create_machine(&name, &image, &spec, opts) {
        if image != spec.image {
            untag_image(&image);
        }
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

/// Rename a distro (`container distro rename`): the journal — spec,
/// disk, and init assets — moves to `new`, and a distro that was
/// running is rematerialized under the new name once the move lands.
/// The ephemeral container is never renamed (the logical name lives in
/// its label, not the runtime id); it's converged away before the move
/// and recreated fresh after, so nothing container-side ever carries
/// `old` past the journal swap.
///
/// A same-named *machine* is refused unless `force` — `cm` would
/// resolve `new` to the distro, silently shadowing it.
pub fn rename(old: &str, new: &str, force: bool) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    validate_name(new)?;
    if old == new {
        bail!("`{old}` is already the distro's name");
    }
    ensure_journal(old)?;
    if distro_exists(new)? {
        bail!("distro `{new}` already exists");
    }
    bail_if_staged(new)?;
    bail_machine_shadow(new, force)?;
    // Staged state under `old`'s name would be orphaned by the move —
    // a rare interrupted-`set` leftover for the user to resolve first.
    let staged = preserved_rootfs(old)?;
    if staged.exists() {
        bail!(
            "`{old}` has a staged filesystem from an interrupted `set` — \
             inspect or remove {} before renaming",
            staged.display()
        );
    }
    let lock = DistroLock::acquire(old)?;
    let mut spec = lock.spec()?;
    // Converge to the resting state first: the journal move then
    // carries the disk, and no container is left naming `old`.
    let was_running = if let Some(info) = find(old).ok().filter(|i| i.is_running()) {
        refresh_conf_snapshots(old, info.id(), &spec);
        run_container_quiet(&["stop", info.id()])?;
        true
    } else {
        false
    };
    converge(old, &lock)?;
    spec.name = new.to_string();
    write_journal(new, &spec)?;
    // Move the disk into the new journal: a rename, so an interruption
    // leaves it whole under one name or the other, never a partial
    // copy — the same swap `migrate` and `set` use.
    if let Err(e) = with_journal_dir_unlocked(new, || {
        with_journal_dir_unlocked(old, || {
            let src = journal_fs_path(old)?;
            if src.exists() {
                unlock(&src);
                fs::rename(&src, journal_fs_path(new)?)
                    .with_context(|| format!("failed to move {}", src.display()))?;
            }
            // The guest-config snapshots move with the journal too.
            for (_, which) in CONF_FILES {
                let src = journal_conf_path(old, which)?;
                if src.exists() {
                    unlock(&src);
                    fs::rename(&src, journal_conf_path(new, which)?)
                        .with_context(|| format!("failed to move {}", src.display()))?;
                    set_locked(&journal_conf_path(new, which)?, true)?;
                }
            }
            Ok(())
        })?;
        let dst = journal_fs_path(new)?;
        if dst.exists() {
            set_locked(&dst, true)?;
        }
        Ok(())
    }) {
        // The disk never left `old`; drop the just-written journal so
        // the rename can be retried.
        let _ = remove_journal_dir(new);
        return Err(e);
    }
    remove_journal_dir(old)?;
    if default_name().as_deref() == Some(old) {
        write_default(Some(new))?;
    }
    drop(lock);
    // An imported distro's snapshot image keeps `old` in its reference
    // and labels — cleanup keys off the new name, so `rm` won't find
    // it later. Say so rather than leaking it silently then.
    if spec.image.starts_with(&snapshot_repo(old)) {
        eprintln!(
            "container-distro: image `{}` is still labeled for `{old}` — \
             `rm` won't remove it automatically",
            spec.image
        );
    }
    if was_running {
        start(new)?;
    }
    Ok(())
}

/// Rename a `container machine` (`cm --rename --machine`). Upstream has
/// no rename — the machine's id is its bundle path — so the machine is
/// recreated: `machine create` under `new` with the same image and
/// resources, then the old `rootfs.ext4` is cloned over the new one's
/// (the same disk swap [`migrate_to_machine`] uses). The old machine
/// is removed once the new one exists and its carried disk boots; a
/// default marker is re-pointed at `new`.
///
/// A same-named *distro* is refused unless `force` — `cm` would
/// resolve `new` to the distro, hiding the machine entirely.
pub fn rename_machine(old: &str, new: &str, force: bool) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    validate_name(new)?;
    if old == new {
        bail!("`{old}` is already the machine's name");
    }
    let machines = container::list_machines()?;
    if machines.iter().any(|m| m.id == new) {
        bail!("a machine named `{new}` already exists");
    }
    bail_distro_shadow(new, force)?;
    let detail = container::inspect_machine(old)?;
    let src = machine_rootfs(&detail)?
        .with_context(|| format!("machine `{old}` has no root filesystem to carry"))?;
    let was_default = machines.iter().any(|m| m.id == old && m.default);
    let was_running = detail.is_running();
    if was_running {
        run_container_quiet(&["machine", "stop", old])?;
    }
    let image = detail
        .image
        .as_ref()
        .map(|i| i.reference.clone())
        .context("machine has no image reference")?;
    // An imported machine's image ref is a `local/machine-<old>` tag —
    // retag it under `new` so the recorded reference stays valid once
    // `old`'s tags are released. A failed retag just keeps `old`'s ref.
    let (image, retagged) = match imported_ref_sha(&image) {
        Some(sha) => {
            let t = machine_import_ref(new, sha);
            if tag_image(&image, &t).is_ok() {
                (t, true)
            } else {
                (image, false)
            }
        }
        None => (image, false),
    };
    // From here a failure tries to leave `old` as it was found.
    let renamed = (|| -> Result<()> {
        // `machine create` boots the machine; its internal boot
        // materializes the plugin-state disk the swap replaces below.
        let mut a = vec![
            "machine".to_string(),
            "create".to_string(),
            "--name".to_string(),
            new.to_string(),
        ];
        if let Some(c) = detail.cpus {
            a.extend(["--cpus".into(), c.to_string()]);
        }
        if let Some(m) = detail.memory {
            a.extend(["--memory".into(), format!("{}M", m / (1024 * 1024))]);
        }
        if let Some(h) = &detail.home_mount {
            a.extend(["--home-mount".into(), h.clone()]);
        }
        a.push(image.clone());
        run_container(&a.iter().map(String::as_str).collect::<Vec<_>>())?;
        // Replace the machine's filesystem while it is stopped: clone
        // to a sibling then rename over its disk, so an interruption
        // leaves the old disk or the new one, never a partial file.
        let swapped = (|| -> Result<()> {
            run_container_quiet(&["machine", "stop", new])?;
            let dst = machine_rootfs(&container::inspect_machine(new)?)?
                .context("the new machine has no rootfs to replace")?;
            let mut tmp = dst.clone().into_os_string();
            tmp.push(".new");
            let tmp = PathBuf::from(tmp);
            let _ = fs::remove_file(&tmp);
            // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
            fs::copy(&src, &tmp).with_context(|| format!("failed to clone {}", src.display()))?;
            fs::rename(&tmp, &dst)
                .with_context(|| format!("failed to install {}", dst.display()))?;
            Ok(())
        })();
        if let Err(e) = swapped {
            // `new` still holds only its image filesystem — remove it
            // so a retry isn't blocked, along with the tag we made.
            let _ = run_container_quiet(&["machine", "rm", new]);
            if retagged {
                untag_image(&image);
            }
            return Err(e.context(format!(
                "failed to move `{old}`'s filesystem into machine `{new}`"
            )));
        }
        // `machine create`'s boot ran the image filesystem; the carried
        // disk hasn't run yet. Verify it like `migrate --out` does —
        // the first-boot flake is why it retries.
        let booted = (0..3).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            boot_machine(new).is_ok()
        });
        if !booted {
            bail!(
                "machine `{new}` holds `{old}`'s filesystem but failed to boot; \
                 `{old}` was left in place (see `container machine logs {new}`)"
            );
        }
        if !was_running {
            let _ = run_container_quiet(&["machine", "stop", new]);
        }
        Ok(())
    })();
    if let Err(e) = renamed {
        if was_running {
            let _ = boot_machine(old);
        }
        return Err(e);
    }
    if let Err(e) = run_container_quiet(&["machine", "rm", old]) {
        eprintln!("container-distro: renamed, but removing machine `{old}` failed: {e:#}");
    } else if retagged {
        // The retagged image is recorded under `new`; `old`'s
        // `local/machine-<old>` tag is bookkeeping to release.
        release_machine_images(old);
    }
    if was_default {
        let _ = run_container_quiet(&["machine", "set-default", new]);
    }
    Ok(())
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
    fn imported_ref_sha_parses_only_sha_tags() {
        let sha = "a".repeat(64);
        assert_eq!(
            imported_ref_sha(&distro_import_ref("d1", &sha)),
            Some(sha.as_str())
        );
        assert_eq!(
            imported_ref_sha(&machine_import_ref("m1", &sha)),
            Some(sha.as_str())
        );
        // Pre-#32 timestamp refs and other tags don't qualify.
        assert_eq!(
            imported_ref_sha("local/distro-d1:imported-1728000000"),
            None
        );
        assert_eq!(imported_ref_sha("local/rootfs:abc"), None);
        assert_eq!(imported_ref_sha("alpine:latest"), None);
    }

    #[test]
    fn imported_refs_are_tag_shaped() {
        let sha = "b".repeat(64);
        assert_eq!(imported_image_ref(&sha), format!("local/rootfs:{sha}"));
        assert_eq!(
            distro_import_ref("d1", &sha),
            format!("local/distro-d1:imported-{sha}")
        );
        assert_eq!(
            machine_import_ref("m1", &sha),
            format!("local/machine-m1:imported-{sha}")
        );
        // Every per-importer ref still reads as a generated
        // `local/*-*:imported-*` tag, and fits the 128-char tag limit.
        assert!(distro_import_ref("d1", &sha).len() < 128);
        assert!(machine_import_ref("m1", &sha).len() < 128);
    }

    #[test]
    fn normalized_sha_strips_and_lowercases() {
        let hex = "AbC".repeat(21) + "d";
        assert_eq!(normalized_sha(&format!("0x{hex}")), hex.to_lowercase());
        assert_eq!(normalized_sha(&format!("0X{hex}")), hex.to_lowercase());
        assert_eq!(normalized_sha(&hex), hex.to_lowercase());
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

    /// An intent-only spec whose pins resolve to the shape the old
    /// tests' effective values had.
    fn spec() -> DistroSpec {
        DistroSpec {
            name: "d1".into(),
            image: "alpine:latest".into(),
            mounts: vec!["/a:/b:ro".parse().unwrap()],
            publish: vec!["8080:80".parse().unwrap()],
            user: HostUser {
                name: "dp".into(),
                uid: 501,
                gid: 20,
            },
            preset: Preset::Default,
            explicit: Intent {
                cpus: Some(2),
                memory: Some("4G".into()),
                home_mount: Some(HomeMount::Rw),
                ssh: Some(true),
                admin: Some(true),
                wsl_conf: Some(WslConfMode::all()),
                ..Intent::default()
            },
            source: Intent::default(),
            admin_grant: false,
            origin: None,
        }
    }

    fn resolved(s: &DistroSpec) -> crate::spec::ResolvedSpec {
        s.resolve().0
    }

    /// The start-time integrity check: identical specs produce no diff;
    /// drifted fields name themselves; `4G` vs `4096M` is not drift.
    #[test]
    fn spec_diff_names_drifted_fields() {
        let want = resolved(&spec());
        // `from_container` normalizes memory to `NM` — same bytes is
        // not drift.
        let mut actual = spec();
        actual.explicit.memory = Some("4096M".into());
        assert!(spec_diff(&want, &resolved(&actual)).is_empty());

        let mut actual = spec();
        actual.explicit.cpus = Some(8);
        actual.explicit.ssh = Some(false);
        actual.explicit.home_mount = Some(HomeMount::Ro);
        let diff = spec_diff(&want, &resolved(&actual));
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
        let diff = spec_diff(&want, &resolved(&actual));
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

    /// An unpinned journal resource is "the host picks": whatever the
    /// host materialized into the container (a reconstructed spec
    /// can't distinguish it from a pin) is consistent — only a journal
    /// pin mismatching the running value is drift.
    #[test]
    fn spec_diff_unpinned_resources_dont_drift() {
        let mut want = spec();
        want.explicit.cpus = None;
        want.explicit.memory = None;
        let want = resolved(&want);

        // Host picked 7/32768M — the reconstruction pins the
        // materialized values; not drift.
        let mut actual = spec();
        actual.explicit.cpus = Some(7);
        actual.explicit.memory = Some("32768M".into());
        assert!(spec_diff(&want, &resolved(&actual)).is_empty());

        // A pinned journal value that doesn't match the run is drift.
        let mut want = spec();
        want.explicit.memory = None;
        let want = resolved(&want);
        let diff = spec_diff(&want, &resolved(&actual));
        assert!(diff.iter().any(|d| d.starts_with("cpus:")), "{diff:?}");
        assert!(!diff.iter().any(|d| d.starts_with("memory:")), "{diff:?}");
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

        let spec = spec();
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
            serde_json::from_slice::<JournalSpec>(&fs::read(&spec_p).unwrap())
                .unwrap()
                .into_spec()
                .0,
            spec
        );
        drop(lock);
        // The flock releases with the fd — but a close-time release
        // isn't sequenced with all observable effects of close(2) (the
        // kernel can lag), so poll a fresh opener instead of asserting
        // on the first probe.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let probe = loop {
            match fs::File::open(&spec_p).unwrap().try_lock() {
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                res => break res,
            }
        };
        assert!(probe.is_ok(), "relock probe failed: {probe:?}");
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

    #[test]
    fn home_mount_default_resolution() {
        // An explicit flag wins over everything — even --restricted.
        let opts = CreateOptions {
            home_mount: Some(HomeMount::Rw),
            restricted: true,
            automount: Some(Automount::Ro),
            ..CreateOptions::default()
        };
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::Rw);
        // --restricted pins none.
        let opts = CreateOptions {
            restricted: true,
            automount: Some(Automount::Rw),
            ..CreateOptions::default()
        };
        assert_eq!(
            opts.resolve_home_mount(&Intent::only_home_mount(HomeMount::Rw)),
            HomeMount::None
        );
        // An explicit --automount is followed (and warns).
        for (am, want) in [
            (Automount::Rw, HomeMount::Rw),
            (Automount::Ro, HomeMount::Ro),
            (Automount::None, HomeMount::None),
        ] {
            let opts = CreateOptions {
                automount: Some(am),
                ..CreateOptions::default()
            };
            assert_eq!(opts.resolve_home_mount(&Intent::default()), want, "{am:?}");
        }
        // --trusted presets rw regardless of source, and beats the
        // automount-follows default (ro volumes, still rw home).
        let opts = CreateOptions {
            trusted: true,
            ..CreateOptions::default()
        };
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::Rw);
        let opts = CreateOptions {
            trusted: true,
            automount: Some(Automount::Ro),
            ..CreateOptions::default()
        };
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::Rw);
        // A source request caps what the preset raises — ro stays ro
        // even under --trusted.
        assert_eq!(
            opts.resolve_home_mount(&Intent::only_home_mount(HomeMount::Ro)),
            HomeMount::Ro
        );
        // Otherwise a source request fills the unset slot — the
        // catalog's `rw` survives the `none` built-in default.
        let opts = CreateOptions::default();
        assert_eq!(
            opts.resolve_home_mount(&Intent::only_home_mount(HomeMount::Rw)),
            HomeMount::Rw
        );
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::None);
    }

    #[test]
    fn ssh_default_follows_home_tier() {
        // Catalog-tier rw → agent on; ad-hoc none → off.
        let opts = CreateOptions::default();
        for (default, want) in [(HomeMount::Rw, true), (HomeMount::None, false)] {
            let spec = build_spec(
                "d".into(),
                "img".into(),
                &opts,
                &Intent::only_home_mount(default),
            )
            .unwrap();
            assert_eq!(spec.resolve().0.ssh, want, "{default:?}");
        }
        // --restricted beats an explicit rw home: every hole needs its
        // own flag there.
        let opts = CreateOptions {
            restricted: true,
            home_mount: Some(HomeMount::Rw),
            ..CreateOptions::default()
        };
        assert!(
            !build_spec("d".into(), "i".into(), &opts, &Intent::default())
                .unwrap()
                .resolve()
                .0
                .ssh
        );
        // --trusted presets home rw, automount rw, and the agent.
        let opts = CreateOptions {
            trusted: true,
            ..CreateOptions::default()
        };
        let spec = build_spec("d".into(), "i".into(), &opts, &Intent::default()).unwrap();
        let (r, _) = spec.resolve();
        assert!(r.ssh);
        assert_eq!(r.home_mount, HomeMount::Rw);
        assert_eq!(r.automount, Automount::Rw);
    }

    #[test]
    fn migrate_out_refuses_a_pinned_agent_optout() {
        // A machine forwards SSH_AUTH_SOCK at every boot — `--machine`
        // is the opt-in, so an unpinned ssh-off distro only warns. The
        // one refusal is a distro with `--no-ssh` explicitly recorded.
        let mut spec = spec();
        spec.explicit.home_mount = Some(HomeMount::None);
        spec.explicit.ssh = Some(false);
        let err = create_machine("m", "img", &spec, &CreateOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("pinned off"), "{err}");
        // Without the pin the same spec proceeds past the check to
        // spawn `container` — not unit-testable here.
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
        let (r, _) = spec.resolve();
        assert_eq!(r.cpus, Some(4));
        assert_eq!(r.memory.as_deref(), Some("4096M"));
        assert_eq!(r.home_mount, HomeMount::Ro);
        assert_eq!(
            spec.user,
            HostUser {
                name: "dp".into(),
                uid: 501,
                gid: 20
            }
        );
        // homeMount is ro → the agent default follows the home tier: off.
        assert!(!r.ssh && r.admin && spec.admin_grant);
        assert!(!r.is_restricted());
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
        let (r, _) = spec.resolve();
        assert_eq!(r.cpus, Some(2));
        assert_eq!(r.memory.as_deref(), Some("1G"));
        assert_eq!(r.home_mount, HomeMount::None);
        // --restricted still defaults network/ssh/admin.
        assert_eq!(r.network.as_deref(), Some("none"));
        assert!(!r.ssh && !r.admin);
    }

    #[test]
    fn our_image_by_label() {
        let img = |name: &str, labels: &[(&str, &str)]| container::ImageListEntry {
            configuration: container::ImageListConfig {
                name: name.into(),
                creation_date: None,
            },
            variants: vec![container::ImageVariant {
                config: Some(container::ImageVariantConfig {
                    config: Some(container::ImageUserConfig {
                        labels: labels
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    }),
                }),
                size: 0,
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
        // The set-style subset a machine can honor passes — resources,
        // home-mount, `--ssh`, and the tier flags.
        assert!(
            reject_machine_opts(&CreateOptions {
                cpus: Some(2),
                memory: Some("1G".into()),
                home_mount: Some(HomeMount::Ro),
                ssh: true,
                preset: Some(Preset::Restricted),
                no_boot: true,
                set_default: true,
                ..CreateOptions::default()
            })
            .is_ok()
        );
        // Options a machine can't honor are refused, named in the
        // error — including `--no-ssh`, a pin no machine can keep.
        let err = reject_machine_opts(&CreateOptions {
            volumes: vec!["/a:/b".parse().unwrap()],
            publish: vec!["8080".parse().unwrap()],
            network: Some("none".into()),
            no_ssh: true,
            sudo: true,
            ..CreateOptions::default()
        })
        .unwrap_err()
        .to_string();
        for f in [
            "--volume",
            "--publish",
            "--network",
            "--no-ssh",
            "--sudo/--no-sudo",
        ] {
            assert!(err.contains(f), "{err}");
        }
        // --automount (even bare) is refused.
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
