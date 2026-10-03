/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` — machine-like Linux distros for Apple `container`.
//!
//! A `container` CLI plugin (see [`plugin`]) that builds long-lived,
//! machine-like containers by composing `container create`/`exec`, adding
//! what `container machine` lacks: mounts outside `$HOME`, published
//! ports, export/import, and changing settings after creation.

mod cli;
mod ops;
mod plugin;
mod spec;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};
use spec::SpecChanges;

fn main() -> ExitCode {
    // Invoked as `container distro …`, `container` may pass the plugin
    // name through as the first argument; drop it if so.
    let mut argv: Vec<_> = std::env::args_os().collect();
    if argv.get(1).is_some_and(|a| a == plugin::PLUGIN_NAME) {
        argv.remove(1);
    }
    let cli = Cli::parse_from(argv);
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("container distro: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Create { name, opts, image } => ops::create(name, &opts, &image),
        Command::List {
            quiet,
            running,
            format,
        } => ops::list(quiet, running, format),
        Command::Inspect { names } => ops::inspect(&names),
        Command::Start { name } => ops::start(&name),
        Command::Stop { names } => ops::stop(&names),
        Command::Delete { force, names } => ops::delete(force, &names),
        Command::Run {
            name,
            user,
            root,
            workdir,
            env,
            shell,
            no_login,
            command,
        } => ops::run(ops::RunOpts {
            name,
            user,
            root,
            workdir,
            env,
            shell,
            no_login,
            command,
        }),
        Command::Set {
            name,
            cpus,
            memory,
            home_mount,
            add_volumes,
            rm_volumes,
            publish,
            unpublish,
        } => ops::set(
            &name,
            &SpecChanges {
                cpus,
                memory,
                home_mount,
                add_mounts: add_volumes,
                remove_mounts: rm_volumes,
                add_publish: publish,
                remove_publish: unpublish,
            },
        ),
        Command::SetDefault { name, .. } => ops::set_default(name.as_deref()),
        Command::Export { name, output } => ops::export(&name, output.as_deref()),
        Command::Import { name, file, opts } => ops::import(&name, &file, &opts),
        Command::InstallPlugin { plugin_dir } => plugin::install(plugin_dir),
        Command::UninstallPlugin { plugin_dir } => plugin::uninstall(plugin_dir),
    }
}
