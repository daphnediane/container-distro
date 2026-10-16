/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `cm` — a WSL-compatible command-line wrapper for Apple container
//! machines and `container distro` distros.

pub(super) mod args;
mod backend;
mod forward;
mod install;
mod list;
mod manage;
mod run;

use std::ffi::OsString;
use std::process::ExitCode;

use anyhow::{Context, Result};
use args::{Action, Args, Backend};
use backend::Target;
use clap::{CommandFactory, Parser};

use super::docs::Docs;
use super::exit_code;
use crate::catalog::wsl;
use crate::container;
use crate::setup::alias;
use crate::setup::layout::FrontEnd;

/// Parse `argv` as `cm` arguments and run them. A lone `-v` is
/// `--version`, as with `wsl -v`; `-v` everywhere else stays
/// `--verbose`, which needs `--list`.
pub(super) fn main_from(mut argv: Vec<OsString>) -> ExitCode {
    lone_v_is_version(&mut argv[1..]);
    main_args(Args::parse_from(argv))
}

/// WSL's `-v` overload: alone it is `--version` (`wsl -v`); anywhere
/// else it stays `--verbose`, which needs `--list`. `args` is
/// everything after the program (or after `container-distro cli`).
pub(super) fn lone_v_is_version(args: &mut [OsString]) {
    if let [only] = args
        && only == "-v"
    {
        *only = "--version".into();
    }
}

/// Run already-parsed `cm` arguments — also `container-distro cli`'s
/// entry — reporting errors as `cm:`.
pub(super) fn main_args(args: Args) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cm: {e:#}");
            ExitCode::FAILURE
        }
    }
}

pub(super) fn run(args: Args) -> Result<ExitCode> {
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
        Action::InstallAlias(t) => alias::install(&t, FrontEnd::Cm).map(|()| ExitCode::SUCCESS),
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
    super::print_version("cm")
}

/// `cm(1)` and the `cm` completions.
fn docs() -> Docs {
    Docs::new(args::Args::command(), "cm")
}
