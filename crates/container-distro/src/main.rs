/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` — the CLI plugin front end over the
//! `container_distro` library.

mod cli;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use cm_core::table::{columns, human_bytes, local_datetime};
use container_distro::ops::{self, DistroSummary, RunOpts};
use container_distro::plugin;
use container_distro::spec::{Automount, SpecChanges};

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

/// The man pages: `container-distro(1)` plus one per subcommand,
/// rendered from the clap definitions so they can't drift from `--help`.
/// The command is renamed so pages are `container-distro*.1`, not
/// `container distro*.1`.
fn man_pages() -> Result<Vec<(String, String)>> {
    let mut pages = Vec::new();
    render_man(&Cli::command(), "container-distro", &mut pages)?;
    Ok(pages)
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

/// Write the man pages into `dir`, or the binary-relative default.
fn install_man(dir: Option<PathBuf>) -> Result<()> {
    let dir = dir.map_or_else(cm_core::man::default_man_dir, Ok)?;
    for path in cm_core::man::write_pages(&dir, &man_pages()?)? {
        println!("Installed {}", path.display());
    }
    Ok(())
}

fn print_table(rows: &[DistroSummary]) {
    let dash = || "-".to_string();
    let mut table = vec![
        [
            "NAME", "IMAGE", "CREATED", "STATE", "IP", "CPUS", "MEMORY", "DISK", "MOUNTS", "PORTS",
            "DEFAULT",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for r in rows {
        table.push(vec![
            r.id.clone(),
            r.image.clone().unwrap_or_else(dash),
            r.created_date
                .as_deref()
                .and_then(local_datetime)
                .unwrap_or_else(dash),
            r.status.clone(),
            r.ip_address.clone().unwrap_or_else(dash),
            r.cpus.map_or_else(dash, |c| c.to_string()),
            r.memory.map_or_else(dash, human_bytes),
            r.disk_size.map_or_else(dash, human_bytes),
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
            network,
            ssh,
            no_ssh,
            sudo,
            no_sudo,
            automount,
            no_automount,
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
                network,
                ssh: if ssh {
                    Some(true)
                } else if no_ssh {
                    Some(false)
                } else {
                    None
                },
                admin: if sudo {
                    Some(true)
                } else if no_sudo {
                    Some(false)
                } else {
                    None
                },
                automount: automount.or(no_automount.then_some(Automount::None)),
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
        Command::Migrate {
            name,
            target_name,
            keep,
            opts,
        } => {
            println!("{}", ops::migrate(&name, target_name, keep, &opts)?);
        }
        Command::InstallPlugin { plugin_dir, from } => {
            plugin::install(plugin_dir, from.as_deref())?;
        }
        Command::InstallMan { dir } => install_man(dir)?,
        Command::UninstallMan { dir } => {
            let dir = dir.map_or_else(cm_core::man::default_man_dir, Ok)?;
            cm_core::man::remove_pages(&dir, &man_pages()?)?;
        }
        Command::UninstallPlugin { plugin_dir } => plugin::uninstall(plugin_dir)?,
    }
    Ok(())
}
