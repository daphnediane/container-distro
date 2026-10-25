/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The command-line front ends: the `container distro` plugin's
//! subcommands and `cm`'s WSL-style interface, which
//! `container-distro cli …` runs — `cm` itself is an alias script for
//! that. The binary is a thin `main` over [`main`].

pub mod commands;
mod docs;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};

use anyhow::{Context, Result};

use crate::container;
use crate::setup::layout::{self, INVOKED_AS_FLAG};

/// Entry point of the `container-distro` binary. Generated `cm`-style
/// aliases run it as `container-distro --invoked-as "$0" cli …`.
pub fn main() -> ExitCode {
    let mut argv: Vec<OsString> = std::env::args_os().collect();
    if let Some(path) = take_invoked_as(&mut argv) {
        layout::set_invoked_as(path);
    }
    commands::main_from(argv)
}

/// Strip the hidden leading `--invoked-as PATH` generated aliases pass
/// — honored only in that position, so it can never collide with a
/// front end's own arguments or a guest command line.
fn take_invoked_as(argv: &mut Vec<OsString>) -> Option<PathBuf> {
    if !(argv.get(1).is_some_and(|a| a == INVOKED_AS_FLAG) && argv.len() > 2) {
        return None;
    }
    let path = PathBuf::from(argv.remove(2));
    argv.remove(1);
    // `$0` may be relative to the caller's cwd; keep it unresolved
    // otherwise — its directory is the point.
    Some(if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |d| d.join(&path))
    })
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
    container::upstream::note_behind_pin();
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
    fn invoked_as_is_stripped() {
        let mut a = argv(&["x", "--invoked-as", "/b/wsl", "cli", "-l"]);
        assert_eq!(
            take_invoked_as(&mut a).as_deref(),
            Some(std::path::Path::new("/b/wsl"))
        );
        assert_eq!(a, argv(&["x", "cli", "-l"]));
    }

    #[test]
    fn invoked_as_only_counts_in_leading_position() {
        // A guest command line that happens to contain the flag is
        // passed through untouched.
        let mut a = argv(&["x", "cli", "--", "echo", "--invoked-as", "y"]);
        assert_eq!(take_invoked_as(&mut a), None);
        assert_eq!(a.len(), 6);
    }

    #[test]
    fn relative_invoked_as_is_made_absolute() {
        let mut a = argv(&["x", "--invoked-as", "bin/wsl"]);
        assert_eq!(
            take_invoked_as(&mut a).unwrap(),
            std::env::current_dir().unwrap().join("bin/wsl")
        );
    }
}
