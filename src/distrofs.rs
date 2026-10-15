/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! container-distro's side of the `distrofs` integration — deployment
//! only. The wire protocol, the client, socket-leaf hygiene, the peer
//! signature check, and the session handshake all live in the
//! separate `distrofs` crate/binary (daphnediane/distrofs); this
//! module only decides *where*
//! things go and *which* binary is trusted to run:
//!
//! * the guest server build this install ships (`guest_binary`) —
//!   materialized into each distro's `sbin.distro` assets;
//! * the host end of the `--publish-socket` relay
//!   (`host_socket_path`, `publish_socket_arg`) — our own
//!   `<darwin temp>/container-distro/s/` layout, stable across releases
//!   so sockets published by older builds still resolve;
//! * the `distrofs` host binary (`client_binary`) — located like any
//!   other helper, then verified (`verify_client`) before it is handed
//!   the session token;
//! * launching the server (`container exec`) and running the client
//!   (`run`) — the socket path, token, and mapped arguments are all
//!   container-distro passes to distrofs; no protocol types cross.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::container::{container_cmd, find_on_path};
use crate::layout;
use crate::naming::APP_NAME;
use crate::oci::hex;
use crate::ops::{self, FilesTarget};
use crate::spec::INIT_DIR;

/// The guest end of the `--publish-socket` relay — where the runtime
/// helper binds inside the distro.
pub const GUEST_SOCKET: &str = "/var/lib/container-distro/distrofs.sock";

/// The server's file name in the init assets (`/sbin.distro/distrofs`).
pub const GUEST_BINARY: &str = "distrofs";

/// The server's file name in this install's `libexec` dir. Apple
/// `container` VM kernels are always arm64 (amd64 distros are
/// Rosetta-translated userland), so one artifact serves every distro.
pub const SERVER_ARTIFACT: &str = "distrofs-server-aarch64";

/// Earlier installs shipped the server as plain `distrofs` — still
/// honored so an upgrade doesn't strand file access.
const LEGACY_SERVER_ARTIFACT: &str = "distrofs";

/// The session token's environment variable (never argv).
const TOKEN_ENV: &str = "DISTROFS_TOKEN";

/// The `DISTROFS` env var pointing at a specific client binary.
const CLIENT_ENV: &str = "DISTROFS";

/// The distrofs binary's verified Developer ID signature requirement:
/// its identifier, an Apple-issued chain. Unsigned dev/cargo builds
/// fall back to the ownership check in [`verify_client`].
const CLIENT_REQUIREMENT: &str =
    "identifier \"io.github.daphnediane.distrofs\" and anchor apple generic";

/// The designated requirement Apple's runtime helper satisfies
/// (`codesign -d -r-` on `container-runtime-linux`, condensed to the
/// identifier, an Apple-issued chain, and Apple's Containerization
/// team). Passed to `distrofs --peer-requirement` so the client checks
/// the connected relay peer after `connect(2)`.
const RUNTIME_REQUIREMENT: &str = "identifier \"com.apple.container.container-runtime-linux\" \
     and anchor apple generic and certificate leaf[subject.OU] = \"UPBK2H6LZM\"";

/// `sun_path` is 104 bytes including the NUL.
const SUN_PATH_MAX: usize = 103;

// ---------- socket path ----------

fn cvt(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

fn cstr(s: &[u8]) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)` — `/var/folders/…/T/`, outside
/// `$HOME` so no virtiofs share reaches it.
fn darwin_user_temp() -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n == 0 || n > buf.len() {
        return None;
    }
    let s = CStr::from_bytes_until_nul(&buf).ok()?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(s.to_bytes())))
}

/// Open `fd/name` as a directory without following a final symlink,
/// and require it to be ours with no group/other access.
fn open_private_dir(dirfd: RawFd, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated; a non-negative return is ours.
    let fd = cvt(unsafe {
        libc::openat(
            dirfd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let meta = File::from(fd.try_clone()?).metadata()?;
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid cannot fail.
    if meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a private directory of this user",
                name.to_string_lossy()
            ),
        ));
    }
    Ok(fd)
}

/// Create (if needed) and verify `base/<APP_NAME>/s`, returning its
/// path. `base` itself must be private to us.
fn private_socket_dir(base: &Path) -> Result<PathBuf> {
    let mut fd = open_private_dir(libc::AT_FDCWD, &cstr(base.as_os_str().as_bytes())?)?;
    for comp in [APP_NAME, "s"] {
        let c = cstr(comp.as_bytes())?;
        // SAFETY: `c` is NUL-terminated; fd is live. EEXIST is fine —
        // the open below verifies whatever is there.
        if unsafe { libc::mkdirat(fd.as_raw_fd(), c.as_ptr(), 0o700) } < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EEXIST) {
                return Err(e.into());
            }
        }
        fd = open_private_dir(fd.as_raw_fd(), &c)?;
    }
    Ok(base.join(APP_NAME).join("s"))
}

/// The host end of `runtime_id`'s relay:
/// `<darwin temp>/<APP_NAME>/s/<16 hex of SHA-256(runtime_id)>.sock`.
/// Creates and verifies the directory; fails if the path would exceed
/// `sun_path`. The distrofs client re-verifies the chain before
/// connecting — this computation exists so `--publish-socket` and the
/// client agree on one stable path per runtime id.
pub fn host_socket_path(runtime_id: &str) -> Result<PathBuf> {
    let base = darwin_user_temp().context("can't resolve the per-user temporary directory")?;
    let dir = private_socket_dir(&base)
        .with_context(|| format!("unsafe socket directory under {}", base.display()))?;
    let path = dir.join(format!(
        "{}.sock",
        &hex(&Sha256::digest(runtime_id.as_bytes()))[..16]
    ));
    if path.as_os_str().len() > SUN_PATH_MAX {
        bail!(
            "socket path {} exceeds the {SUN_PATH_MAX}-byte Unix socket limit",
            path.display()
        );
    }
    Ok(path)
}

/// The `--publish-socket` value for runtime container `runtime_id`.
pub fn publish_socket_arg(runtime_id: &str) -> Result<String> {
    let host = host_socket_path(runtime_id)?;
    let host = host.to_str().context("socket path is not UTF-8")?;
    // `container` splits the spec on `:`.
    if host.contains(':') {
        bail!("socket path {host} contains `:`");
    }
    Ok(format!("{host}:{GUEST_SOCKET}"))
}

// ---------- the guest server binary ----------

/// Where this install keeps the guest build, in priority order: under
/// `--dev`, the sibling distrofs worktree's
/// `target/aarch64-unknown-linux-musl` builds; then the running
/// binary's [`layout::libexec_dir_for`] — the `cargo install` +
/// `scripts/build-distrofs.sh --install` layout, the `.pkg`, and the
/// plugin copy (`<plugins>/distro/libexec/…`, filled by
/// `install-plugin`); then the distrofs-managed shared location
/// (`<prefix>/libexec/distrofs/`, where `distrofs fetch-servers` and
/// the distrofs `.pkg` put verified downloads). Earlier installs
/// shipped the server as plain `distrofs`; that name is still
/// accepted.
fn server_candidates(exe: Option<&Path>, dev: bool) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if dev {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../distrofs/target/aarch64-unknown-linux-musl");
        c.push(target.join("release").join(GUEST_BINARY));
        c.push(target.join("debug").join(GUEST_BINARY));
    }
    if let Some(d) = exe.and_then(layout::libexec_dir_for) {
        c.push(d.join(SERVER_ARTIFACT));
        c.push(d.join(LEGACY_SERVER_ARTIFACT));
    }
    if let Some(d) = exe
        .and_then(layout::prefix_of)
        .map(|p| p.join("libexec/distrofs"))
    {
        c.push(d.join(SERVER_ARTIFACT));
    }
    c
}

/// The byte marker every distrofs build carries — cheap assurance the
/// artifact we materialize into `sbin.distro` is actually a distrofs
/// binary (a stale or misplaced file would otherwise only surface as a
/// failed `container exec`).
const SERVER_STAMP: &[u8] = b"distrofs-version:";

/// The guest `distrofs` build this install ships, if any — a real file
/// carrying the version stamp.
#[must_use]
pub fn guest_binary() -> Option<PathBuf> {
    let exe = layout::current_exe().ok();
    server_candidates(exe.as_deref(), crate::dev_mode())
        .into_iter()
        .find(|p| p.is_file() && stamped(p))
}

/// Whether `path` is a file containing the distrofs version stamp.
fn stamped(p: &Path) -> bool {
    std::fs::read(p).is_ok_and(|b| b.windows(SERVER_STAMP.len()).any(|w| w == SERVER_STAMP))
}

// ---------- the host client binary ----------

/// The `DISTROFS` override, when it may be honored — same rule as
/// `CONTAINER_CLI`: the bare name (a PATH lookup) always applies; any
/// other value needs `--dev`, so a variable planted in a shell profile
/// can't swap the binary the session token is handed to. Ignored
/// values warn once per process.
fn client_override() -> Option<std::ffi::OsString> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let v = env_or_none(CLIENT_ENV)?;
    let p = Path::new(&v);
    if p == Path::new("distrofs") || crate::dev_mode() {
        return Some(v);
    }
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("ignoring DISTROFS={} (pass --dev to honor it)", p.display());
    }
    None
}

fn env_or_none(name: &str) -> Option<std::ffi::OsString> {
    let v = std::env::var_os(name)?;
    (!v.is_empty()).then_some(v)
}

/// Candidate locations for the host `distrofs` client, in order: the
/// `DISTROFS` override (see [`client_override`]), a sibling of the
/// running binary (`install-plugin` copies it beside `bin/distro`, and
/// `cargo install` puts it beside `cm`/`container-distro`), the
/// sibling worktree's build under `--dev`, the libexec dir, then PATH.
fn client_candidates(exe: Option<&Path>, dev: bool) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(v) = client_override() {
        let p = PathBuf::from(&v);
        if p.components().count() > 1 {
            c.push(p);
        } else if let Some(found) = find_on_path(&p) {
            c.push(found);
        }
    }
    if let Some(dir) = exe.and_then(Path::parent) {
        c.push(dir.join("distrofs"));
    }
    if dev {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../distrofs/target");
        c.push(target.join("debug").join("distrofs"));
        c.push(target.join("release").join("distrofs"));
    }
    if let Some(d) = exe.and_then(layout::libexec_dir_for) {
        c.push(d.join("distrofs"));
    }
    if let Some(found) = find_on_path(Path::new("distrofs")) {
        c.push(found);
    }
    c
}

/// Whether `path` is owned by us or root and not writable by an
/// untrusted account — the rule for an unsigned (cargo/dev) distrofs
/// build. Group-write is acceptable only from groups we can trust to
/// guard the install root: `wheel`/root, `admin`, or our own egid.
fn writable_only_by_trusted(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = path.symlink_metadata() else {
        return false;
    };
    // SAFETY: geteuid/getegid cannot fail.
    let (euid, egid) = unsafe { (libc::geteuid(), libc::getegid()) };
    (m.uid() == euid || m.uid() == 0)
        && m.mode() & 0o002 == 0
        && (m.mode() & 0o020 == 0 || [0, egid, 80].contains(&m.gid()))
}

/// Verify the `distrofs` binary at `path` before it is exec'd and
/// handed the session token. A Developer ID-signed copy must satisfy
/// [`CLIENT_REQUIREMENT`]; an unsigned or ad-hoc copy (cargo builds,
/// dev worktrees) must sit on a path writable only by us or root —
/// same check applied up the ancestor chain to `/`.
fn verify_client(path: &Path) -> Result<()> {
    let out = std::process::Command::new("codesign")
        .args(["-dvv", "--"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let team = out.ok().and_then(|o| {
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .find_map(|l| l.strip_prefix("TeamIdentifier=").map(str::to_string))
    });
    match team.as_deref() {
        // Signed: the Developer ID requirement must hold. `codesign -v`
        // checks the signature's validity and the requirement in one go.
        Some(t) if t != "not set" => {
            let st = std::process::Command::new("codesign")
                .args(["-v", &format!("-R={CLIENT_REQUIREMENT}"), "--"])
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if !st.is_ok_and(|s| s.success()) {
                bail!(
                    "{} is signed but does not satisfy `{CLIENT_REQUIREMENT}`",
                    path.display()
                );
            }
            return Ok(());
        }
        _ => {}
    }
    // Unsigned/ad-hoc: every component from the binary up must be ours
    // or root's, and not writable by anyone else.
    let mut p = Some(path);
    while let Some(c) = p {
        if !writable_only_by_trusted(c) {
            bail!(
                "{} is unsigned and {} is writable by untrusted users — \
                 install a signed distrofs build or fix permissions",
                path.display(),
                c.display()
            );
        }
        p = c.parent();
    }
    Ok(())
}

/// The located `distrofs` client binary, if any — not yet verified.
/// `install-plugin` uses this to choose what to copy; [`client_binary`]
/// verifies before exec.
#[must_use]
pub fn client_binary_path() -> Option<PathBuf> {
    let exe = layout::current_exe().ok();
    client_candidates(exe.as_deref(), crate::dev_mode())
        .into_iter()
        .find(|p| p.is_file())
}

/// Locate and verify the host `distrofs` binary.
fn client_binary() -> Result<PathBuf> {
    let bin = client_binary_path().context(
        "no distrofs client binary found — install distrofs (`cargo install --path \
         <distrofs checkout>`), run install-plugin from a tree that has one, or pass \
         --dev to use the sibling worktree's build",
    )?;
    verify_client(&bin)?;
    Ok(bin)
}

// ---------- the session ----------

/// A fresh 32-byte session token.
fn random32() -> io::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

/// `container exec -d` the server as guest root. The token rides the
/// environment (`--env NAME` inherits it from ours), never argv.
fn launch(t: &FilesTarget, token: &[u8; 32]) -> Result<()> {
    let bin = format!("{INIT_DIR}/{GUEST_BINARY}");
    let status = container_cmd()
        .args([
            "exec", "--detach", "--user", "0:0", "--env", TOKEN_ENV, &t.id, &bin, "serve",
        ])
        .args([
            "--socket",
            GUEST_SOCKET,
            "--uid",
            &t.uid.to_string(),
            "--gid",
            &t.gid.to_string(),
        ])
        .env(TOKEN_ENV, hex(token))
        .stdout(Stdio::null())
        .status()
        .context("failed to run `container exec`")?;
    if !status.success() {
        // The assets were just refreshed, so a missing binary means the
        // distro mounts legacy shared assets that can't be rewritten.
        bail!(
            "failed to start the file server in `{0}` (see the `container exec` error above) — \
             if {bin} is missing, `container distro set {0}` moves the distro onto its own init \
             assets",
            t.name
        );
    }
    Ok(())
}

/// Run `distrofs` subcommand `args` against `name` (or the default
/// distro): boot it if needed, launch a server instance with a fresh
/// token, then exec the verified client with the socket path, the
/// relay peer's code-signing requirement, and the mapped arguments.
/// Returns the client's exit status for the caller to propagate.
pub fn run(name: Option<String>, args: &[String]) -> Result<ExitStatus> {
    if guest_binary().is_none() {
        bail!(
            "no distrofs server build found for this install — run \
             `scripts/build-distrofs.sh --install <cargo root>` from a checkout (or pass \
             --dev to use the sibling worktree's build); see DEV.md \"Building distrofs\""
        );
    }
    let target = ops::files_target(name)?;
    let socket = host_socket_path(&target.id)?;
    if std::fs::symlink_metadata(&socket).is_err() {
        bail!(
            "`{0}` is running without the file-access relay (it started before files were enabled) — \
             `container distro stop {0}` and try again",
            target.name
        );
    }
    let client = client_binary()?;
    let token = random32()?;
    launch(&target, &token)?;
    let mut cmd = std::process::Command::new(client);
    cmd.arg("--socket")
        .arg(&socket)
        .arg("--peer-requirement")
        .arg(RUNTIME_REQUIREMENT);
    if crate::dev_mode() {
        cmd.arg("--dev");
    }
    cmd.args(args).env(TOKEN_ENV, hex(&token));
    cmd.status().context("failed to run the distrofs client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_candidates_cover_bin_plugin_and_dev_layouts() {
        let exe = Path::new("/opt/p/bin/container-distro");
        assert_eq!(
            server_candidates(Some(exe), false),
            vec![
                PathBuf::from("/opt/p/libexec/container-distro/distrofs-server-aarch64"),
                PathBuf::from("/opt/p/libexec/container-distro/distrofs"),
                PathBuf::from("/opt/p/libexec/distrofs/distrofs-server-aarch64"),
            ]
        );
        // The plugin copy finds the build `install-plugin` put beside
        // it, whatever prefix the `container` it plugs into lives at.
        assert_eq!(
            server_candidates(
                Some(Path::new(
                    "/dev/ctr/libexec/container-plugins/distro/bin/container-distro"
                )),
                false
            ),
            vec![
                PathBuf::from(
                    "/dev/ctr/libexec/container-plugins/distro/libexec/container-distro/distrofs-server-aarch64"
                ),
                PathBuf::from(
                    "/dev/ctr/libexec/container-plugins/distro/libexec/container-distro/distrofs"
                ),
                PathBuf::from(
                    "/dev/ctr/libexec/container-plugins/distro/libexec/distrofs/distrofs-server-aarch64"
                ),
            ]
        );
        // No install prefix, no candidate — in-tree builds need --dev,
        // and then come first.
        assert!(server_candidates(Some(Path::new("/src/cd/target/debug/cm")), false).is_empty());
        assert!(server_candidates(None, false).is_empty());
        let dev = server_candidates(Some(exe), true);
        assert!(dev[0].ends_with("target/aarch64-unknown-linux-musl/release/distrofs"));
        assert_eq!(
            dev.last().unwrap(),
            Path::new("/opt/p/libexec/distrofs/distrofs-server-aarch64")
        );
    }

    #[test]
    fn test_client_candidates_cover_sibling_libexec_and_dev_layouts() {
        let exe = Path::new("/opt/p/bin/container-distro");
        let c = client_candidates(Some(exe), false);
        assert_eq!(c[0], PathBuf::from("/opt/p/bin/distrofs"));
        assert_eq!(
            c[1],
            PathBuf::from("/opt/p/libexec/container-distro/distrofs")
        );
        // The plugin copy looks beside `bin/distro` first.
        let c = client_candidates(
            Some(Path::new(
                "/dev/ctr/libexec/container-plugins/distro/bin/distro",
            )),
            false,
        );
        assert_eq!(
            c[0],
            PathBuf::from("/dev/ctr/libexec/container-plugins/distro/bin/distrofs")
        );
    }

    #[test]
    fn test_host_socket_path_fits_sun_path() {
        let p = host_socket_path("some-distro-with-a-long-name-0123456789").unwrap();
        assert!(p.as_os_str().len() <= SUN_PATH_MAX);
        assert!(p.starts_with(private_socket_dir(&darwin_user_temp().unwrap()).unwrap()));
    }

    #[test]
    fn test_private_socket_dir_creates_and_accepts_its_own_chain() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let p = private_socket_dir(d.path()).unwrap();
        assert!(p.ends_with(format!("{APP_NAME}/s")));
        private_socket_dir(d.path()).unwrap();
    }

    #[test]
    fn test_private_socket_dir_rejects_symlinks_and_open_modes() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", d.path().join("link")).unwrap();
        assert!(
            open_private_dir(
                libc::AT_FDCWD,
                &cstr(d.path().join("link").as_os_str().as_bytes()).unwrap()
            )
            .is_err()
        );
        let open = tempfile::tempdir().unwrap();
        std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(private_socket_dir(open.path()).is_err());
    }

    #[test]
    fn test_publish_socket_arg_shape() {
        let a = publish_socket_arg("d1-abc123").unwrap();
        let (host, guest) = a.split_once(':').unwrap();
        assert!(host.ends_with(".sock") && host.starts_with('/'));
        assert_eq!(guest, GUEST_SOCKET);
    }
}
