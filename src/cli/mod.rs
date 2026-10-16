/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The command-line front ends: `cm`'s WSL-style interface and the
//! `container distro` plugin's subcommands. Both binaries are thin
//! `main`s over [`main`], which also reaches the other front end —
//! `container-distro cli …` runs `cm`.

pub mod cm;
pub mod distro;
mod docs;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};

use anyhow::{Context, Result};

use crate::container;
use crate::setup::layout::{self, DISTRO_FRONT_END_FLAG, FrontEnd, INVOKED_AS_FLAG};

/// Entry point for both binaries; `native` is the binary's own front
/// end.
pub fn main(native: FrontEnd) -> ExitCode {
    let mut argv: Vec<OsString> = std::env::args_os().collect();
    let (invoked_as, selected) = take_prelude(&mut argv);
    layout::set_invocation(native, invoked_as);
    match selected.unwrap_or(native) {
        FrontEnd::Cm => cm::main_from(argv),
        FrontEnd::Distro => distro::main_from(argv),
    }
}

/// Strip the hidden leading arguments generated aliases and launchers
/// pass — `--invoked-as PATH`, then `--front-end=distro` — honored only
/// in that position, so they can never collide with a front end's own
/// arguments or a guest command line.
fn take_prelude(argv: &mut Vec<OsString>) -> (Option<PathBuf>, Option<FrontEnd>) {
    let mut invoked_as = None;
    if argv.get(1).is_some_and(|a| a == INVOKED_AS_FLAG) && argv.len() > 2 {
        let path = PathBuf::from(argv.remove(2));
        argv.remove(1);
        // `$0` may be relative to the caller's cwd; keep it unresolved
        // otherwise — its directory is the point.
        invoked_as = Some(if path.is_absolute() {
            path
        } else {
            std::env::current_dir().map_or(path.clone(), |d| d.join(&path))
        });
    }
    let mut selected = None;
    if argv.get(1).is_some_and(|a| a == DISTRO_FRONT_END_FLAG) {
        argv.remove(1);
        selected = Some(FrontEnd::Distro);
    }
    (invoked_as, selected)
}

/// `NAME VERSION`, then `container --version` — shared by
/// `cm --version` and `container-distro version`.
fn print_version(name: &str) -> Result<ExitCode> {
    println!("{name} {}", env!("CARGO_PKG_VERSION"));
    if container::resolve_binary().is_none() {
        println!("container: not installed");
        return Ok(ExitCode::SUCCESS);
    }
    let status = container::container_cmd()
        .arg("--version")
        .status()
        .context("failed to run `container --version`")?;
    Ok(exit_code(status))
}

/// A child's exit status as ours.
fn exit_code(status: ExitStatus) -> ExitCode {
    ExitCode::from(status.code().map_or(1, |c| c.clamp(0, 255) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn prelude_strips_invoked_as_and_selector() {
        let mut a = argv(&["x", "--invoked-as", "/b/wsl", "--front-end=distro", "run"]);
        let (inv, sel) = take_prelude(&mut a);
        assert_eq!(inv.as_deref(), Some(std::path::Path::new("/b/wsl")));
        assert_eq!(sel, Some(FrontEnd::Distro));
        assert_eq!(a, argv(&["x", "run"]));
    }

    #[test]
    fn prelude_only_counts_in_leading_position() {
        // A guest command line that happens to contain the flags is
        // passed through untouched.
        let mut a = argv(&["cm", "--", "echo", "--invoked-as", "x"]);
        assert_eq!(take_prelude(&mut a), (None, None));
        assert_eq!(a.len(), 5);
        let mut a = argv(&["cm", "-e", "--front-end=distro"]);
        assert_eq!(take_prelude(&mut a), (None, None));
    }

    #[test]
    fn relative_invoked_as_is_made_absolute() {
        let mut a = argv(&["x", "--invoked-as", "bin/wsl"]);
        let (inv, _) = take_prelude(&mut a);
        assert_eq!(
            inv.unwrap(),
            std::env::current_dir().unwrap().join("bin/wsl")
        );
    }
}
