/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The host account and resources a distro is provisioned from.

use crate::distro::spec::HostUser;
use anyhow::{Context, Result, bail};
use std::env;

pub(super) fn host_home() -> Result<String> {
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

pub(super) fn host_user() -> Result<HostUser> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_user_name_charset() {
        for ok in ["daphne", "a.b-c_d", "x", "User1", "0root"] {
            assert!(safe_user_name(ok), "{ok}");
        }
        for bad in ["", "-evil", ".hidden", "a b", "x:y", "a/b", "u$", "u!"] {
            assert!(!safe_user_name(bad), "{bad}");
        }
    }
}
