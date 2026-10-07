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
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use backend::Target;
use clap::{CommandFactory, Parser};
use cli::{Action, Args, InstallOpts, InstallSource, ShellType};
use cm_core::container::{self, ArgvMode};
use cm_core::forward::PortMapping;
use cm_core::{catalog, naming, oci, table, wsl};
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
            cache,
            catalog,
        } => list(running_only, quiet, verbosity, online, cache, catalog),
        Action::PurgeCache => {
            let (count, bytes) = wsl::purge_cache();
            println!(
                "Removed {count} cached .wsl file(s), freed {} MiB",
                bytes >> 20
            );
            Ok(ExitCode::SUCCESS)
        }
        Action::SetDefault(m) => set_default(&m),
        Action::Terminate(m) => terminate(&m),
        Action::Unregister(m) => unregister(&m),
        Action::Install(opts) => install(&opts),
        Action::Export { name, file } => export(&name, &file),
        Action::Import {
            name,
            install_location,
            file,
            distro,
        } => import(&name, install_location.as_deref(), &file, distro),
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
    cache: bool,
    catalog_file: Option<PathBuf>,
) -> Result<ExitCode> {
    if online {
        return list_online(verbosity, catalog_file.as_deref());
    }
    if cache {
        return list_cache(catalog_file.as_deref());
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

/// `cm -l -o`: the installable catalog — baked in (or a `--catalog`
/// file), so a plain listing never touches `container`. `-v` adds each
/// entry's local state — `LOCAL` (a pulled image, a cached `.wsl`, or
/// `downloading` while a fetch is in flight) and `INSTANCES` (machines
/// and distros installed from the entry, running ones marked).
///
/// Every probe is best-effort and never blocks: `container` is
/// consulted only when already running — a listing shouldn't start
/// services — and the cache is read without flocking; an in-flight
/// download is detected by `try_lock` on its `.lock` sidecar, never
/// waited on.
fn list_online(verbosity: u8, catalog_file: Option<&Path>) -> Result<ExitCode> {
    let cat = catalog::load(catalog_file)?;
    let arch = oci::host_arch();
    if verbosity == 0 {
        print!("{}", catalog::render(&cat.entries, arch));
        return Ok(ExitCode::SUCCESS);
    }
    let probe = OnlineProbe::gather();
    let status: Vec<catalog::EntryStatus> =
        cat.entries.iter().map(|e| probe.status_of(e)).collect();
    print!(
        "{}",
        catalog::render_verbose(&cat.entries, arch, &status, verbosity)
    );
    Ok(ExitCode::SUCCESS)
}

/// Local state for `--list --online --verbose`. Every field degrades
/// to empty rather than erroring or blocking.
struct OnlineProbe {
    /// Verified `<sha256>.wsl` files in the download cache, by hash.
    cached: std::collections::HashSet<String>,
    /// Images `container` knows, or empty when services are down.
    images: Vec<container::ImageListEntry>,
    /// `(running, image reference)` per installed name — a distro
    /// shadows a same-named machine, like everywhere else in `cm`.
    instances: std::collections::BTreeMap<String, (bool, Option<String>)>,
}

/// A SHA-256 in cache-filename form: lowercase hex, `0x` stripped —
/// the label on an imported image keeps the catalog's raw spelling.
fn normalize_sha(s: &str) -> String {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
        .to_lowercase()
}

impl OnlineProbe {
    fn gather() -> Self {
        use std::collections::{BTreeMap, HashSet};
        let cached: HashSet<String> = wsl::cache_entries().into_iter().map(|e| e.sha256).collect();
        let mut images = Vec::new();
        let mut instances = BTreeMap::new();
        if container::system_running() {
            images = container::list_images().unwrap_or_default();
            // `machine list` has no image column; inspect reports the
            // reference each machine was created from.
            for m in container::list_machines().unwrap_or_default() {
                let image = container::inspect_machine(&m.id)
                    .ok()
                    .and_then(|d| d.image.map(|i| i.reference));
                instances.insert(m.id.clone(), (m.is_running(), image));
            }
            if backend::distros_enabled() {
                for c in distro::distros().unwrap_or_default() {
                    instances.insert(
                        c.id().to_string(),
                        (
                            c.is_running(),
                            c.configuration.image.as_ref().map(|i| i.reference.clone()),
                        ),
                    );
                }
            }
        }
        Self {
            cached,
            images,
            instances,
        }
    }

    /// The `rootfs-sha256` label on a local image, normalized — the
    /// link from a distro's image back to the `.wsl` download it was
    /// imported from.
    fn rootfs_sha(&self, image_ref: &str) -> Option<String> {
        use container_distro::spec::LABEL_ROOTFS_SHA256;
        let key = naming::label_key(LABEL_ROOTFS_SHA256);
        self.images
            .iter()
            .find(|i| container::same_image(&i.configuration.name, image_ref))
            .and_then(|i| i.labels().get(&key).map(|s| normalize_sha(s)))
    }

    fn status_of(&self, e: &catalog::Entry) -> catalog::EntryStatus {
        let mut st = catalog::EntryStatus::default();
        let Some(source) = e.source(oci::host_arch()) else {
            return st;
        };
        for (name, (running, image)) in &self.instances {
            let hit = image.as_deref().is_some_and(|r| match source {
                catalog::Source::Image(i) => container::same_image(r, i),
                catalog::Source::Wsl(d) => self
                    .rootfs_sha(r)
                    .is_some_and(|s| s == d.sha256_normalized()),
            });
            if hit {
                st.instances.push((name.clone(), *running));
            }
        }
        st.local = match source {
            catalog::Source::Image(i)
                if self
                    .images
                    .iter()
                    .any(|x| container::same_image(&x.configuration.name, i)) =>
            {
                catalog::Local::Yes
            }
            catalog::Source::Wsl(d) => {
                let sha = d.sha256_normalized();
                if self.cached.contains(&sha) {
                    catalog::Local::Yes
                } else if wsl::download_in_progress(&sha) {
                    catalog::Local::Downloading
                } else {
                    catalog::Local::No
                }
            }
            _ => catalog::Local::No,
        };
        st
    }
}

/// `cm --list --cache`: the `.wsl` download cache, which catalog entry
/// each file is for, and which image it was imported as. Cache files
/// are named by their verified SHA-256 — the same hash lands on the
/// imported image's `rootfs-sha256` label, which is how the two link.
fn list_cache(catalog_file: Option<&std::path::Path>) -> Result<ExitCode> {
    use container_distro::spec::LABEL_ROOTFS_SHA256;

    let cached = wsl::cache_entries();
    let cat = catalog::load(catalog_file).ok();
    let images = container::list_images().unwrap_or_default();
    let sha_key = naming::label_key(LABEL_ROOTFS_SHA256);

    println!(
        "Cached .wsl downloads ({})",
        wsl::cache_dir().map_or_else(|| "?".into(), |p| p.display().to_string())
    );
    if cached.is_empty() {
        println!("  (empty)");
        return Ok(ExitCode::SUCCESS);
    }
    let mut rows = vec![vec![
        "SHA256".into(),
        "SIZE".into(),
        "CATALOG ENTRY".into(),
        "IMPORTED AS".into(),
    ]];
    for e in &cached {
        // Prefer the catalog's own name for this SHA; fall back to the
        // name recorded in the lockfile at fetch time, marking it when
        // the catalog still knows the name but at a different SHA —
        // i.e. a superseded build.
        let entry_name = cat
            .as_ref()
            .and_then(|c| {
                c.entries
                    .iter()
                    .find(|en| {
                        [&en.amd64_url, &en.arm64_url]
                            .into_iter()
                            .flatten()
                            .any(|d| d.sha256_normalized() == e.sha256)
                    })
                    .map(|en| en.name.clone())
            })
            .or_else(|| {
                let meta = e.meta.as_ref()?;
                let superseded = cat.as_ref().is_some_and(|c| {
                    c.entries
                        .iter()
                        .any(|en| en.name.eq_ignore_ascii_case(&meta.name))
                });
                Some(if superseded {
                    format!("{} (superseded)", meta.name)
                } else {
                    meta.name.clone()
                })
            })
            .unwrap_or_default();
        let image_ref = images
            .iter()
            .filter(|i| i.labels().get(&sha_key) == Some(&e.sha256))
            .map(|i| i.configuration.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        rows.push(vec![
            format!("{}…", &e.sha256[..12.min(e.sha256.len())]),
            format!("{} MiB", e.size >> 20),
            entry_name,
            image_ref,
        ]);
    }
    for line in table::columns(&rows, 3) {
        println!("{line}");
    }
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

/// What `--install` resolved to on this host.
enum Resolved {
    /// An OCI image to create a machine or distro from.
    Image(String),
    /// A catalog `.wsl` download (URL + SHA-256); the entry's NAME is
    /// the last-resort distro name after the package's own manifest.
    Wsl {
        url: String,
        sha256: String,
        name: String,
    },
}

/// The positional `--install` argument is a catalog name when it
/// matches one (case-insensitive), otherwise an image reference —
/// catalog names contain no `/`/`:`/`@`, so a qualified ref always
/// bypasses the catalog, as does `--from-image` explicitly. A catalog
/// entry resolves to its `Image` or to the `.wsl` download for the
/// host arch; an entry with neither is un-installable here (like WSL
/// on ARM rejecting amd64-only distributions).
fn resolve(cat: &catalog::Catalog, source: &InstallSource) -> Result<Resolved> {
    let entry = match source {
        InstallSource::Default => cat.default_entry().context("the distro catalog is empty")?,
        InstallSource::CatalogOrImage(a) => match catalog::lookup(&cat.entries, a) {
            Some(e) => e,
            None => return Ok(Resolved::Image(a.clone())),
        },
        InstallSource::Image(r) => return Ok(Resolved::Image(r.clone())),
        InstallSource::File(_) => bail!("internal error: file source resolved as image"),
    };
    match entry.source(oci::host_arch()) {
        Some(catalog::Source::Image(i)) => Ok(Resolved::Image(i.to_string())),
        Some(catalog::Source::Wsl(d)) => Ok(Resolved::Wsl {
            url: d.url.clone(),
            sha256: d.sha256.clone(),
            name: entry.name.clone(),
        }),
        None => bail!(
            "{:?} is not available for {} (no image or .wsl download for that architecture)",
            entry.name,
            oci::host_arch()
        ),
    }
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
        return install_from_file(opts, &create, file, None, distro::ImportSource::Local);
    }
    let cat = catalog::load(opts.catalog.as_deref())?;
    let image = match resolve(&cat, &opts.source)? {
        Resolved::Image(i) => i,
        Resolved::Wsl { url, sha256, name } => {
            // `fetched` holds a shared lock on the cache entry — purge
            // and prune wait for the import below instead of deleting
            // the file mid-flight.
            let fetched = wsl::fetch(&url, &sha256, &name)?;
            return install_from_file(
                opts,
                &create,
                fetched.path(),
                Some(&name),
                distro::ImportSource::Download {
                    url: &url,
                    sha256: &sha256,
                },
            );
        }
    };
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

/// `--install --from-file`, or a catalog `.wsl` entry: import a rootfs
/// tar or `.wsl` package as a distro — machines can't boot a bare
/// rootfs, so this is always the distro path regardless of `--distro`.
/// `name_hint` is a catalog entry's NAME when the source was a catalog
/// download; the naming order is `--name`, the `.wsl` manifest's
/// `oobe.defaultName`, `name_hint`, then the file stem.
fn install_from_file(
    opts: &InstallOpts,
    create: &CreateOptions,
    file: &std::path::Path,
    name_hint: Option<&str>,
    source: distro::ImportSource<'_>,
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
                .or_else(|| name_hint.map(str::to_string))
                .unwrap_or_else(|| file_default_name(file)),
        ),
    };
    container::validate_name(&name)?;
    let name = distro::import(&name, file, create, source)?;
    if opts.no_launch {
        return Ok(ExitCode::SUCCESS);
    }
    let mut cmd = distro::run_command(RunOpts {
        name: Some(name),
        ..RunOpts::default()
    })?;
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"))
}

/// WSL `--export`: distros go through `container export` directly;
/// machines through the scratch-container workaround (`container
/// export` can't snapshot a machine's plugin-state rootfs — see
/// doc/gaps/export-import.md). FILE `-` writes the tar to stdout.
fn export(name: &str, file: &str) -> Result<ExitCode> {
    let output = (file != "-").then_some(Path::new(file));
    match named_target(name)? {
        Target::Distro(d) => distro::export(&d, output)?,
        Target::Machine(_) => distro::export_machine(name, output)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// WSL `--import`: wrap the rootfs tar as an OCI image, load it, and
/// create a machine — or, with `--distro`, import it as a distro.
fn import(
    name: &str,
    install_location: Option<&str>,
    file: &str,
    as_distro: bool,
) -> Result<ExitCode> {
    container::validate_name(name)?;
    if let Some(loc) = install_location.filter(|l| !l.is_empty() && *l != "-") {
        eprintln!("cm: ignoring install location `{loc}` — storage is managed by `container`");
    }
    let file = Path::new(file);
    if as_distro {
        if !backend::distros_enabled() {
            bail!("--distro needs distro support (unset CM_BACKEND=machine)");
        }
        container::ensure_started()?;
        // The new distro would shadow a same-named machine for `cm` —
        // warn, like `-d` resolution does.
        if container::list_machines()?.iter().any(|m| m.id == name) {
            eprintln!("cm: `{name}` is also a machine; `cm` resolves the name to the distro");
        }
        distro::import(
            name,
            file,
            &CreateOptions::default(),
            distro::ImportSource::Local,
        )?;
    } else {
        distro::import_machine(name, file)?;
    }
    Ok(ExitCode::SUCCESS)
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
