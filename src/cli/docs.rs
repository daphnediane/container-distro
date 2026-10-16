/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Man pages and shell completions generated from a front end's clap
//! definition, so they can't drift from `--help`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap_complete::shells::{Bash, Fish, Zsh};

use crate::completions::{self, Shell};
use crate::man;

/// A front end's clap command under the name it's installed as —
/// `container-distro`, not `container distro`, so pages are
/// `container-distro*.1` and completions bind to the real binary.
pub(crate) struct Docs {
    cmd: clap::Command,
    name: &'static str,
}

impl Docs {
    pub(crate) fn new(cmd: clap::Command, name: &'static str) -> Self {
        Self {
            cmd: cmd.name(name),
            name,
        }
    }

    /// `<name>(1)` plus one `<name>-<sub>(1)` per (nested) subcommand.
    pub(crate) fn man_pages(&self) -> Result<Vec<(String, String)>> {
        let mut pages = Vec::new();
        render_man(&self.cmd, self.name, &mut pages)?;
        Ok(pages)
    }

    pub(crate) fn completions(&self) -> Vec<(Shell, String)> {
        let mut cmd = self.cmd.clone();
        Shell::ALL
            .iter()
            .map(|&s| {
                let mut buf = Vec::new();
                match s {
                    Shell::Bash => clap_complete::generate(Bash, &mut cmd, self.name, &mut buf),
                    Shell::Zsh => clap_complete::generate(Zsh, &mut cmd, self.name, &mut buf),
                    Shell::Fish => clap_complete::generate(Fish, &mut cmd, self.name, &mut buf),
                }
                (s, String::from_utf8(buf).expect("completion is not UTF-8"))
            })
            .collect()
    }

    /// Write the man pages into `dir`, or the binary-relative default —
    /// then prune pages left behind by subcommands that no longer exist.
    pub(crate) fn install_man(&self, dir: Option<PathBuf>) -> Result<()> {
        let dir = dir.map_or_else(man::default_man_dir, Ok)?;
        let pages = self.man_pages()?;
        for path in man::write_pages(&dir, &pages)? {
            println!("Installed {}", path.display());
        }
        self.remove_stale_pages(&dir, &pages)
    }

    /// Removes only files that still look like our generated pages.
    pub(crate) fn uninstall_man(&self, dir: Option<PathBuf>) -> Result<()> {
        let dir = dir.map_or_else(man::default_man_dir, Ok)?;
        let pages = self.man_pages()?;
        man::remove_pages(&dir, &pages)?;
        self.remove_stale_pages(&dir, &pages)
    }

    /// Only a command with subcommands owns a `<name>-*.1` namespace;
    /// `cm-*.1` could be anyone's.
    fn remove_stale_pages(&self, dir: &std::path::Path, pages: &[(String, String)]) -> Result<()> {
        if self.cmd.has_subcommands() {
            man::remove_stale_pages(dir, &format!("{}-", self.name), pages)?;
        }
        Ok(())
    }

    pub(crate) fn install_completions(&self, dir: Option<PathBuf>) -> Result<()> {
        let dir = dir.map_or_else(completions::default_share_dir, Ok)?;
        for path in completions::write_files(&dir, self.name, &self.completions())? {
            println!("Installed {}", path.display());
        }
        Ok(())
    }

    /// Removes only files that still look like generated scripts.
    pub(crate) fn uninstall_completions(&self, dir: Option<PathBuf>) -> Result<()> {
        let dir = dir.map_or_else(completions::default_share_dir, Ok)?;
        completions::remove_files(&dir, self.name, &self.completions())
    }
}

fn render_man(cmd: &clap::Command, base: &str, pages: &mut Vec<(String, String)>) -> Result<()> {
    // `Command::name` only takes 'static strings; the leaked names live
    // as long as the process, which exits right after rendering.
    let static_name: &'static str = Box::leak(base.to_string().into_boxed_str());
    let mut buf: Vec<u8> = Vec::new();
    clap_mangen::Man::new(cmd.clone().name(static_name))
        .source(concat!("container-distro ", env!("CARGO_PKG_VERSION")))
        .manual("Container Distro Manual")
        .render(&mut buf)
        .with_context(|| format!("failed to render {base}(1)"))?;
    pages.push((
        format!("{base}.1"),
        String::from_utf8(buf).context("man page is not UTF-8")?,
    ));
    for sub in cmd.get_subcommands() {
        render_man(sub, &format!("{base}-{}", sub.get_name()), pages)?;
    }
    Ok(())
}
