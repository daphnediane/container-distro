/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `cm` — a WSL-compatible command-line wrapper for Apple container machines.

mod cli;
mod list;

use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::process::CommandExt;
use std::process::{ExitCode, ExitStatus, Stdio};

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Action, Args, InstallOpts, ShellType};
use cm_core::container::{self, ArgvMode};
use cm_core::forward::PortMapping;
use list::Entry;

fn main() -> ExitCode {
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
    match args.action()? {
        Action::Version => version(),
        Action::Status => passthru(&["system", "status"]),
        Action::Shutdown { system } => shutdown(system),
        Action::List {
            running_only,
            quiet,
            verbosity,
        } => list(running_only, quiet, verbosity),
        Action::SetDefault(m) => passthru(&["machine", "set-default", &m]),
        Action::Terminate(m) => {
            container::ensure_started()?;
            passthru(&["machine", "stop", &m])
        }
        Action::Unregister(m) => {
            container::ensure_started()?;
            passthru(&["machine", "rm", &m])
        }
        Action::Install(opts) => install(&opts),
        Action::Forward { machine, mappings } => forward(machine.as_deref(), &mappings),
        Action::Run => run_in_machine(&args),
    }
}

/// Spawn `container` with inherited stdio and forward its exit code.
fn passthru(args: &[&str]) -> Result<ExitCode> {
    let status = container::container_cmd()
        .args(args)
        .status()
        .with_context(|| format!("failed to run `container {}`", args.join(" ")))?;
    Ok(exit_code(status))
}

fn exit_code(status: ExitStatus) -> ExitCode {
    ExitCode::from(status.code().map_or(1, |c| c.clamp(0, 255) as u8))
}

/// Replace this process with `container machine run` for TTY passthrough.
fn run_in_machine(args: &Args) -> Result<ExitCode> {
    container::ensure_started()?;
    let executable = match args.shell_type {
        Some(ShellType::Standard) if args.command.is_empty() => Some(container::resolve_shell(
            args.distribution.as_deref(),
            args.user.as_deref(),
        )),
        _ => None,
    };
    let mut cmd = container::run_command(
        args.distribution.as_deref(),
        args.user.as_deref(),
        args.cd.as_deref(),
        &args.env,
        executable.as_deref(),
        &args.command,
        argv_mode(args),
    );
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container machine run`"))
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

fn list(running_only: bool, quiet: bool, verbosity: u8) -> Result<ExitCode> {
    container::ensure_started()?;
    let mut entries: Vec<Entry> = Vec::new();
    for m in container::list_machines()? {
        if running_only && !m.is_running() {
            continue;
        }
        let detail = if verbosity >= 2 {
            container::inspect_machine(&m.id).ok()
        } else {
            None
        };
        entries.push(Entry::from_machine(&m, detail.as_ref()));
    }
    if entries.is_empty() {
        if running_only {
            eprintln!("There are no running container machines.");
        } else {
            eprintln!(
                "No container machines are installed.\n\
                 Use `cm --install <image>` to create one."
            );
        }
        return Ok(ExitCode::FAILURE);
    }
    let out = if quiet {
        list::format_quiet(&entries)
    } else if verbosity > 0 {
        list::format_verbose(&entries, verbosity)
    } else {
        list::format_simple(&entries)
    };
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// WSL `--shutdown`: stop every running machine. Services stay up unless
/// `system` is set, since `container system stop` also stops unrelated
/// containers.
fn shutdown(system: bool) -> Result<ExitCode> {
    if !container::system_running() {
        return Ok(ExitCode::SUCCESS);
    }
    let mut code = ExitCode::SUCCESS;
    for m in container::list_machines()?
        .iter()
        .filter(|m| m.is_running())
    {
        let c = passthru(&["machine", "stop", &m.id])?;
        if !c.eq(&ExitCode::SUCCESS) {
            code = c;
        }
    }
    if system {
        let c = passthru(&["system", "stop"])?;
        if !c.eq(&ExitCode::SUCCESS) {
            code = c;
        }
    }
    Ok(code)
}

/// WSL `--install`: create a machine from an image, then open a shell in it.
fn install(opts: &InstallOpts) -> Result<ExitCode> {
    container::ensure_started()?;
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| container::default_machine_name(&opts.image));
    let mut cmd = container::container_cmd();
    cmd.args(["machine", "create", "--name", &name]);
    if let Some(c) = opts.cpus {
        cmd.arg("--cpus").arg(c.to_string());
    }
    if let Some(m) = &opts.memory {
        cmd.args(["--memory", m]);
    }
    if let Some(h) = opts.home_mount {
        cmd.args(["--home-mount", h.as_str()]);
    }
    let status = cmd
        .arg(&opts.image)
        .status()
        .context("failed to run `container machine create`")?;
    if !status.success() || opts.no_launch {
        return Ok(exit_code(status));
    }
    let mut cmd = container::run_command(Some(&name), None, None, &[], None, &[], ArgvMode::Shell);
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container machine run`"))
}

/// Forward localhost ports to a machine's IP, booting it if needed.
fn forward(machine: Option<&str>, mappings: &[PortMapping]) -> Result<ExitCode> {
    container::ensure_started()?;
    let find = || -> Result<Option<container::Machine>> {
        Ok(container::list_machines()?
            .into_iter()
            .find(|m| machine.map_or(m.default, |n| m.id == n)))
    };
    let label = machine.unwrap_or("default");
    let mut m = find()?.with_context(|| format!("no `{label}` machine found"))?;
    if !m.is_running() {
        let status = container::run_command(
            Some(&m.id),
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
            return Ok(exit_code(status));
        }
        m = find()?.with_context(|| format!("machine `{label}` disappeared"))?;
    }
    let ip: IpAddr = m
        .ip_address
        .as_deref()
        .with_context(|| format!("machine `{}` has no IP address", m.id))?
        .split('/')
        .next()
        .unwrap_or_default()
        .parse()
        .context("failed to parse the machine's IP address")?;
    eprintln!("Forwarding to machine `{}` (Ctrl-C to stop)", m.id);
    cm_core::forward::forward(IpAddr::V4(Ipv4Addr::LOCALHOST), ip, mappings)?;
    Ok(ExitCode::SUCCESS)
}

fn version() -> Result<ExitCode> {
    println!("cm {}", env!("CARGO_PKG_VERSION"));
    passthru(&["--version"])
}
