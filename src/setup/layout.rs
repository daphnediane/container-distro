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
//! Paths are always relative to the canonicalized binary — a symlink
//! to it resolves to the real install — never to an environment
//! variable or a baked-in path, so nothing outside the binary's own
//! tree can steer them.
//!
//! The same rule finds *our own commands*: there is no searching for a
//! sibling `cm` or `container-distro`, or for either on `PATH`. Every
//! binary carries both front ends, so [`self_invocation`] re-runs this
//! binary with the leading arguments that select the wanted one, and
//! that is what generated launchers and aliases bake. The one
//! exception to "resolve everything" is [`invoked_as`]: an alias
//! script passes its own unresolved path so aliases created through it
//! land in the same directory.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

/// Which command-line interface a process runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontEnd {
    /// `cm`'s WSL-style flags.
    Cm,
    /// `container distro`'s subcommands.
    Distro,
}

/// Hidden leading argument: the alias path a generated alias script
/// was run as (`"$0"`), so bare-name aliases it creates land beside it.
pub const INVOKED_AS_FLAG: &str = "--invoked-as";

/// Hidden leading argument selecting the `container distro` front end
/// in a binary whose own front end is `cm` — what launchers baked by
/// the `cm` binary use. `container-distro` reaches `cm` through its
/// public `cli` subcommand instead.
pub const DISTRO_FRONT_END_FLAG: &str = "--front-end=distro";

#[derive(Debug)]
struct Invocation {
    native: FrontEnd,
    invoked_as: Option<PathBuf>,
}

static INVOCATION: OnceLock<Invocation> = OnceLock::new();

/// Record how this process was started; the binaries' `main` calls this
/// once, before any front end runs.
pub fn set_invocation(native: FrontEnd, invoked_as: Option<PathBuf>) {
    let _ = INVOCATION.set(Invocation { native, invoked_as });
}

/// The front end this binary runs without a selector (`Distro` when
/// [`set_invocation`] never ran, e.g. in tests).
fn native() -> FrontEnd {
    INVOCATION.get().map_or(FrontEnd::Distro, |i| i.native)
}

/// The alias path this process was started through, when a generated
/// alias passed `--invoked-as` — unresolved, so its directory is where
/// the user keeps aliases.
#[must_use]
pub fn invoked_as() -> Option<&'static Path> {
    INVOCATION.get().and_then(|i| i.invoked_as.as_deref())
}

/// The leading arguments that reach front end `want` in a binary whose
/// own front end is `native`.
#[must_use]
pub fn front_end_args(native: FrontEnd, want: FrontEnd) -> &'static [&'static str] {
    match (native, want) {
        (FrontEnd::Distro, FrontEnd::Cm) => &["cli"],
        (FrontEnd::Cm, FrontEnd::Distro) => &[DISTRO_FRONT_END_FLAG],
        _ => &[],
    }
}

/// How to run this binary again with front end `want`: the
/// canonicalized running binary plus its leading arguments. Generated
/// launchers and aliases bake this — never a sibling or `PATH` lookup,
/// which could resolve to some other binary.
pub fn self_invocation(want: FrontEnd) -> Result<(PathBuf, &'static [&'static str])> {
    Ok((current_exe()?, front_end_args(native(), want)))
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
    fn test_front_end_args_reach_the_other_front_end() {
        use FrontEnd::{Cm, Distro};
        assert!(front_end_args(Cm, Cm).is_empty());
        assert!(front_end_args(Distro, Distro).is_empty());
        assert_eq!(front_end_args(Distro, Cm), ["cli"]);
        assert_eq!(front_end_args(Cm, Distro), [DISTRO_FRONT_END_FLAG]);
    }

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
