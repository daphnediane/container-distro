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

/// `<exe>/../share/man/man1` for a binary that lives in a `bin`
/// directory — both macOS `man` and Linux man-db add
/// `<dir>/../share/man` to the search path for every `bin` directory on
/// `PATH`, so pages for `$CARGO_HOME/bin/cm` (or `cargo install --root
/// <P>`) are found with no manpath configuration.
fn man_dir_for(exe: &Path) -> Result<PathBuf> {
    let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    exe.parent()
        .filter(|p| p.file_name() == Some(OsStr::new("bin")))
        .and_then(Path::parent)
        .map(|prefix| prefix.join("share/man/man1"))
        .with_context(|| {
            format!(
                "{} isn't under a `bin` directory — pass an explicit dir",
                exe.display()
            )
        })
}

/// The directory `*.1` man pages are written to when none is given: the
/// `share/man/man1` of the prefix the running binary is installed under.
/// Dev builds (e.g. `target/debug/cm`) don't have one and must pass a
/// dir explicitly.
pub fn default_man_dir() -> Result<PathBuf> {
    man_dir_for(&env::current_exe().context("cannot locate the running binary")?)
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

/// Whether `existing` is one of our generated pages — not a hand-written
/// or foreign page that happens to share the file name. Checks the `.TH`
/// title plus the rendered NAME line (the about text, which is stable
/// across versions), so a page installed by an older release still
/// matches but anything else is left alone.
fn is_our_page(existing: &str, rendered: &str, page_name: &str) -> bool {
    let stem = page_name.strip_suffix(".1").unwrap_or(page_name);
    let escaped = stem.replace('-', "\\-");
    let name_line = rendered
        .lines()
        .find(|l| l.starts_with(&format!("{escaped} \\-")));
    existing.contains(&format!(".TH {stem} ")) && name_line.is_some_and(|l| existing.contains(l))
}

/// Remove previously written `pages` from `dir` — only when the file on
/// disk still looks like one of our generated pages; foreign or edited
/// files are left in place with a warning. Missing files and a missing
/// directory are not errors.
pub fn remove_pages(dir: &Path, pages: &[(String, String)]) -> Result<()> {
    for (name, rendered) in pages {
        let path = dir.join(name);
        match fs::read_to_string(&path) {
            Ok(existing) if !is_our_page(&existing, rendered, name) => {
                eprintln!(
                    "not removing {}: not one of our generated pages",
                    path.display()
                );
            }
            Ok(_) => {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
                println!("Removed {}", path.display());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn man_dir_follows_the_bin_prefix() {
        assert_eq!(
            man_dir_for(Path::new("/usr/local/bin/cm")).unwrap(),
            Path::new("/usr/local/share/man/man1")
        );
        assert_eq!(
            man_dir_for(Path::new("/x/cargo/bin/cm")).unwrap(),
            Path::new("/x/cargo/share/man/man1")
        );
        // A dev build in target/debug has no prefix — the caller must
        // pass a dir.
        assert!(man_dir_for(Path::new("/w/target/debug/cm")).is_err());
        assert!(man_dir_for(Path::new("/w/target/debug/deps/cm-abc")).is_err());
    }

    #[test]
    fn write_and_remove_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("man1");
        let pages = vec![(
            "cm.1".to_string(),
            ".TH cm 1\n.SH NAME\ncm \\- WSL\\-compatible wrapper".to_string(),
        )];
        let written = write_pages(&dir, &pages).unwrap();
        assert_eq!(written, [dir.join("cm.1")]);
        remove_pages(&dir, &pages).unwrap();
        assert!(!written[0].exists());
        // Removing again is a no-op, not an error.
        remove_pages(&dir, &pages).unwrap();
    }

    #[test]
    fn remove_pages_leaves_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // Same file name, different contents — not ours.
        let foreign = dir.join("cm.1");
        fs::write(&foreign, ".TH cm 1\n.SH NAME\ncm \\- some other tool").unwrap();
        let pages = vec![(
            "cm.1".to_string(),
            ".TH cm 1\n.SH NAME\ncm \\- WSL\\-compatible wrapper".to_string(),
        )];
        remove_pages(&dir, &pages).unwrap();
        assert!(foreign.exists());
    }
}
