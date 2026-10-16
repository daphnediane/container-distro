/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` — the CLI plugin front end over the
//! `container_distro` library.

mod args;
mod catalog;
mod list;
mod run;
mod set;

use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, Parser};

use super::docs::Docs;
use crate::ops;
use crate::plugin;
use crate::spec::{Intent, OriginSource};
use args::{Cli, Command};

pub fn main() -> ExitCode {
    // Invoked as `container distro …`, `container` may pass the plugin
    // name through as the first argument; drop it if so.
    let mut argv: Vec<_> = std::env::args_os().collect();
    if argv.get(1).is_some_and(|a| a == plugin::PLUGIN_NAME) {
        argv.remove(1);
    }
    let cli = Cli::parse_from(argv);
    if cli.dev {
        crate::enable_dev();
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("container distro: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `container-distro(1)` plus one page per subcommand, and the
/// completions — under the binary's name, not `container distro`.
fn docs() -> Docs {
    Docs::new(Cli::command(), "container-distro")
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Create { name, opts, image } => {
            println!(
                "{}",
                ops::create(name, &opts, &image, OriginSource::Image, &Intent::default())?
            );
        }
        Command::Catalog { command } => catalog::run(command)?,
        Command::List(args) => list::run(args)?,
        Command::Inspect { names } => ops::inspect(&names)?,
        Command::Start { name } => ops::start(&name)?,
        Command::Stop { names } => ops::stop(&names)?,
        Command::Delete { force, all, names } => {
            if all {
                ops::delete_all(force)?
            } else {
                ops::delete(force, &names)?
            }
        }
        Command::Run(args) => run::run(args)?,
        Command::Set(args) => set::run(args)?,
        Command::SyncProfiles => ops::sync_profiles()?,
        Command::Terminal { name } => ops::open_terminal(&name)?,
        Command::SetDefault { name, .. } => ops::set_default(name.as_deref())?,
        Command::Export { name, output } => ops::export(&name, output.as_deref())?,
        Command::Import { name, file, opts } => {
            println!(
                "{}",
                ops::import(
                    &name,
                    &file,
                    &opts,
                    ops::ImportSource::Local,
                    &Intent::default(),
                    None
                )?
            );
        }
        Command::Rename { old, new, force } => {
            ops::rename(&old, &new, force)?;
            println!("`{old}` renamed to `{new}`");
        }
        Command::Migrate {
            name,
            out,
            target_name,
            keep,
            opts,
            ..
        } => {
            let result = if out {
                ops::migrate_to_machine(&name, target_name, keep, &opts)?
            } else {
                ops::migrate(&name, target_name, keep, &opts)?
            };
            println!("{result}");
        }
        Command::InstallPlugin { plugin_dir, from } => {
            plugin::install(plugin_dir, from.as_deref())?;
        }
        Command::InstallMan { dir } => docs().install_man(dir)?,
        Command::UninstallMan { dir } => docs().uninstall_man(dir)?,
        Command::InstallCompletions { dir } => docs().install_completions(dir)?,
        Command::UninstallCompletions { dir } => docs().uninstall_completions(dir)?,
        Command::UninstallPlugin { plugin_dir } => plugin::uninstall(plugin_dir)?,
        Command::Update {
            version,
            force,
            dry_run,
        } => {
            crate::update::update(version.as_deref(), force, dry_run)?;
        }
    }
    Ok(())
}
