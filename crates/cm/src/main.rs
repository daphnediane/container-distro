/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `cm` — a WSL-compatible command-line wrapper for Apple container
//! machines and `container distro` distros.

mod alias;
mod backend;
mod cli;
mod list;

use std::io::IsTerminal;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use backend::Target;
use clap::{CommandFactory, Parser};
use cli::{Action, Args, InstallOpts, InstallSource, ShellType};
use cm_core::container::{self, ArgvMode};
use cm_core::forward::PortMapping;
use cm_core::{catalog, wsl};
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
        Action::InstallMan(dir) => install_man(dir),
        Action::UninstallMan(dir) => uninstall_man(dir),
        Action::InstallCompletions(dir) => install_completions(dir),
        Action::UninstallCompletions(dir) => uninstall_completions(dir),
        Action::InstallAlias(t) => alias::install(&t).map(|()| ExitCode::SUCCESS),
        Action::UninstallAlias(t) => alias::uninstall(&t).map(|()| ExitCode::SUCCESS),
        Action::Status => passthru(&["system", "status"]),
        Action::Shutdown { system } => shutdown(system),
        Action::List {
            running_only,
            quiet,
            verbosity,
            online,
            catalog,
        } => list(running_only, quiet, verbosity, online, catalog),
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

/// Resolve `-d` (or the default), starting services first. Names are
/// validated before they're passed to any `container` subcommand so a
/// flag-shaped value can't smuggle options into the inner CLI.
fn target(name: Option<&str>) -> Result<Target> {
    if let Some(n) = name {
        container::validate_name(n)?;
    }
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
    if let Some(u) = args.user.as_deref() {
        container::validate_user(u)?;
    }
    match target(args.distribution.as_deref())? {
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

fn list(
    running_only: bool,
    quiet: bool,
    verbosity: u8,
    online: bool,
    catalog_file: Option<PathBuf>,
) -> Result<ExitCode> {
    // The catalog is baked in (or a `--catalog` file); listing it never
    // touches `container`.
    if online {
        let entries = catalog::load(catalog_file.as_deref())?;
        print!("{}", catalog::render(&entries));
        return Ok(ExitCode::SUCCESS);
    }
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

/// The positional `--install` argument is a catalog name when it
/// matches one (case-insensitive), otherwise an image reference —
/// catalog names contain no `/`/`:`/`@`, so a qualified ref always
/// bypasses the catalog, as does `--from-image` explicitly.
fn resolve_image(entries: &[catalog::Entry], source: &InstallSource) -> Result<String> {
    Ok(match source {
        InstallSource::Default => entries
            .first()
            .context("the distro catalog is empty")?
            .image
            .clone(),
        InstallSource::CatalogOrImage(a) => {
            catalog::lookup(entries, a).map_or_else(|| a.clone(), |e| e.image.clone())
        }
        InstallSource::Image(r) => r.clone(),
        InstallSource::File(_) => bail!("internal error: file source resolved as image"),
    })
}

/// WSL `--install`: create a machine (or, with distro options, a distro)
/// from an image, then open a shell in it.
fn install(opts: &InstallOpts) -> Result<ExitCode> {
    container::ensure_started()?;
    let create = CreateOptions {
        volumes: opts.shares.clone(),
        publish: opts.publish.clone(),
        cpus: opts.cpus.map(u64::from),
        memory: opts.memory.clone(),
        home_mount: opts.home_mount,
        restricted: opts.restricted,
        ..CreateOptions::default()
    };
    if let InstallSource::File(file) = &opts.source {
        return install_from_file(opts, &create, file);
    }
    let entries = catalog::load(opts.catalog.as_deref())?;
    let image = resolve_image(&entries, &opts.source)?;
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| container::default_machine_name(&image));
    container::validate_name(&name)?;
    if opts.wants_distro() {
        if !backend::distros_enabled() {
            bail!("distro options need distro support (unset CM_BACKEND=machine)");
        }
        let name = distro::create(Some(name), &create, &image)?;
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
        .arg(&image)
        .status()
        .context("failed to run `container machine create`")?;
    if !status.success() || opts.no_launch {
        return Ok(exit_code(status));
    }
    let mut cmd = container::run_command(Some(&name), None, None, &[], None, &[], ArgvMode::Shell);
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container machine run`"))
}

/// `--install --from-file`: import a rootfs tar or `.wsl` package as a
/// distro — machines can't boot a bare rootfs, so this is always the
/// distro path regardless of `--distro`.
fn install_from_file(
    opts: &InstallOpts,
    create: &CreateOptions,
    file: &std::path::Path,
) -> Result<ExitCode> {
    if !backend::distros_enabled() {
        bail!("--from-file needs distro support (unset CM_BACKEND=machine)");
    }
    if !file.is_file() {
        bail!("{} does not exist", file.display());
    }
    // --name is used verbatim; a derived name (the .wsl manifest's
    // oobe.defaultName, else the file stem) is sanitized to a valid one.
    let name = match &opts.name {
        Some(n) => n.clone(),
        None => container::default_machine_name(
            &wsl::distribution_name(file)
                .ok()
                .flatten()
                .unwrap_or_else(|| file_default_name(file)),
        ),
    };
    container::validate_name(&name)?;
    let name = distro::import(&name, file, create)?;
    if opts.no_launch {
        return Ok(ExitCode::SUCCESS);
    }
    let mut cmd = distro::run_command(RunOpts {
        name: Some(name),
        ..RunOpts::default()
    })?;
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"))
}

/// Derive a default distro name from a rootfs filename: drop the
/// archive suffixes (`rocky-9.wsl` → `rocky-9`, `fs.tar.gz` → `fs`).
fn file_default_name(file: &std::path::Path) -> String {
    let Some(mut stem) = file.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return "distro".to_string();
    };
    for ext in [".tar.gz", ".tgz", ".tar.xz", ".wsl", ".tar"] {
        if stem.len() > ext.len() && stem.to_lowercase().ends_with(ext) {
            stem.truncate(stem.len() - ext.len());
            break;
        }
    }
    stem
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

/// The `cm(1)` man page, rendered from the clap definition so it can
/// never drift from `--help`.
fn man_pages() -> Result<Vec<(String, String)>> {
    let mut buf: Vec<u8> = Vec::new();
    clap_mangen::Man::new(cli::Args::command())
        .render(&mut buf)
        .context("failed to render the man page")?;
    Ok(vec![(
        "cm.1".to_string(),
        String::from_utf8(buf).context("man page is not UTF-8")?,
    )])
}

fn install_man(dir: Option<PathBuf>) -> Result<ExitCode> {
    let dir = dir.map_or_else(cm_core::man::default_man_dir, Ok)?;
    for path in cm_core::man::write_pages(&dir, &man_pages()?)? {
        println!("Installed {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Removes only files that still look like our generated pages.
fn uninstall_man(dir: Option<PathBuf>) -> Result<ExitCode> {
    let dir = dir.map_or_else(cm_core::man::default_man_dir, Ok)?;
    cm_core::man::remove_pages(&dir, &man_pages()?)?;
    Ok(ExitCode::SUCCESS)
}

/// The completion scripts, generated from the clap definition so they
/// can't drift from `--help`.
fn completions() -> Vec<(cm_core::completions::Shell, String)> {
    use clap_complete::shells::{Bash, Fish, Zsh};
    use cm_core::completions::Shell;
    let mut cmd = cli::Args::command();
    Shell::ALL
        .iter()
        .map(|&s| {
            let mut buf = Vec::new();
            match s {
                Shell::Bash => clap_complete::generate(Bash, &mut cmd, "cm", &mut buf),
                Shell::Zsh => clap_complete::generate(Zsh, &mut cmd, "cm", &mut buf),
                Shell::Fish => clap_complete::generate(Fish, &mut cmd, "cm", &mut buf),
            }
            (s, String::from_utf8(buf).expect("completion is not UTF-8"))
        })
        .collect()
}

fn install_completions(dir: Option<PathBuf>) -> Result<ExitCode> {
    let dir = dir.map_or_else(cm_core::completions::default_share_dir, Ok)?;
    for path in cm_core::completions::write_files(&dir, "cm", &completions())? {
        println!("Installed {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Removes only files that still look like generated scripts.
fn uninstall_completions(dir: Option<PathBuf>) -> Result<ExitCode> {
    let dir = dir.map_or_else(cm_core::completions::default_share_dir, Ok)?;
    cm_core::completions::remove_files(&dir, "cm", &completions())?;
    Ok(ExitCode::SUCCESS)
}
