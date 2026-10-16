/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Where an install keeps its files, derived from the running binary.
//!
//! A binary in a `bin` directory has an install *prefix* — its parent:
//! `$CARGO_HOME` after `cargo install`, `/usr/local` for the `.pkg`, or
//! `<plugins>/distro` for the `container distro` plugin copy (whatever
//! prefix that `container` has). Packaged files live under it the
//! conventional way — `share/` for man pages and completions,
//! `libexec/<APP_NAME>/` for helpers like the guest `distrofs` build.
//! A binary anywhere else (`target/debug/cm`) has no prefix, so callers
//! need an explicit path or `--dev`.
//!
//! Paths are always relative to the canonicalized binary — an alias
//! symlink (`wsl` → `cm`) resolves to the real install — never to an
//! environment variable or a baked-in path, so nothing outside the
//! binary's own tree can steer them.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::naming::APP_NAME;

/// The running binary, canonicalized.
pub fn current_exe() -> Result<PathBuf> {
    let exe = env::current_exe().context("cannot locate the running binary")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn canonical(exe: &Path) -> PathBuf {
    exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf())
}

/// The install prefix of `exe`: the parent of the `bin` directory it
/// lives in, or `None` outside one.
#[must_use]
pub fn prefix_of(exe: &Path) -> Option<PathBuf> {
    let exe = canonical(exe);
    let dir = exe.parent()?;
    (dir.file_name() == Some(OsStr::new("bin")))
        .then(|| dir.parent().map(Path::to_path_buf))
        .flatten()
}

/// `<prefix>/share`.
#[must_use]
pub fn share_dir(prefix: &Path) -> PathBuf {
    prefix.join("share")
}

/// `<prefix>/libexec/<APP_NAME>` — packaged helpers.
#[must_use]
pub fn libexec_dir(prefix: &Path) -> PathBuf {
    prefix.join("libexec").join(APP_NAME)
}

/// The helper directory that belongs to `exe`: [`libexec_dir`] of its
/// prefix, or — for a binary installed in that directory itself — its
/// own directory.
#[must_use]
pub fn libexec_dir_for(exe: &Path) -> Option<PathBuf> {
    let exe = canonical(exe);
    let dir = exe.parent()?;
    if dir.file_name() == Some(OsStr::new(APP_NAME)) {
        return Some(dir.to_path_buf());
    }
    prefix_of(&exe).map(|p| libexec_dir(&p))
}

/// [`prefix_of`] the running binary, with an error naming it — for
/// defaults the user can override with an explicit path.
pub fn current_prefix() -> Result<PathBuf> {
    let exe = current_exe()?;
    prefix_of(&exe).with_context(|| {
        format!(
            "{} isn't under a `bin` directory — pass an explicit dir",
            exe.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_of_bin_layouts() {
        for (exe, prefix) in [
            ("/usr/local/bin/cm", "/usr/local"),
            ("/x/cargo/bin/cm", "/x/cargo"),
            (
                "/ctr/libexec/container-plugins/distro/bin/container-distro",
                "/ctr/libexec/container-plugins/distro",
            ),
        ] {
            assert_eq!(
                prefix_of(Path::new(exe)).as_deref(),
                Some(Path::new(prefix)),
                "{exe}"
            );
        }
        // Dev builds have no prefix.
        assert_eq!(prefix_of(Path::new("/w/target/debug/cm")), None);
        assert_eq!(prefix_of(Path::new("/w/target/debug/deps/cm-abc")), None);
        assert_eq!(prefix_of(Path::new("/cm")), None);
    }

    #[test]
    fn test_share_and_libexec_dirs() {
        let p = Path::new("/usr/local");
        assert_eq!(share_dir(p), Path::new("/usr/local/share"));
        assert_eq!(
            libexec_dir(p),
            Path::new("/usr/local/libexec/container-distro")
        );
    }

    #[test]
    fn test_libexec_dir_for() {
        assert_eq!(
            libexec_dir_for(Path::new("/opt/p/bin/cm")).as_deref(),
            Some(Path::new("/opt/p/libexec/container-distro"))
        );
        // Installed into the libexec dir itself: its own directory.
        assert_eq!(
            libexec_dir_for(Path::new("/opt/p/libexec/container-distro/helper")).as_deref(),
            Some(Path::new("/opt/p/libexec/container-distro"))
        );
        assert_eq!(libexec_dir_for(Path::new("/w/target/debug/cm")), None);
    }

    #[test]
    fn test_symlinked_binary_resolves_to_its_real_prefix() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real/bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("cm"), b"").unwrap();
        let alias = d.path().join("alias");
        std::fs::create_dir_all(&alias).unwrap();
        std::os::unix::fs::symlink(real.join("cm"), alias.join("wsl")).unwrap();
        let want = d.path().canonicalize().unwrap().join("real");
        assert_eq!(prefix_of(&alias.join("wsl")), Some(want));
    }
}
