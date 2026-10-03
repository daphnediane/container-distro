/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` — the CLI plugin front end over the
//! `container_distro` library.

mod cli;

use std::os::unix::process::CommandExt;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use cm_core::table::{columns, human_bytes};
use container_distro::ops::{self, DistroSummary, RunOpts};
use container_distro::plugin;
use container_distro::spec::SpecChanges;

use cli::{Cli, Command, Format};

fn main() -> ExitCode {
    // Invoked as `container distro …`, `container` may pass the plugin
    // name through as the first argument; drop it if so.
    let mut argv: Vec<_> = std::env::args_os().collect();
    if argv.get(1).is_some_and(|a| a == plugin::PLUGIN_NAME) {
        argv.remove(1);
    }
    let cli = Cli::parse_from(argv);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("container distro: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_table(rows: &[DistroSummary]) {
    let dash = || "-".to_string();
    let mut table = vec![
        [
            "NAME", "IMAGE", "STATE", "IP", "CPUS", "MEMORY", "MOUNTS", "PORTS", "DEFAULT",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for r in rows {
        table.push(vec![
            r.id.clone(),
            r.image.clone().unwrap_or_else(dash),
            r.status.clone(),
            r.ip_address.clone().unwrap_or_else(dash),
            r.cpus.map_or_else(dash, |c| c.to_string()),
            r.memory.map_or_else(dash, human_bytes),
            if r.mounts.is_empty() {
                dash()
            } else {
                r.mounts.join(",")
            },
            if r.ports.is_empty() {
                dash()
            } else {
                r.ports.join(",")
            },
            if r.default { "*".into() } else { String::new() },
        ]);
    }
    for line in columns(&table, 2) {
        println!("{}", line.trim_end());
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Create { name, opts, image } => {
            println!("{}", ops::create(name, &opts, &image)?);
        }
        Command::List {
            quiet,
            running,
            format,
        } => {
            cm_core::container::ensure_started()?;
            let rows = ops::summaries(running)?;
            if quiet {
                rows.iter().for_each(|r| println!("{}", r.id));
            } else if format == Format::Json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                print_table(&rows);
            }
        }
        Command::Inspect { names } => ops::inspect(&names)?,
        Command::Start { name } => ops::start(&name)?,
        Command::Stop { names } => ops::stop(&names)?,
        Command::Delete { force, names } => ops::delete(force, &names)?,
        Command::Run {
            name,
            user,
            root,
            workdir,
            env,
            shell,
            no_login,
            command,
        } => {
            let mut cmd = ops::run_command(RunOpts {
                name,
                user,
                root,
                workdir,
                env,
                shell,
                no_login,
                command,
            })?;
            return Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"));
        }
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
        )?,
        Command::SetDefault { name, .. } => ops::set_default(name.as_deref())?,
        Command::Export { name, output } => ops::export(&name, output.as_deref())?,
        Command::Import { name, file, opts } => {
            println!("{}", ops::import(&name, &file, &opts)?);
        }
        Command::InstallPlugin { plugin_dir } => plugin::install(plugin_dir)?,
        Command::UninstallPlugin { plugin_dir } => plugin::uninstall(plugin_dir)?,
    }
    Ok(())
}
