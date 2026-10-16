/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro install-all` / `uninstall-all`: everything around
//! an installed `container-distro` binary in one step — man pages and
//! completions for both interfaces, the `cm` alias (plus any extra
//! `--alias`), and the `container distro` plugin registration.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use crate::naming::APP_NAME;
use crate::setup::layout::{self, FrontEnd};
use crate::setup::{alias, plugin};

/// Where install-all reads and writes.
#[derive(Debug, Args)]
pub struct Target {
    /// Install prefix — `<prefix>/bin/container-distro` is the binary
    /// aliases run (default: the prefix of the running binary)
    #[arg(long, value_name = "DIR")]
    prefix: Option<PathBuf>,
    /// Write into DIR instead of the prefix, laid out as the prefix —
    /// for packaging: aliases still run `<prefix>/bin/container-distro`
    #[arg(long, value_name = "DIR", requires = "prefix")]
    stage: Option<PathBuf>,
    /// Another `cm`-style alias besides `cm` (repeatable), e.g. `wsl`
    #[arg(long = "alias", value_name = "NAME")]
    aliases: Vec<String>,
    /// Leave the `container distro` plugin registration alone
    #[arg(long, conflicts_with = "plugin_dir")]
    no_plugin: bool,
    /// Plugin directory (default: <container prefix>/libexec/container-plugins)
    #[arg(long, value_name = "DIR")]
    plugin_dir: Option<PathBuf>,
}

/// The resolved prefix (baked into aliases) and root (written to).
struct Dirs {
    prefix: PathBuf,
    root: PathBuf,
}

impl Dirs {
    fn of(t: &Target) -> Result<Self> {
        let prefix = match &t.prefix {
            Some(p) => p.clone(),
            None => {
                let exe = layout::current_exe()?;
                if exe.file_name().is_some_and(|n| n == plugin::PLUGIN_NAME) {
                    bail!(
                        "this is the `container distro` plugin copy — pass --prefix, \
                         or run install-all from `container-distro`"
                    );
                }
                layout::current_prefix()?
            }
        };
        let root = t.stage.clone().unwrap_or_else(|| prefix.clone());
        Ok(Self { prefix, root })
    }

    fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    fn man(&self) -> PathBuf {
        layout::share_dir(&self.root).join("man/man1")
    }

    fn share(&self) -> PathBuf {
        layout::share_dir(&self.root)
    }
}

/// `cm` plus the extra aliases, validated as bare names.
fn alias_names(t: &Target) -> Result<Vec<String>> {
    let mut names = vec!["cm".to_string()];
    for a in &t.aliases {
        if a.is_empty() || a.contains('/') || a == APP_NAME {
            bail!("--alias takes a bare name other than {APP_NAME}, not `{a}`");
        }
        if !names.contains(a) {
            names.push(a.clone());
        }
    }
    Ok(names)
}

/// A named install step.
type Step<'a> = (&'static str, Box<dyn FnOnce() -> Result<()> + 'a>);

/// Run every step, reporting failures as they happen, and fail at the
/// end if any did — one bad step shouldn't leave the rest undone.
fn run_steps(steps: Vec<Step<'_>>) -> Result<()> {
    let mut failed = Vec::new();
    for (what, step) in steps {
        if let Err(e) = step() {
            eprintln!("container distro: {what}: {e:#}");
            failed.push(what);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        bail!("incomplete — failed: {}", failed.join(", "))
    }
}

pub(super) fn install(t: Target) -> Result<()> {
    let d = Dirs::of(&t)?;
    let names = alias_names(&t)?;
    let plugin_dir = t.plugin_dir.clone();
    let written = d.bin().join(APP_NAME);
    if !written.is_file() {
        bail!(
            "{} not found — install-all sets up around an installed binary \
             (`cargo install container-distro`, or a staged payload)",
            written.display()
        );
    }
    // Aliases run the *installed* binary, even when writing a stage.
    let baked = d.prefix.join("bin").join(APP_NAME);
    let man = d.man();
    let share = d.share();
    let mut steps: Vec<Step> = vec![
        (
            "man pages",
            Box::new(|| {
                super::docs().install_man(Some(man.clone()))?;
                crate::cli::cm::docs().install_man(Some(man.clone()))
            }),
        ),
        (
            "completions",
            Box::new(|| {
                super::docs().install_completions(Some(share.clone()))?;
                crate::cli::cm::docs().install_completions(Some(share.clone()))
            }),
        ),
    ];
    for name in names {
        let link = d.bin().join(&name);
        let (baked, man) = (baked.clone(), man.clone());
        steps.push((
            "aliases",
            Box::new(move || {
                alias::install_at(
                    &link,
                    &baked,
                    layout::front_end_args(FrontEnd::Cm),
                    FrontEnd::Cm,
                    Some(&man),
                    name == "cm",
                )
            }),
        ));
    }
    if !t.no_plugin {
        steps.push((
            "plugin",
            Box::new(move || plugin::install(plugin_dir, Some(&written))),
        ));
    }
    run_steps(steps)
}

pub(super) fn uninstall(t: Target) -> Result<()> {
    if t.stage.is_some() {
        bail!("--stage only applies to install-all");
    }
    let d = Dirs::of(&t)?;
    let names = alias_names(&t)?;
    let plugin_dir = t.plugin_dir.clone();
    let man = d.man();
    let share = d.share();
    let mut steps: Vec<Step> = Vec::new();
    for name in names {
        let link = d.bin().join(&name);
        let man = man.clone();
        steps.push((
            "aliases",
            Box::new(move || alias::uninstall_at(&link, Some(&man), name == "cm")),
        ));
    }
    steps.push((
        "man pages",
        Box::new(|| {
            super::docs().uninstall_man(Some(man.clone()))?;
            crate::cli::cm::docs().uninstall_man(Some(man.clone()))
        }),
    ));
    steps.push((
        "completions",
        Box::new(|| {
            super::docs().uninstall_completions(Some(share.clone()))?;
            crate::cli::cm::docs().uninstall_completions(Some(share.clone()))
        }),
    ));
    if !t.no_plugin {
        steps.push(("plugin", Box::new(move || plugin::uninstall(plugin_dir))));
    }
    run_steps(steps)
}

#[cfg(test)]
fn dirs_for(prefix: &std::path::Path, stage: Option<&std::path::Path>) -> Dirs {
    Dirs {
        prefix: prefix.to_path_buf(),
        root: stage.unwrap_or(prefix).to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn target(aliases: &[&str]) -> Target {
        Target {
            prefix: None,
            stage: None,
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            no_plugin: true,
            plugin_dir: None,
        }
    }

    #[test]
    fn cm_is_always_installed_and_extras_are_bare_names() {
        assert_eq!(alias_names(&target(&[])).unwrap(), ["cm"]);
        assert_eq!(
            alias_names(&target(&["wsl", "cm", "wsl"])).unwrap(),
            ["cm", "wsl"]
        );
        for bad in ["", "a/b", "container-distro"] {
            assert!(alias_names(&target(&[bad])).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn stage_writes_under_root_with_prefix_layout() {
        let d = dirs_for(Path::new("/usr/local"), Some(Path::new("/s/payload")));
        assert_eq!(d.bin(), Path::new("/s/payload/bin"));
        assert_eq!(d.man(), Path::new("/s/payload/share/man/man1"));
        assert_eq!(d.prefix, Path::new("/usr/local"));
    }
}
