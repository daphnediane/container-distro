/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Opening a shell or running a command in a machine or distro.

use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::process::{ExitCode, Stdio};

use anyhow::{Context, Result, bail};

use super::args::{Args, ShellType};
use super::backend::Target;
use super::target;
use crate::container::{self, ArgvMode};
use crate::distro::{self, RunOpts};

/// Replace this process with the machine's or distro's command, for TTY
/// passthrough.
pub(super) fn run_in_target(args: &Args) -> Result<ExitCode> {
    if args.terminal {
        // Open the distro in Terminal.app via its `.terminal`
        // launcher — the profile's `run -n NAME` runs in the new window.
        return match target(args.distribution.as_deref(), args.backend())? {
            Target::Distro(d) => distro::open_terminal(&d).map(|()| ExitCode::SUCCESS),
            Target::Machine(_) => bail!("--terminal opens distros; machines have no profile"),
        };
    }
    if let Some(u) = args.user.as_deref() {
        container::validate_user(u)?;
    }
    match target(args.distribution.as_deref(), args.backend())? {
        Target::Machine(m) => run_in_machine(args, m.as_deref()),
        Target::Distro(d) => run_in_distro(args, d),
    }
}

fn run_in_machine(args: &Args, machine: Option<&str>) -> Result<ExitCode> {
    if argv_mode(args) == ArgvMode::Exact
        && !args.command.is_empty()
        && let Some(mut cmd) = machine_exec(args, machine)?
    {
        return Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"));
    }
    let executable = match args.shell_type {
        Some(ShellType::Standard) if args.command.is_empty() => {
            Some(container::resolve_shell(machine, args.user.as_deref()))
        }
        _ => None,
    };
    let mut cmd = container::run_command(
        machine,
        args.user.as_deref(),
        args.cd.as_deref(),
        &args.env,
        executable.as_deref(),
        &args.command,
        argv_mode(args),
    );
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container machine run`"))
}

/// Exact-argv commands go straight to the machine's backing container via
/// `container exec`, avoiding `machine run`'s shell re-evaluation. Returns
/// `None` to fall back to `machine run` (with per-argument quoting) when
/// the inspect data lacks a container ID or user.
fn machine_exec(args: &Args, machine: Option<&str>) -> Result<Option<std::process::Command>> {
    let Ok(mut detail) = container::inspect_machine_or_default(machine) else {
        return Ok(None);
    };
    if !detail.is_running() {
        let status = container::run_command(
            Some(&detail.id),
            None,
            None,
            &[],
            None,
            &["true".to_string()],
            ArgvMode::Shell,
        )
        .stdin(Stdio::null())
        .status()
        .context("failed to boot the machine")?;
        if !status.success() {
            bail!("failed to boot machine `{}`", detail.id);
        }
        detail = container::inspect_machine(&detail.id)?;
    }
    let cwd = std::env::current_dir().ok();
    let home = std::env::var("HOME").ok();
    Ok(container::machine_exec_command(
        &detail,
        args.user.as_deref(),
        args.cd.as_deref(),
        &args.env,
        &args.command,
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        cwd.as_deref(),
        home.as_deref(),
    ))
}

/// Distros run through `container exec`, which is argv-exact already; the
/// WSL shell semantics of a bare / `--` command line map to `--shell`.
fn run_in_distro(args: &Args, name: String) -> Result<ExitCode> {
    let mut cmd = distro::run_command(RunOpts {
        name: Some(name),
        user: args.user.clone(),
        root: false,
        workdir: args.cd.clone(),
        env: args.env.clone(),
        shell: argv_mode(args) == ArgvMode::Shell && !args.command.is_empty(),
        no_login: args.shell_type == Some(ShellType::Standard),
        command: args.command.clone(),
    })?;
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"))
}

/// WSL semantics: `-e` (and `--shell-type none`) execute argv exactly;
/// a bare or `--` command line is evaluated by the shell.
fn argv_mode(args: &Args) -> ArgvMode {
    if args.exec || args.shell_type == Some(ShellType::None) {
        ArgvMode::Exact
    } else {
        ArgvMode::Shell
    }
}

/// Open a distro after create/import — `cm -d NAME`'s login shell
/// exec'd into this console, or (`--terminal`) a Terminal.app window
/// via the distro's `.terminal` profile.
pub(super) fn launch(name: &str, terminal_window: bool) -> Result<ExitCode> {
    if terminal_window {
        return distro::open_terminal(name).map(|()| ExitCode::SUCCESS);
    }
    let mut cmd = distro::run_command(RunOpts {
        name: Some(name.to_string()),
        ..RunOpts::default()
    })?;
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"))
}
