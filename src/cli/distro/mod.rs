/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` — the CLI plugin front end over the
//! `container_distro` library.

mod args;
mod catalog;
mod install_all;
mod list;
mod run;
mod set;

use std::ffi::OsString;
use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, Parser};

use super::docs::Docs;
use crate::distro;
use crate::distro::spec::{Intent, OriginSource};
use crate::setup::layout::FrontEnd;
use crate::setup::{alias, plugin};
use args::{Cli, Command};

/// Parse `argv` as `container distro` arguments and run them.
pub(super) fn main_from(mut argv: Vec<OsString>) -> ExitCode {
    // Invoked as `container distro …`, `container` may pass the plugin
    // name through as the first argument; drop it if so.
    if argv.get(1).is_some_and(|a| a == plugin::PLUGIN_NAME) {
        argv.remove(1);
    }
    if argv.get(1).is_some_and(|a| a == "cli") {
        super::cm::lone_v_is_version(&mut argv[2..]);
    }
    let cli = Cli::parse_from(argv);
    if cli.dev {
        crate::enable_dev();
    }
    // `cli` hands over to `cm`, which reports its own errors and exit
    // codes.
    let command = match cli.command {
        Command::Cli(args) => return super::cm::main_args(*args),
        command => command,
    };
    match run(command) {
        Ok(code) => code,
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

fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Create { name, opts, image } => {
            println!(
                "{}",
                distro::create(name, &opts, &image, OriginSource::Image, &Intent::default())?
            );
        }
        Command::Catalog { command } => catalog::run(command)?,
        Command::List(args) => list::run(args)?,
        Command::Inspect { names } => distro::inspect(&names)?,
        Command::Start { name } => distro::start(&name)?,
        Command::Stop { names } => distro::stop(&names)?,
        Command::Delete { force, all, names } => {
            if all {
                distro::delete_all(force)?
            } else {
                distro::delete(force, &names)?
            }
        }
        Command::Run(args) => run::run(args)?,
        Command::Set(args) => set::run(args)?,
        Command::SyncProfiles => distro::sync_profiles()?,
        Command::Terminal { name } => distro::open_terminal(&name)?,
        Command::SetDefault { name, .. } => distro::set_default(name.as_deref())?,
        Command::Export { name, output } => distro::export(&name, output.as_deref())?,
        Command::Import { name, file, opts } => {
            println!(
                "{}",
                distro::import(
                    &name,
                    &file,
                    &opts,
                    distro::ImportSource::Local,
                    &Intent::default(),
                    None
                )?
            );
        }
        Command::Rename { old, new, force } => {
            distro::rename(&old, &new, force)?;
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
                distro::migrate_to_machine(&name, target_name, keep, &opts)?
            } else {
                distro::migrate(&name, target_name, keep, &opts)?
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
        Command::Version => return super::print_version("container-distro"),
        Command::InstallAlias { target } => alias::install(&target, FrontEnd::Distro)?,
        Command::UninstallAlias { target } => alias::uninstall(&target)?,
        Command::InstallAll(t) => install_all::install(t)?,
        Command::UninstallAll(t) => install_all::uninstall(t)?,
        Command::Cli(_) => unreachable!("handled in main_from"),
        Command::Update {
            version,
            force,
            dry_run,
        } => {
            crate::setup::update::update(version.as_deref(), force, dry_run)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
