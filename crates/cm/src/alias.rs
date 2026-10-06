/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `wsl`-style alias symlinks for `cm` (`--install-alias` /
//! `--uninstall-alias`), plus a `<name>.1` man-page symlink so
//! `man wsl` works too.
//!
//! Removal is strictly verified: a file is only deleted when it is a
//! symlink resolving to this `cm` binary (or to `cm.1` for the man
//! page). Anything else is refused or left alone.

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cm_core::man;

/// The canonicalized `cm` executable we're aliasing.
fn our_exe() -> Result<PathBuf> {
    env::current_exe()?
        .canonicalize()
        .context("cannot resolve the cm binary")
}

/// Resolve `NAME` or `PATH` into the absolute path of the alias symlink:
/// a bare name goes next to the running `cm` binary; a path is used as
/// given (relative to the cwd).
fn alias_path(target: &str) -> Result<PathBuf> {
    if target.is_empty() {
        bail!("empty alias");
    }
    let path = PathBuf::from(target);
    if path.is_absolute() {
        return Ok(path);
    }
    if target.contains('/') {
        return Ok(env::current_dir()?.join(path));
    }
    let dir = our_exe()?
        .parent()
        .context("the cm binary has no parent directory")?
        .to_path_buf();
    Ok(dir.join(target))
}

fn alias_name(link: &Path) -> Result<String> {
    link.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .with_context(|| format!("{} is not a usable file name", link.display()))
}

/// Where the symlink's target actually points (relative links resolved
/// against the link's directory), whether or not it still exists.
fn link_target(link: &Path) -> Result<PathBuf> {
    let t = fs::read_link(link).with_context(|| format!("failed to read {}", link.display()))?;
    if t.is_absolute() {
        Ok(t)
    } else {
        Ok(link
            .parent()
            .map(|p| p.join(&t))
            .unwrap_or_else(|| t.clone()))
    }
}

fn points_to(link: &Path, target: &Path) -> bool {
    link_target(link)
        .map(|t| t.canonicalize().unwrap_or(t) == *target)
        .unwrap_or(false)
}

/// `alias.1` -> `cm.1` in `cm`'s man dir (which is already on the
/// manpath, wherever the alias itself lives). Only links when `cm.1`
/// is installed — installing the alias doesn't install the man page,
/// and a later `--install-man` + `--install-alias` adds just the link.
fn install_man_link(name: &str) -> Result<()> {
    let dir = match man::default_man_dir() {
        Ok(d) => d,
        // e.g. a dev build with no install prefix — the alias still
        // stands; just skip the man link.
        Err(e) => {
            eprintln!("note: skipping {name}.1 ({e:#})");
            return Ok(());
        }
    };
    let cm_page = dir.join("cm.1");
    if !cm_page.is_file() {
        eprintln!(
            "note: cm.1 isn't installed in {}; skipping {name}.1 \
             (run `cm --install-man` first)",
            dir.display()
        );
        return Ok(());
    }
    let link = dir.join(format!("{name}.1"));
    match link.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            if points_to(&link, &cm_page) {
                println!("{} already points to cm.1", link.display());
            } else {
                eprintln!(
                    "not touching {}: a symlink, but not to cm.1",
                    link.display()
                );
            }
        }
        Ok(_) => eprintln!("not touching {}: not a symlink", link.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // Relative link: the page must resolve in this directory.
            symlink("cm.1", &link)
                .with_context(|| format!("failed to create {}", link.display()))?;
            println!("Installed {} -> cm.1", link.display());
        }
        Err(e) => return Err(e).with_context(|| format!("failed to stat {}", link.display())),
    }
    Ok(())
}

/// `cm --install-alias NAME|PATH` — symlink `NAME`/`PATH` to this binary
/// plus a `<name>(1)` man-page link.
pub fn install(target: &str) -> Result<()> {
    let link = alias_path(target)?;
    let exe = our_exe()?;
    if link == exe {
        bail!("`{target}` resolves to the cm binary itself");
    }
    if let Some(dir) = link.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    match link.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            if points_to(&link, &exe) {
                println!("{} already points to cm", link.display());
            } else {
                bail!(
                    "{} exists and doesn't point to cm — refusing to replace it",
                    link.display()
                );
            }
        }
        Ok(_) => bail!("{} exists and isn't a symlink — refusing", link.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            symlink(&exe, &link).with_context(|| format!("failed to create {}", link.display()))?;
            println!("Installed {} -> {}", link.display(), exe.display());
        }
        Err(e) => return Err(e).with_context(|| format!("failed to stat {}", link.display())),
    }
    install_man_link(&alias_name(&link)?)
}

/// `cm --uninstall-alias NAME|PATH` — remove the symlink and its man
/// page, only if both provably point at this `cm`.
pub fn uninstall(target: &str) -> Result<()> {
    let link = alias_path(target)?;
    let exe = our_exe()?;
    match link.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            if !points_to(&link, &exe) {
                bail!(
                    "{} is a symlink but not to this cm — refusing",
                    link.display()
                );
            }
            fs::remove_file(&link)
                .with_context(|| format!("failed to remove {}", link.display()))?;
            println!("Removed {}", link.display());
        }
        Ok(_) => bail!("{} exists but isn't a symlink — refusing", link.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            println!("Not installed ({})", link.display());
        }
        Err(e) => return Err(e).with_context(|| format!("failed to stat {}", link.display())),
    }
    // The man-page link: only remove when it's a symlink to cm.1.
    let Ok(man_dir) = man::default_man_dir() else {
        return Ok(());
    };
    let man_link = man_dir.join(format!("{}.1", alias_name(&link)?));
    match man_link.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            if points_to(&man_link, &man_dir.join("cm.1")) {
                fs::remove_file(&man_link)
                    .with_context(|| format!("failed to remove {}", man_link.display()))?;
                println!("Removed {}", man_link.display());
            } else {
                eprintln!(
                    "not removing {}: a symlink, but not to cm.1",
                    man_link.display()
                );
            }
        }
        Ok(_) => eprintln!("not removing {}: not a symlink", man_link.display()),
        Err(_) => {} // missing man page or dir — nothing to do
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_resolves_against_cwd() {
        let p = alias_path("sub/wsl").unwrap();
        assert_eq!(p, env::current_dir().unwrap().join("sub/wsl"));
    }

    #[test]
    fn bare_name_lands_next_to_exe() {
        let p = alias_path("wsl").unwrap();
        let expected = our_exe().unwrap().parent().unwrap().join("wsl");
        assert_eq!(p, expected);
    }

    #[test]
    fn foreign_symlink_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("wsl");
        symlink("/bin/sh", &link).unwrap();
        assert!(!points_to(&link, &our_exe().unwrap()));
    }
}
