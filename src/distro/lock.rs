/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `uchg` locking and symlink-refusing file writes for host-trusted state.

use anyhow::{Context, Result, bail};
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::process::{Command, Stdio};

/// Set or clear the macOS user-immutable flag on `path`.
///
/// virtiofs exposes no flag operations, so `uchg` is a real boundary
/// against a guest rewriting these files through a rw-shared home:
/// writes, unlinks, and renames all fail with EPERM until a host
/// process clears the flag (verified against a running distro).
pub(super) fn set_locked(path: &Path, locked: bool) -> Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    // `lchflags`, not `chflags`: a symlink planted at `path` gets its
    // own flags changed, never its target's — following it would let
    // a guest-planted link unlock any file the host user can chflag.
    // SAFETY: `c` is a valid NUL-terminated path; lchflags only reads it.
    let rc = unsafe { libc::lchflags(c.as_ptr(), if locked { libc::UF_IMMUTABLE } else { 0 }) };
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

/// `unlock`'s counterpart — best-effort `uchg` on a host-owned file a
/// rw-shared guest could otherwise rewrite. Used outside the journal
/// for the terminal-launcher artifacts (`terminal-cmds/*.sh`,
/// `terminal-profiles/*.terminal`, the iTerm2 dynamic-profile file):
/// a guest rewriting them would own the next profile launch.
pub(crate) fn lock(path: &Path) {
    if fs::symlink_metadata(path).is_ok() {
        let _ = set_locked(path, true);
    }
}

/// `lock` for a *symlink itself*: `chflags(2)` follows links (it maps
/// to lchmod, which refuses them), so flagging `~/.container-distro`
/// would lock its target — the state dir. Shells out to `chflags -h`,
/// which takes the link inode.
pub(crate) fn lock_symlink(path: &Path) {
    let _ = Command::new("/usr/bin/chflags")
        .args(["-h", "uchg"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Whether `path` carries the user-immutable flag — checked with
/// `lstat` (the flag can't ride a symlink). A guest can't set it through
/// a virtiofs share, so a locked file is host-written: the check doubles
/// as journal provenance.
pub(crate) fn is_locked(path: &Path) -> bool {
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
/// `mode` is applied through the open descriptor, never the path.
pub(crate) fn write_nofollow(path: &Path, body: &[u8], mode: u32) -> Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    for _ in 0..2 {
        // SAFETY: `c` is a valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW,
                mode as libc::c_uint,
            )
        };
        if fd >= 0 {
            // SAFETY: fd is a live descriptor open() just returned.
            let mut f = unsafe { fs::File::from_raw_fd(fd) };
            f.write_all(body)
                .with_context(|| format!("failed to write {}", path.display()))?;
            return f
                .set_permissions(fs::Permissions::from_mode(mode))
                .with_context(|| format!("failed to chmod {}", path.display()));
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

/// `chmod` that refuses a symlink at `path` (`O_NOFOLLOW` open, then
/// `fchmod`) instead of changing the mode of whatever it points at.
pub(super) fn chmod_nofollow(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .and_then(|f| f.set_permissions(fs::Permissions::from_mode(mode)))
        .with_context(|| format!("failed to chmod {}", path.display()))
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
pub(super) fn open_locked_rw(path: &Path) -> Result<fs::File> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A symlink planted where the journal spec belongs is removed
    /// rather than followed — the file can't have been locked, so it
    /// was never ours.
    #[test]
    fn open_locked_rw_rejects_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        // The link targets a locked file of ours — standing in for
        // another distro's journal asset. Never a system file: a lock
        // call that follows the link there can raise an admin prompt.
        let victim = dir.path().join("victim");
        fs::write(&victim, b"keep").unwrap();
        set_locked(&victim, true).unwrap();
        let spec_p = dir.path().join("spec.json");
        std::os::unix::fs::symlink(&victim, &spec_p).unwrap();
        let refused = open_locked_rw(&spec_p).is_err();
        let still_locked = is_locked(&victim);
        // Unlock before asserting so the tempdir can always clean up.
        set_locked(&victim, false).unwrap();
        assert!(refused);
        assert!(fs::symlink_metadata(&spec_p).is_err(), "link not removed");
        assert!(still_locked, "unlock followed the planted link");
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
    }
}
