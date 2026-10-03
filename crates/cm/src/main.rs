/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `cm` — a WSL-compatible command-line wrapper for Apple container
//! machines and `container distro` distros.

mod backend;
mod cli;
mod list;

use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::process::CommandExt;
use std::process::{ExitCode, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use backend::Target;
use clap::Parser;
use cli::{Action, Args, InstallOpts, ShellType};
use cm_core::container::{self, ArgvMode};
use cm_core::forward::PortMapping;
use container_distro::ops::{self as distro, CreateOptions, RunOpts};
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
        Action::SetDefault(m) => set_default(&m),
        Action::Terminate(m) => terminate(&m),
        Action::Unregister(m) => unregister(&m),
        Action::Install(opts) => install(&opts),
        Action::Forward { machine, mappings } => forward(machine.as_deref(), &mappings),
        Action::Run => run_in_target(&args),
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

/// Resolve `-d` (or the default), starting services first.
fn target(name: Option<&str>) -> Result<Target> {
    container::ensure_started()?;
    backend::resolve(name)
}

/// Resolve a name that must exist as a machine or a distro.
fn named_target(name: &str) -> Result<Target> {
    target(Some(name))
}

/// Replace this process with the machine's or distro's command, for TTY
/// passthrough.
fn run_in_target(args: &Args) -> Result<ExitCode> {
    match target(args.distribution.as_deref())? {
        Target::Machine(m) => run_in_machine(args, m.as_deref()),
        Target::Distro(d) => run_in_distro(args, d),
    }
}

fn run_in_machine(args: &Args, machine: Option<&str>) -> Result<ExitCode> {
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

fn list(running_only: bool, quiet: bool, verbosity: u8) -> Result<ExitCode> {
    container::ensure_started()?;
    let machines = container::list_machines()?;
    let distros = backend::distro_summaries(running_only)?;
    for name in backend::clashes(&machines, &distros) {
        eprintln!("cm: `{name}` is both a machine and a distro; `-d {name}` uses the distro");
    }
    let mut machine_entries = Vec::new();
    for m in machines {
        if running_only && !m.is_running() {
            continue;
        }
        let detail = if verbosity >= 2 {
            container::inspect_machine(&m.id).ok()
        } else {
            None
        };
        machine_entries.push(Entry::from_machine(&m, detail.as_ref()));
    }
    let entries = list::merge(
        machine_entries,
        distros.iter().map(Entry::from_distro).collect(),
    );
    if entries.is_empty() {
        if running_only {
            eprintln!("There are no running container machines or distros.");
        } else {
            eprintln!(
                "No container machines or distros are installed.\n\
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

/// WSL `-s`: a distro becomes the default; a machine becomes the default
/// and any default distro is cleared so the machine takes effect.
fn set_default(name: &str) -> Result<ExitCode> {
    match named_target(name)? {
        Target::Distro(d) => {
            distro::set_default(Some(&d))?;
            Ok(ExitCode::SUCCESS)
        }
        Target::Machine(_) => {
            let code = passthru(&["machine", "set-default", name])?;
            if code == ExitCode::SUCCESS && backend::distros_enabled() {
                distro::set_default(None)?;
            }
            Ok(code)
        }
    }
}

fn terminate(name: &str) -> Result<ExitCode> {
    match named_target(name)? {
        Target::Distro(d) => {
            distro::stop(&[d])?;
            Ok(ExitCode::SUCCESS)
        }
        Target::Machine(_) => passthru(&["machine", "stop", name]),
    }
}

fn unregister(name: &str) -> Result<ExitCode> {
    match named_target(name)? {
        Target::Distro(d) => {
            distro::delete(true, &[d])?;
            Ok(ExitCode::SUCCESS)
        }
        Target::Machine(_) => passthru(&["machine", "rm", name]),
    }
}

/// WSL `--shutdown`: stop every running machine and distro. Services stay
/// up unless `system` is set, since `container system stop` also stops
/// unrelated containers.
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
        if c != ExitCode::SUCCESS {
            code = c;
        }
    }
    let running: Vec<String> = backend::distro_summaries(true)?
        .into_iter()
        .map(|d| d.id)
        .collect();
    if !running.is_empty()
        && let Err(e) = distro::stop(&running)
    {
        eprintln!("cm: {e:#}");
        code = ExitCode::FAILURE;
    }
    if system {
        let c = passthru(&["system", "stop"])?;
        if c != ExitCode::SUCCESS {
            code = c;
        }
    }
    Ok(code)
}

/// WSL `--install`: create a machine (or, with distro options, a distro)
/// from an image, then open a shell in it.
fn install(opts: &InstallOpts) -> Result<ExitCode> {
    container::ensure_started()?;
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| container::default_machine_name(&opts.image));
    if opts.wants_distro() {
        if !backend::distros_enabled() {
            bail!("distro options need distro support (unset CM_BACKEND=machine)");
        }
        let create = CreateOptions {
            volumes: opts.shares.clone(),
            publish: opts.publish.clone(),
            cpus: opts.cpus.map(u64::from),
            memory: opts.memory.clone(),
            home_mount: opts.home_mount.unwrap_or_default(),
            ..CreateOptions::default()
        };
        let name = distro::create(Some(name), &create, &opts.image)?;
        if opts.no_launch {
            return Ok(ExitCode::SUCCESS);
        }
        let mut cmd = distro::run_command(RunOpts {
            name: Some(name),
            ..RunOpts::default()
        })?;
        return Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"));
    }

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

/// The IPv4 address of a running machine, booting it if needed.
fn machine_ip(machine: Option<&str>) -> Result<(String, IpAddr)> {
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
            bail!("failed to boot machine `{}`", m.id);
        }
        m = find()?.with_context(|| format!("machine `{label}` disappeared"))?;
    }
    let ip = parse_ip(m.ip_address.as_deref(), &m.id)?;
    Ok((m.id, ip))
}

/// The IPv4 address of a distro, booting it if needed.
fn distro_ip(name: &str) -> Result<(String, IpAddr)> {
    if !distro::find(name)?.is_running() {
        distro::start(name)?;
    }
    let info = distro::find(name)?;
    Ok((name.to_string(), parse_ip(info.ipv4(), name)?))
}

fn parse_ip(addr: Option<&str>, id: &str) -> Result<IpAddr> {
    addr.with_context(|| format!("`{id}` has no IP address"))?
        .split('/')
        .next()
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("failed to parse the IP address of `{id}`"))
}

/// Forward localhost ports to a machine's or distro's IP.
fn forward(name: Option<&str>, mappings: &[PortMapping]) -> Result<ExitCode> {
    let (id, ip) = match target(name)? {
        Target::Machine(m) => machine_ip(m.as_deref())?,
        Target::Distro(d) => distro_ip(&d)?,
    };
    eprintln!("Forwarding to `{id}` (Ctrl-C to stop)");
    cm_core::forward::forward(IpAddr::V4(Ipv4Addr::LOCALHOST), ip, mappings)?;
    Ok(ExitCode::SUCCESS)
}

fn version() -> Result<ExitCode> {
    println!("cm {}", env!("CARGO_PKG_VERSION"));
    passthru(&["--version"])
}
