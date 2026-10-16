/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `cm` — a WSL-compatible command-line wrapper for Apple container
//! machines and `container distro` distros.

mod alias;
mod args;
mod backend;
mod forward;
mod install;
mod list;
mod manage;
mod run;

use std::process::{ExitCode, ExitStatus};

use anyhow::{Context, Result};
use args::{Action, Args, Backend};
use backend::Target;
use clap::{CommandFactory, Parser};

use super::docs::Docs;
use crate::catalog::wsl;
use crate::container;

pub fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cm: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<ExitCode> {
    if args.dev {
        crate::enable_dev();
    }
    let sel = args.backend();
    match args.action()? {
        Action::Version => version(),
        Action::InstallMan(dir) => docs().install_man(dir).map(|()| ExitCode::SUCCESS),
        Action::UninstallMan(dir) => docs().uninstall_man(dir).map(|()| ExitCode::SUCCESS),
        Action::InstallCompletions(dir) => {
            docs().install_completions(dir).map(|()| ExitCode::SUCCESS)
        }
        Action::UninstallCompletions(dir) => docs()
            .uninstall_completions(dir)
            .map(|()| ExitCode::SUCCESS),
        Action::InstallAlias(t) => alias::install(&t).map(|()| ExitCode::SUCCESS),
        Action::UninstallAlias(t) => alias::uninstall(&t).map(|()| ExitCode::SUCCESS),
        Action::Status => passthru(&["system", "status"]),
        Action::Update {
            version,
            force,
            dry_run,
        } => crate::setup::update::update(version.as_deref(), force, dry_run)
            .map(|()| ExitCode::SUCCESS),
        Action::Shutdown { system } => manage::shutdown(system),
        Action::List {
            running_only,
            quiet,
            verbosity,
            online,
            cache,
            catalog,
            arch,
        } => list::run(running_only, quiet, verbosity, online, cache, catalog, arch),
        Action::PurgeCache => {
            let (count, bytes) = wsl::purge_cache();
            println!(
                "Removed {count} cached .wsl file(s), freed {} MiB",
                bytes >> 20
            );
            Ok(ExitCode::SUCCESS)
        }
        Action::SetDefault(m) => manage::set_default(&m, sel),
        Action::Terminate(m) => manage::terminate(&m, sel),
        Action::Unregister(m) => manage::unregister(&m, sel),
        Action::Migrate(m) => manage::migrate(&m, sel),
        Action::Install(opts) => install::install(&opts),
        Action::Export { name, file } => install::export(&name, &file, sel),
        Action::Import {
            name,
            install_location,
            file,
            backend,
            arch,
            force,
            terminal,
        } => install::import(
            &name,
            install_location.as_deref(),
            &file,
            backend,
            arch,
            force,
            terminal,
        ),
        Action::Rename {
            old,
            new,
            backend,
            force,
        } => manage::rename(&old, &new, backend, force),
        Action::Forward { machine, mappings } => {
            forward::forward(machine.as_deref(), &mappings, sel)
        }
        Action::Run => run::run_in_target(&args),
    }
}

/// Spawn `container` with inherited stdio and forward its exit code.
pub(super) fn passthru(args: &[&str]) -> Result<ExitCode> {
    container::require_installed()?;
    let status = container::container_cmd()
        .args(args)
        .status()
        .with_context(|| format!("failed to run `container {}`", args.join(" ")))?;
    Ok(exit_code(status))
}

pub(super) fn exit_code(status: ExitStatus) -> ExitCode {
    ExitCode::from(status.code().map_or(1, |c| c.clamp(0, 255) as u8))
}

/// Resolve `-d` (or the default), starting services first. Names are
/// validated before they're passed to any `container` subcommand so a
/// flag-shaped value can't smuggle options into the inner CLI.
pub(super) fn target(name: Option<&str>, sel: Backend) -> Result<Target> {
    if let Some(n) = name {
        container::validate_name(n)?;
    }
    container::ensure_started()?;
    backend::resolve(name, sel)
}

/// Resolve a name that must exist as a machine or a distro.
pub(super) fn named_target(name: &str, sel: Backend) -> Result<Target> {
    target(Some(name), sel)
}

fn version() -> Result<ExitCode> {
    println!("cm {}", env!("CARGO_PKG_VERSION"));
    if container::resolve_binary().is_none() {
        println!("container: not installed");
        return Ok(ExitCode::SUCCESS);
    }
    passthru(&["--version"])
}

/// `cm(1)` and the `cm` completions.
fn docs() -> Docs {
    Docs::new(args::Args::command(), "cm")
}
