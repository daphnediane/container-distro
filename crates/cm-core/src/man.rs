/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Installing generated man pages.
//!
//! `cargo install` only copies binaries, so man pages are generated
//! from the clap definitions at runtime (`clap_mangen` — they can never
//! drift from the CLI) and written by the installed binary itself:
//! `cm --install-man` / `container-distro install-man`, or as part of
//! `container-distro install-plugin`.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The directory `*.1` man pages are written to when none is given.
///
/// If the running executable lives in a `bin` directory, the pages go in
/// the sibling `share/man/man1`: both macOS `man` and Linux man-db add
/// `<dir>/../share/man` to the search path for every `bin` directory on
/// `PATH`, so pages for `$CARGO_HOME/bin/cm` (or `cargo install --root
/// <P>`) are found with no manpath configuration.
///
/// When the binary isn't in a `bin` directory (a dev build in
/// `target/`), falls back to `$CARGO_HOME/share/man/man1`, then
/// `~/.cargo/share/man/man1` — matching where `cargo install` puts
/// binaries.
pub fn default_man_dir() -> Result<PathBuf> {
    if let Ok(exe) = env::current_exe() {
        // Resolve symlinks so a `wsl` -> `cm` alias still lands in the
        // real binary's prefix.
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(prefix) = exe
            .parent()
            .filter(|p| p.file_name() == Some(OsStr::new("bin")))
            .and_then(|p| p.parent())
        {
            return Ok(prefix.join("share/man/man1"));
        }
    }
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
        .context("cannot derive a man directory; pass one explicitly")?;
    Ok(cargo_home.join("share/man/man1"))
}

/// Write `pages` (`cm.1`-style file name → roff source) into `dir`,
/// creating it. Returns the written paths.
pub fn write_pages(dir: &Path, pages: &[(String, String)]) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let mut written = Vec::with_capacity(pages.len());
    for (name, roff) in pages {
        let path = dir.join(name);
        fs::write(&path, roff).with_context(|| {
            format!(
                "cannot write {} (re-run with sudo, or pass an explicit dir)",
                path.display()
            )
        })?;
        written.push(path);
    }
    Ok(written)
}

/// Remove previously written `page` file names from `dir`. Missing files
/// and a missing directory are not errors.
pub fn remove_pages(dir: &Path, names: &[String]) -> Result<()> {
    for name in names {
        let path = dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => println!("Removed {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to remove {}", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_man_dir_is_a_man1() {
        // Either the exe's `<bin>/../share/man/man1` or the cargo-home
        // fallback — both end the same way.
        assert_eq!(
            default_man_dir().unwrap().file_name(),
            Some(OsStr::new("man1"))
        );
    }

    #[test]
    fn write_and_remove_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("man1");
        let pages = vec![("cm.1".to_string(), ".TH CM 1".to_string())];
        let written = write_pages(&dir, &pages).unwrap();
        assert_eq!(written, [dir.join("cm.1")]);
        assert_eq!(fs::read_to_string(&written[0]).unwrap(), ".TH CM 1");
        let names: Vec<String> = pages.iter().map(|(n, _)| n.clone()).collect();
        remove_pages(&dir, &names).unwrap();
        assert!(!written[0].exists());
        // Removing again is a no-op, not an error.
        remove_pages(&dir, &names).unwrap();
    }
}
