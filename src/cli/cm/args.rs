/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! WSL-compatible argument parsing.
//!
//! Mirrors `wsl.exe` argument semantics rather than clap's defaults: `-e` and
//! `--` both consume the remainder of the command line verbatim, and a bare
//! positional argument is treated as a command to execute.

use std::path::PathBuf;

pub use crate::distro::spec::HomeMount;
use crate::distro::spec::{Arch, ConfList, MountSpec, Preset, PublishSpec};
use crate::util::forward::PortMapping;
use anyhow::{Result, bail};
use clap::{ArgAction, Parser, ValueEnum};

/// What `--install` installs from.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallSource {
    /// Bare `--install`: the catalog's default entry (Ubuntu, like WSL).
    Default,
    /// Positional argument: a catalog name (case-insensitive) or an
    /// image reference.
    CatalogOrImage(String),
    /// `--from-image`: an image reference, catalog bypassed entirely.
    Image(String),
    /// `--from-file`: a local rootfs tar / `.wsl` package
    /// (`wsl --install --from-file`). Implies `--distro` — machines
    /// can't boot a bare rootfs.
    File(PathBuf),
}

/// Options for `--install`.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallOpts {
    pub source: InstallSource,
    /// `--catalog` override, for resolving `CatalogOrImage` names.
    pub catalog: Option<PathBuf>,
    pub name: Option<String>,
    pub no_launch: bool,
    pub cpus: Option<u32>,
    pub memory: Option<String>,
    /// `--arch`: the guest arch — amd64 selects x86-64 sources and runs
    /// under Rosetta.
    pub arch: Option<Arch>,
    pub home_mount: Option<HomeMount>,
    /// Create a `container distro` — the default; `wants_machine()`
    /// reports the `--machine` opt-out. Distro-only options
    /// (`--restricted`, `--from-file`, `--share`, `--publish`) are
    /// rejected alongside `--machine` at parse time.
    pub distro: bool,
    /// Create a `container machine` instead of a distro.
    pub machine: bool,
    /// The defaults tier (`--preset`); mutually exclusive with
    /// `restricted`/`trusted` at parse.
    pub preset: Option<Preset>,
    /// Pin SSH-agent forwarding on (`--ssh`) — machines forward it
    /// anyway, so this only documents the opt-in there.
    pub ssh: bool,
    /// Pin SSH-agent forwarding off (`--no-ssh`) — a machine can't
    /// honor it.
    pub no_ssh: bool,
    /// Restricted defaults (no mounts, network, agent, sudo). Distro-only
    /// — machines can't be restricted.
    pub restricted: bool,
    /// Trusted defaults (rw home, rw automount, SSH agent). With
    /// `--machine`, opts an untrusted-tier install into the agent-
    /// forwarding tier machines always run at.
    pub trusted: bool,
    /// Distro-only: honor the guest's /etc/wsl.conf (`FEATURE,...`,
    /// `all`/`none`; bare flag = `all`).
    pub use_wsl_conf: Option<ConfList>,
    /// Distro-only: honor nothing from /etc/wsl.conf.
    pub no_wsl_conf: bool,
    /// Deny parts of the guest's /etc/wsl.conf — a no-op on machines,
    /// which never read it.
    pub ignore_wsl_conf: Option<ConfList>,
    /// Allow the new machine or distro to share its name with one in
    /// the other backend (`cm` resolves a shared name to the distro).
    pub force: bool,
    pub shares: Vec<MountSpec>,
    pub publish: Vec<PublishSpec>,
    /// Pin terminal-profile generation on (`--terminal-profile`) —
    /// distro-only.
    pub terminal_profile: bool,
    /// Pin it off (`--no-terminal-profile`); trivially satisfied by a
    /// machine, which never gets profiles.
    pub no_terminal_profile: bool,
    /// `--terminal`: open the new distro in Terminal.app instead of
    /// exec'ing a shell into the current console — also acts as an
    /// explicit `--terminal-profile` opt-in.
    pub terminal: bool,
}

impl InstallOpts {
    /// `--machine` opts out of the distro default.
    pub fn wants_machine(&self) -> bool {
        self.machine
    }
}

/// Which backend a name or a creator resolves to — `--distro`/`--machine`
/// are selectors, not creators: they apply anywhere `cm` resolves a
/// distro-or-machine name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Backend {
    /// Distro-first: a distro shadows a same-named machine, and the
    /// default distro beats the default machine.
    #[default]
    Auto,
    /// `--distro`: require a distro — a name that isn't one is an error.
    Distro,
    /// `--machine`: require a machine — even a same-named distro loses.
    Machine,
}

/// Which shell flavor to start for an interactive session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShellType {
    /// Non-login interactive shell.
    Standard,
    /// Login shell (the default).
    Login,
    /// No shell — only valid together with a command.
    None,
}

#[derive(Debug, Parser)]
#[command(
    name = "cm",
    version,
    disable_version_flag = true,
    about = "WSL-compatible wrapper for Apple container machines",
    long_about = "Runs commands and shells inside Apple `container` machines using WSL-style arguments.\n\
                  With no arguments, opens a login shell in the default machine.",
    override_usage = "cm [OPTIONS] [-- <COMMAND LINE>]",
    after_help = "Examples:\n  \
                  cm                          Open a shell in the default machine\n  \
                  cm -d alpine                Open a shell in the 'alpine' machine\n  \
                  cm -e uname -a              Run a command without a shell\n  \
                  cm -- ls -la                Pass a command line through verbatim\n  \
                  cm -l -v                    List machines\n  \
                  cm -t alpine                Stop the 'alpine' machine"
)]
pub struct Args {
    /// Machine to use (uses the default if not specified)
    #[arg(short = 'd', long = "distribution", value_name = "MACHINE")]
    pub distribution: Option<String>,

    /// Run as the specified user
    #[arg(short = 'u', long = "user", value_name = "USER")]
    pub user: Option<String>,

    /// Set the working directory inside the machine
    #[arg(long = "cd", value_name = "DIR")]
    pub cd: Option<String>,

    /// Shell type to start for an interactive session
    #[arg(long = "shell-type", value_enum, value_name = "TYPE")]
    pub shell_type: Option<ShellType>,

    /// Execute the command line without a shell; consumes the rest of the line
    #[arg(short = 'e', long = "exec")]
    pub exec: bool,

    /// Set an environment variable (key=value, or key to inherit from host)
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    /// List container machines
    #[arg(short = 'l', long = "list")]
    pub list: bool,

    /// List all machines (with --list)
    #[arg(long = "all", requires = "list")]
    pub all: bool,

    /// List only running machines (with --list)
    #[arg(long = "running", requires = "list")]
    pub running: bool,

    /// Show only machine names (with --list)
    #[arg(
        short = 'q',
        long = "quiet",
        requires = "list",
        conflicts_with = "verbose"
    )]
    pub quiet: bool,

    /// Show a NAME/STATE/VERSION table (with --list); repeat for more
    /// columns. With --online, adds per-entry local status instead
    #[arg(
        short = 'v',
        long = "verbose",
        action = ArgAction::Count,
        requires = "list",
        conflicts_with = "quiet"
    )]
    pub verbose: u8,

    /// List the installable-distro catalog instead of installed machines
    /// and distros (with --list); --verbose adds LOCAL/INSTANCES columns
    #[arg(
        short = 'o',
        long = "online",
        requires = "list",
        conflicts_with_all = ["all", "running", "quiet", "cache"]
    )]
    pub online: bool,

    /// List the `.wsl` download cache and the images imported from it
    /// instead of installed machines and distros (with --list)
    #[arg(
        long = "cache",
        requires = "list",
        conflicts_with_all = ["all", "running", "quiet", "verbose"]
    )]
    pub cache: bool,

    /// Delete cached `.wsl` downloads (verified against their SHA-256
    /// on reuse anyway; imported images are unaffected)
    #[arg(long = "purge-cache")]
    pub purge_cache: bool,

    /// Set the default machine
    #[arg(short = 's', long = "set-default", value_name = "MACHINE")]
    pub set_default: Option<String>,

    /// Stop a running machine
    #[arg(short = 't', long = "terminate", value_name = "MACHINE")]
    pub terminate: Option<String>,

    /// Stop all running machines
    #[arg(long = "shutdown")]
    pub shutdown: bool,

    /// With --shutdown, also stop `container` services (`container system stop`)
    #[arg(long = "system", requires = "shutdown")]
    pub system: bool,

    /// Show container system status
    #[arg(long = "status")]
    pub status: bool,

    /// Update container-distro itself: download a release .pkg (the
    /// latest, or VERSION like `0.5.0`) and hand it to Installer.app.
    /// Downgrades and unsigned packages ask first; --force skips that
    #[arg(long = "update", num_args = 0..=1, value_name = "VERSION")]
    pub update: Option<Option<String>>,

    /// Report what --update would install without installing (with
    /// --update)
    #[arg(long, requires = "update")]
    pub dry_run: bool,

    /// Enable dev-mode overrides for this run — a non-default
    /// CONTAINER_CLI binary and the CONTAINER_DISTRO_RELEASES_URL
    /// fixture server (testing only — DEV.md)
    #[arg(long)]
    pub dev: bool,

    /// Delete a machine and its persistent storage
    #[arg(long = "unregister", value_name = "MACHINE")]
    pub unregister: Option<String>,

    /// Make NAME an instance of the selected backend: a machine becomes
    /// a distro (or vice versa with --machine), a pre-journal distro is
    /// adopted — its live config becomes the journal
    #[arg(long = "migrate", value_name = "NAME")]
    pub migrate: Option<String>,

    /// Create a distro from a catalog name or container image and boot
    /// it (`cm -l -o` lists the catalog); bare installs the default
    /// distribution. With --machine, creates a `container machine`
    #[arg(long = "install", value_name = "DISTRO|IMAGE", num_args = 0..=1)]
    pub install: Option<Option<String>>,

    /// Install from an image reference, bypassing catalog name
    /// resolution (with --install)
    #[arg(long = "from-image", requires = "install", value_name = "IMAGE")]
    pub from_image: Option<String>,

    /// Install from a local rootfs tar or `.wsl` package instead of an
    /// image (with --install; implies --distro)
    #[arg(long = "from-file", requires = "install", value_name = "FILE")]
    pub from_file: Option<PathBuf>,

    /// Use FILE as the installable-distro catalog instead of the
    /// built-in list (with `-l -o` or `--install`)
    #[arg(long, value_name = "FILE")]
    pub catalog: Option<PathBuf>,

    /// Name for the machine created by --install
    #[arg(long = "name", requires = "install", value_name = "NAME")]
    pub name: Option<String>,

    /// Do not open a shell after --install finishes
    #[arg(long = "no-launch", requires = "install")]
    pub no_launch: bool,

    /// Number of virtual CPUs for the machine created by --install
    #[arg(long = "cpus", requires = "install", value_name = "N")]
    pub cpus: Option<u32>,

    /// Memory for the machine created by --install (e.g. 4G)
    #[arg(long = "memory", requires = "install", value_name = "SIZE")]
    pub memory: Option<String>,

    /// CPU architecture for what --install or --import creates —
    /// `amd64` selects x86-64 images and .wsl catalog downloads, run
    /// under Rosetta translation on Apple silicon. With --list it
    /// filters the listing to entries/instances of that arch
    #[arg(long = "arch", value_enum, value_name = "ARCH")]
    pub arch: Option<Arch>,

    /// Home directory mount for what --install creates (default: none —
    /// rw only for distros installed from a catalog entry, which implies
    /// trust in that catalog)
    #[arg(
        long = "home-mount",
        requires = "install",
        value_enum,
        value_name = "MODE"
    )]
    pub home_mount: Option<HomeMount>,

    /// Select a distro (`container distro`): the target for -d and the
    /// default, and the kind created by --install/--import — though
    /// those default to distros already
    #[arg(long = "distro", conflicts_with = "machine")]
    pub distro: bool,

    /// Select a machine (`container machine`): the target for -d and the
    /// default, and the kind created by --install/--import
    #[arg(long = "machine")]
    pub machine: bool,

    /// The defaults tier a distro created by --install runs at —
    /// `default`, `restricted`, or `trusted` — recorded so
    /// `container distro set --preset` can re-derive it later
    #[arg(long, value_enum, requires = "install", conflicts_with_all = ["restricted", "trusted"])]
    pub preset: Option<Preset>,

    /// Restricted defaults for a distro created by --install: no home or
    /// extra mounts, no network, no SSH agent, no sudo/doas. A
    /// convenience preset, not a sandbox. (`--preset restricted`)
    #[arg(long = "restricted", visible_alias = "untrusted", requires = "install")]
    pub restricted: bool,

    /// Trusted defaults for what --install creates: rw home share,
    /// /Volumes automounted rw (distros), SSH agent forwarded — the
    /// mirror of --restricted. Also opts an untrusted-tier install into
    /// --machine, which always forwards the agent. (`--preset trusted`)
    #[arg(long = "trusted", requires = "install", conflicts_with = "restricted")]
    pub trusted: bool,

    /// Forward your SSH agent into what --install creates — an explicit
    /// opt-in that pins agent forwarding on a distro regardless of its
    /// home-mount mode (machines forward it unconditionally)
    #[arg(long, requires = "install", conflicts_with = "no_ssh")]
    pub ssh: bool,

    /// Keep your SSH agent out of what --install creates — an explicit
    /// pin a machine can't honor (`--machine` always forwards it)
    #[arg(long = "no-ssh", requires = "install", conflicts_with = "machine")]
    pub no_ssh: bool,

    /// Honor the guest's /etc/wsl.conf in a distro created by --install
    /// — optionally a comma list of hostname, fstab, init, command,
    /// automount, or `all`/`none` (bare flag = `all`; default: `all`
    /// for --trusted and catalog installs, `none` otherwise; implies
    /// --distro)
    #[arg(
        long = "use-wsl-conf",
        requires = "install",
        conflicts_with = "machine",
        value_name = "FEATURE,...|all|none",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "all"
    )]
    pub use_wsl_conf: Option<ConfList>,

    /// Honor nothing from a created distro's /etc/wsl.conf
    /// (`--use-wsl-conf=none` by another name)
    #[arg(
        long = "no-wsl-conf",
        requires = "install",
        conflicts_with_all = ["use_wsl_conf", "ignore_wsl_conf"]
    )]
    pub no_wsl_conf: bool,

    /// Deny parts of a created distro's /etc/wsl.conf
    /// (harmless with --machine: machines never read the file)
    #[arg(
        long = "ignore-wsl-conf",
        requires = "install",
        value_name = "FEATURE,...|all|none"
    )]
    pub ignore_wsl_conf: Option<ConfList>,

    /// Generate macOS terminal profiles (Terminal.app + iTerm2) for the
    /// distro --install creates — the default, unless the package opts
    /// out. A machine can't honor the opt-in
    #[arg(
        long = "terminal-profile",
        requires = "install",
        conflicts_with_all = ["machine", "no_terminal_profile"]
    )]
    pub terminal_profile: bool,

    /// Don't generate terminal profiles for what --install creates
    /// (a no-op with --machine: machines never get profiles)
    #[arg(long = "no-terminal-profile", requires = "install")]
    pub no_terminal_profile: bool,

    /// Open the distro in a Terminal.app window via its generated
    /// `.terminal` profile — `cm -d NAME --terminal`, and with
    /// --install/--import it replaces the console launch. Acts as an
    /// explicit `--terminal-profile` opt-in (journaled)
    #[arg(
        long,
        conflicts_with_all = ["machine", "exec", "no_launch", "command", "no_terminal_profile"]
    )]
    pub terminal: bool,

    /// Share a host directory into a distro created by --install:
    /// SRC:DST[:opt,...] — ro, rw, and guest mount flags like
    /// nosuid,nodev,noexec (repeatable; implies --distro)
    #[arg(long = "share", requires = "install", value_name = "SRC:DST[:opt,...]")]
    pub shares: Vec<MountSpec>,

    /// Publish a port from a distro created by --install:
    /// [HOST_IP:]HOST_PORT[:GUEST_PORT] (repeatable; implies --distro)
    #[arg(long = "publish", requires = "install", value_name = "SPEC")]
    pub publish: Vec<PublishSpec>,

    /// Forward 127.0.0.1:HOST_PORT to the machine's GUEST_PORT (repeatable;
    /// runs in the foreground until interrupted). Experimental — for distros
    /// prefer --publish at install or `container distro set --publish`
    #[arg(long = "forward", value_name = "HOST_PORT[:GUEST_PORT]")]
    pub forward: Vec<PortMapping>,

    /// Export a machine's or distro's root filesystem as a tar (FILE `-`
    /// writes stdout)
    #[arg(long = "export", num_args = 2, value_names = ["NAME", "FILE"])]
    pub export: Option<Vec<String>>,

    /// Import a rootfs tar or tar.gz as a distro (FILE `-` reads
    /// stdin; with --machine, wraps it as an OCI image and creates a
    /// `container machine` instead). INSTALL_LOCATION is accepted for
    /// WSL compatibility and ignored
    #[arg(
        long = "import",
        num_args = 2..=3,
        value_names = ["NAME", "INSTALL_LOCATION", "FILE"]
    )]
    pub import: Option<Vec<String>>,

    /// Rename a machine or distro — its filesystem and settings carry
    /// to the new name; a running target is restarted under it
    /// (`--machine`/`--distro` selects which when the name is
    /// ambiguous)
    #[arg(long = "rename", num_args = 2, value_names = ["OLD", "NEW"])]
    pub rename: Option<Vec<String>>,

    /// Allow --install, --import, or --rename to create a machine or
    /// distro whose name is already taken in the other backend — `cm`
    /// resolves a shared name to the distro, shadowing the machine
    #[arg(long = "force")]
    pub force: bool,

    /// Install the `cm(1)` man page; writes to DIR, or the `share/man/man1`
    /// next to the binary's `bin` directory (e.g. $CARGO_HOME/share/man/man1
    /// after `cargo install`)
    #[arg(long, value_name = "DIR", num_args = 0..=1, require_equals = true)]
    pub install_man: Option<Option<PathBuf>>,

    /// Remove a `cm(1)` man page installed by --install-man (only files that
    /// still look like our generated pages are removed)
    #[arg(long, value_name = "DIR", num_args = 0..=1, require_equals = true)]
    pub uninstall_man: Option<Option<PathBuf>>,

    /// Install the bash/zsh/fish completions for cm; writes under DIR
    /// (the share root), or the `share` next to the binary's `bin`
    /// directory (e.g. $CARGO_HOME/share after `cargo install`)
    #[arg(long, value_name = "DIR", num_args = 0..=1, require_equals = true)]
    pub install_completions: Option<Option<PathBuf>>,

    /// Remove completions installed by --install-completions (only files
    /// that still look like generated scripts are removed)
    #[arg(long, value_name = "DIR", num_args = 0..=1, require_equals = true)]
    pub uninstall_completions: Option<Option<PathBuf>>,

    /// Create an alias symlink to cm — a NAME lands next to the binary
    /// (e.g. `wsl`), a PATH like ~/.local/bin/wsl is used as given — plus
    /// a NAME(1) man-page symlink
    #[arg(long, value_name = "NAME|PATH")]
    pub install_alias: Option<String>,

    /// Remove an alias created by --install-alias (only a symlink that
    /// actually resolves to this cm, and its man-page symlink, are removed)
    #[arg(long, value_name = "NAME|PATH")]
    pub uninstall_alias: Option<String>,

    /// Print version information
    #[arg(long = "version")]
    pub version: bool,

    /// Command line to execute in the machine (everything after -- is verbatim)
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND LINE"
    )]
    pub command: Vec<String>,
}

/// A validated top-level action.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Open a shell or run a command in a machine.
    Run,
    /// WSL-shaped machine list; `online` lists the installable catalog
    /// instead (`wsl --list --online`), `cache` lists the `.wsl`
    /// download cache — both honoring `--catalog`. `online` + `verbose`
    /// adds each entry's local state (cached/pulled, installed
    /// instances).
    List {
        running_only: bool,
        quiet: bool,
        verbosity: u8,
        online: bool,
        cache: bool,
        catalog: Option<PathBuf>,
        arch: Option<Arch>,
    },
    /// Empty the `.wsl` download cache.
    PurgeCache,
    /// Stop every running machine; with `system`, also stop services.
    Shutdown { system: bool },
    /// `container system status`.
    Status,
    /// Self-update to a release .pkg via Installer.app.
    Update {
        version: Option<String>,
        force: bool,
        dry_run: bool,
    },
    /// `container machine set-default`.
    SetDefault(String),
    /// `container machine stop`.
    Terminate(String),
    /// `container machine rm`.
    Unregister(String),
    /// `container distro migrate` — converge NAME to the selected
    /// backend (machine↔distro, or adopt a pre-journal distro).
    Migrate(String),
    /// `container machine create`, then optionally open a shell.
    Install(InstallOpts),
    /// `container export` for a distro; the scratch-container workaround
    /// for a machine. `file` is `-` for stdout.
    Export { name: String, file: String },
    /// `distro import` — or `machine create` from an OCI-wrapped rootfs
    /// tar with `--machine`.
    Import {
        name: String,
        install_location: Option<String>,
        file: String,
        backend: Backend,
        arch: Option<Arch>,
        force: bool,
        /// `--terminal`: open the new distro in Terminal.app after
        /// registering instead of leaving it stopped.
        terminal: bool,
    },
    /// Rename a distro or machine to a fresh name — the machine form
    /// recreates it under the new name, carrying its filesystem.
    Rename {
        old: String,
        new: String,
        backend: Backend,
        force: bool,
    },
    /// Forward localhost ports to a machine.
    Forward {
        machine: Option<String>,
        mappings: Vec<PortMapping>,
    },
    /// Print cm and container versions.
    Version,
    /// Install the generated `cm(1)` man page.
    InstallMan(Option<PathBuf>),
    /// Remove a previously installed `cm(1)` man page.
    UninstallMan(Option<PathBuf>),
    /// Install the generated shell completions.
    InstallCompletions(Option<PathBuf>),
    /// Remove previously installed shell completions.
    UninstallCompletions(Option<PathBuf>),
    /// Symlink `NAME`/`PATH` to cm + a `<name>(1)` man page link.
    InstallAlias(String),
    /// Remove an alias created by `--install-alias`.
    UninstallAlias(String),
}

impl Args {
    /// Resolve the parsed arguments into a single validated action.
    pub fn action(&self) -> Result<Action> {
        // The catalog only feeds `--install` name resolution and
        // `--list --online`; anywhere else it's silently ignored, so
        // reject it instead.
        if self.catalog.is_some()
            && !(self.install.is_some() || (self.list && (self.online || self.cache)))
        {
            bail!("--catalog only applies to --install and --list --online/--cache");
        }
        // `--force` only has meaning where a name is created or an
        // update is confirmed.
        if self.force
            && !(self.install.is_some()
                || self.import.is_some()
                || self.rename.is_some()
                || self.update.is_some())
        {
            bail!("--force only applies to --install, --import, --rename, and --update");
        }
        // `--arch` selects what an install or import materializes, and
        // which arch `--list` shows/filters for; anywhere else it
        // silently does nothing, so reject it.
        if self.arch.is_some() && self.install.is_none() && self.import.is_none() && !self.list {
            bail!("--arch only applies to --install, --import, and --list");
        }
        if self.arch.is_some() && self.cache {
            bail!("--arch does not apply to --list --cache");
        }
        // `--machine` can't satisfy distro-only install options
        // (`--use-wsl-conf` already conflicts at parse; `--ignore`
        // asks for nothing a machine doesn't already do).
        if self.machine
            && (self.restricted
                || self.preset.is_some()
                || self.from_file.is_some()
                || !self.shares.is_empty()
                || !self.publish.is_empty())
        {
            bail!(
                "--preset, --restricted, --from-file, --share, and --publish need a distro (drop --machine)"
            );
        }
        let action = if self.version {
            Action::Version
        } else if let Some(dir) = &self.install_man {
            Action::InstallMan(dir.clone())
        } else if let Some(dir) = &self.uninstall_man {
            Action::UninstallMan(dir.clone())
        } else if let Some(dir) = &self.install_completions {
            Action::InstallCompletions(dir.clone())
        } else if let Some(dir) = &self.uninstall_completions {
            Action::UninstallCompletions(dir.clone())
        } else if let Some(alias) = &self.install_alias {
            Action::InstallAlias(alias.clone())
        } else if let Some(alias) = &self.uninstall_alias {
            Action::UninstallAlias(alias.clone())
        } else if self.status {
            Action::Status
        } else if let Some(v) = &self.update {
            Action::Update {
                version: v.clone(),
                force: self.force,
                dry_run: self.dry_run,
            }
        } else if let Some(m) = &self.set_default {
            Action::SetDefault(m.clone())
        } else if let Some(m) = &self.terminate {
            Action::Terminate(m.clone())
        } else if let Some(m) = &self.unregister {
            Action::Unregister(m.clone())
        } else if let Some(m) = &self.migrate {
            Action::Migrate(m.clone())
        } else if let Some(i) = &self.install {
            let sources = self.from_file.is_some() as u8
                + self.from_image.is_some() as u8
                + i.is_some() as u8;
            if sources > 1 {
                bail!("--install takes at most one of an image, --from-image, or --from-file");
            }
            let source = if let Some(f) = &self.from_file {
                InstallSource::File(f.clone())
            } else if let Some(r) = &self.from_image {
                InstallSource::Image(r.clone())
            } else if let Some(a) = i {
                InstallSource::CatalogOrImage(a.clone())
            } else {
                InstallSource::Default
            };
            Action::Install(InstallOpts {
                source,
                catalog: self.catalog.clone(),
                name: self.name.clone(),
                no_launch: self.no_launch,
                cpus: self.cpus,
                memory: self.memory.clone(),
                arch: self.arch,
                home_mount: self.home_mount,
                distro: self.distro,
                machine: self.machine,
                preset: self.preset,
                ssh: self.ssh,
                no_ssh: self.no_ssh,
                restricted: self.restricted,
                trusted: self.trusted,
                use_wsl_conf: self.use_wsl_conf.clone(),
                no_wsl_conf: self.no_wsl_conf,
                ignore_wsl_conf: self.ignore_wsl_conf.clone(),
                force: self.force,
                shares: self.shares.clone(),
                publish: self.publish.clone(),
                terminal_profile: self.terminal_profile,
                no_terminal_profile: self.no_terminal_profile,
                terminal: self.terminal,
            })
        } else if let Some(e) = &self.export {
            Action::Export {
                name: e[0].clone(),
                file: e[1].clone(),
            }
        } else if let Some(i) = &self.import {
            // WSL's three values are NAME INSTALL_LOCATION FILE; with
            // two, the second is FILE.
            Action::Import {
                name: i[0].clone(),
                install_location: (i.len() == 3).then(|| i[1].clone()),
                file: i.last().unwrap().clone(),
                backend: self.backend(),
                arch: self.arch,
                force: self.force,
                terminal: self.terminal,
            }
        } else if let Some(r) = &self.rename {
            Action::Rename {
                old: r[0].clone(),
                new: r[1].clone(),
                backend: self.backend(),
                force: self.force,
            }
        } else if !self.forward.is_empty() {
            let action = Action::Forward {
                machine: self.distribution.clone(),
                mappings: self.forward.clone(),
            };
            self.check_no_run_args_except_distribution()?;
            self.check_selector(&action)?;
            return Ok(action);
        } else if self.shutdown {
            Action::Shutdown {
                system: self.system,
            }
        } else if self.purge_cache {
            Action::PurgeCache
        } else if self.list {
            Action::List {
                running_only: self.running,
                quiet: self.quiet,
                verbosity: self.verbose,
                online: self.online,
                cache: self.cache,
                catalog: self.catalog.clone(),
                arch: self.arch,
            }
        } else {
            return self.validate_run();
        };
        // `--terminal` opens a distro's `.terminal` profile — it only
        // has meaning where a distro is opened or created.
        if self.terminal
            && !matches!(
                action,
                Action::Run | Action::Install(_) | Action::Import { .. }
            )
        {
            bail!("--terminal only applies to opening a distro — plain, --install, or --import");
        }
        self.check_no_run_args()?;
        self.check_selector(&action)?;
        Ok(action)
    }

    /// The `--distro`/`--machine` selector the arguments ask for.
    pub fn backend(&self) -> Backend {
        if self.machine {
            Backend::Machine
        } else if self.distro {
            Backend::Distro
        } else {
            Backend::Auto
        }
    }

    /// `--distro`/`--machine` select a backend — meaningful only where
    /// a distro-or-machine name is resolved or a kind is created.
    fn check_selector(&self, action: &Action) -> Result<()> {
        if self.backend() != Backend::Auto
            && !matches!(
                action,
                Action::Run
                    | Action::Forward { .. }
                    | Action::Export { .. }
                    | Action::Import { .. }
                    | Action::Rename { .. }
                    | Action::Install(_)
                    | Action::SetDefault(_)
                    | Action::Terminate(_)
                    | Action::Unregister(_)
                    | Action::Migrate(_)
            )
        {
            bail!("--distro/--machine only apply to actions that name a machine or distro");
        }
        Ok(())
    }

    fn check_no_run_args(&self) -> Result<()> {
        if self.distribution.is_some() {
            bail!("cannot combine --distribution with a management action");
        }
        self.check_no_run_args_except_distribution()
    }

    fn check_no_run_args_except_distribution(&self) -> Result<()> {
        let mut bad = Vec::new();
        if self.user.is_some() {
            bad.push("--user");
        }
        if self.cd.is_some() {
            bad.push("--cd");
        }
        if self.shell_type.is_some() {
            bad.push("--shell-type");
        }
        if self.exec {
            bad.push("--exec");
        }
        if !self.env.is_empty() {
            bad.push("--env");
        }
        if !self.command.is_empty() {
            bad.push("a command line");
        }
        if !bad.is_empty() {
            bail!("cannot combine {} with a management action", bad.join(", "));
        }
        Ok(())
    }

    fn validate_run(&self) -> Result<Action> {
        // The launcher runs `cm -d NAME` — session-shaping flags the
        // profile can't carry are rejected rather than ignored
        // (`--exec`/`--no-launch`/a command line conflict at parse).
        if self.terminal
            && (self.user.is_some()
                || self.cd.is_some()
                || self.shell_type.is_some()
                || !self.env.is_empty())
        {
            bail!(
                "--terminal launches the distro's profile; --user, --cd, --shell-type, and --env don't apply"
            );
        }
        if self.command.is_empty() {
            if self.exec {
                bail!("--exec requires a command line");
            }
            if self.shell_type == Some(ShellType::None) {
                bail!("--shell-type none requires a command line");
            }
        }
        Ok(Action::Run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(argv)
    }

    #[test]
    fn no_args_is_run() {
        let args = parse(&["cm"]).unwrap();
        assert_eq!(args.action().unwrap(), Action::Run);
    }

    #[test]
    fn distribution_run() {
        let args = parse(&["cm", "-d", "alpine"]).unwrap();
        assert_eq!(args.distribution.as_deref(), Some("alpine"));
        assert_eq!(args.action().unwrap(), Action::Run);
    }

    #[test]
    fn exec_consumes_rest() {
        let args = parse(&["cm", "-d", "alpine", "-e", "uname", "-a"]).unwrap();
        assert!(args.exec);
        assert_eq!(args.command, ["uname", "-a"]);
    }

    #[test]
    fn dash_dash_passthrough() {
        let args = parse(&["cm", "--", "cat", "/proc/cpuinfo"]).unwrap();
        assert_eq!(args.command, ["cat", "/proc/cpuinfo"]);
    }

    #[test]
    fn bare_positional_is_command() {
        let args = parse(&["cm", "uname"]).unwrap();
        assert_eq!(args.command, ["uname"]);
        assert_eq!(args.action().unwrap(), Action::Run);
    }

    #[test]
    fn exec_requires_command() {
        let args = parse(&["cm", "-e"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn shell_type_none_requires_command() {
        let args = parse(&["cm", "--shell-type", "none"]).unwrap();
        assert!(args.action().is_err());
        let args = parse(&["cm", "--shell-type", "none", "-e", "ls"]).unwrap();
        assert_eq!(args.action().unwrap(), Action::Run);
    }

    #[test]
    fn list_subflags() {
        let args = parse(&["cm", "-l"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::List {
                running_only: false,
                quiet: false,
                verbosity: 0,
                online: false,
                cache: false,
                catalog: None,
                arch: None
            }
        );
        let args = parse(&["cm", "-l", "-q"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List { quiet: true, .. }
        ));
        let args = parse(&["cm", "--list", "--running"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List {
                running_only: true,
                ..
            }
        ));
    }

    #[test]
    fn verbose_counts() {
        let args = parse(&["cm", "-l", "-v", "-v"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List { verbosity: 2, .. }
        ));
        assert!(parse(&["cm", "-l", "-v", "-q"]).is_err());
    }

    #[test]
    fn quiet_requires_list() {
        assert!(parse(&["cm", "-q"]).is_err());
    }

    #[test]
    fn terminate_and_set_default() {
        let args = parse(&["cm", "-t", "alpine"]).unwrap();
        assert_eq!(args.action().unwrap(), Action::Terminate("alpine".into()));
        let args = parse(&["cm", "-s", "alpine"]).unwrap();
        assert_eq!(args.action().unwrap(), Action::SetDefault("alpine".into()));
    }

    #[test]
    fn run_args_conflict_with_actions() {
        let args = parse(&["cm", "-l", "-d", "x"]).unwrap();
        assert!(args.action().is_err());
        let args = parse(&["cm", "--shutdown", "-u", "root"]).unwrap();
        assert!(args.action().is_err());
        let args = parse(&["cm", "-t", "x", "ls"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn install_with_name() {
        let args = parse(&["cm", "--install", "alpine:latest", "--name", "dev"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Install(InstallOpts {
                source: InstallSource::CatalogOrImage("alpine:latest".into()),
                catalog: None,
                name: Some("dev".into()),
                no_launch: false,
                cpus: None,
                memory: None,
                arch: None,
                home_mount: None,
                distro: false,
                machine: false,
                restricted: false,
                trusted: false,
                use_wsl_conf: None,
                no_wsl_conf: false,
                ignore_wsl_conf: None,
                force: false,
                preset: None,
                ssh: false,
                no_ssh: false,
                shares: vec![],
                publish: vec![],
                terminal_profile: false,
                no_terminal_profile: false,
                terminal: false,
            })
        );
    }

    #[test]
    fn install_bare_installs_default() {
        let args = parse(&["cm", "--install"]).unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert_eq!(opts.source, InstallSource::Default);
        assert!(!opts.wants_machine());
    }

    #[test]
    fn install_sources_are_exclusive() {
        let args = parse(&["cm", "--install", "--from-image", "ghcr.io/x/y:1"]).unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert_eq!(opts.source, InstallSource::Image("ghcr.io/x/y:1".into()));
        assert!(!opts.wants_machine());

        let args = parse(&["cm", "--install", "--from-file", "rocky-9.wsl"]).unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert_eq!(
            opts.source,
            InstallSource::File(PathBuf::from("rocky-9.wsl"))
        );
        assert!(!opts.wants_machine());

        // At most one source per --install.
        for extra in [
            &["alpine", "--from-file", "x.wsl"][..],
            &["--from-image", "a:b", "--from-file", "x.wsl"][..],
            &["alpine", "--from-image", "a:b"][..],
        ] {
            let mut argv = vec!["cm", "--install"];
            argv.extend_from_slice(extra);
            let args = parse(&argv).unwrap();
            assert!(args.action().is_err(), "{argv:?}");
        }
        // --from-* need --install.
        assert!(parse(&["cm", "--from-file", "x.wsl"]).is_err());
        assert!(parse(&["cm", "--from-image", "a:b"]).is_err());
    }

    #[test]
    fn catalog_override_pairs() {
        let args = parse(&["cm", "--catalog", "c.json", "-l", "-o"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List {
                online: true,
                catalog: Some(_),
                ..
            }
        ));
        let args = parse(&["cm", "--catalog", "c.json", "--install", "X"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::Install(InstallOpts {
                catalog: Some(_),
                ..
            })
        ));
        // Meaningless elsewhere.
        let args = parse(&["cm", "--catalog", "c.json", "-l"]).unwrap();
        assert!(args.action().is_err());
        let args = parse(&["cm", "--catalog", "c.json", "-d", "x"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn list_online() {
        let args = parse(&["cm", "-l", "-o"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::List {
                running_only: false,
                quiet: false,
                verbosity: 0,
                online: true,
                cache: false,
                catalog: None,
                arch: None
            }
        );
        let args = parse(&["cm", "--list", "--online"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List { online: true, .. }
        ));
        assert!(parse(&["cm", "--online"]).is_err());
        assert!(parse(&["cm", "-l", "-o", "-q"]).is_err());
        let args = parse(&["cm", "-l", "-o", "-v"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List {
                online: true,
                verbosity: 1,
                ..
            }
        ));
        let args = parse(&["cm", "-l", "-o", "-v", "-v"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::List {
                online: true,
                verbosity: 2,
                ..
            }
        ));
        assert!(parse(&["cm", "-l", "-o", "--running"]).is_err());
    }

    #[test]
    fn list_arch() {
        for mode in ["-l", "-l -o"] {
            let mut argv = vec!["cm"];
            argv.extend(mode.split(' '));
            argv.extend(["--arch", "amd64"]);
            let args = parse(&argv).unwrap();
            assert_eq!(
                args.action().unwrap(),
                Action::List {
                    running_only: false,
                    quiet: false,
                    verbosity: 0,
                    online: mode.contains('o'),
                    cache: false,
                    catalog: None,
                    arch: Some(Arch::Amd64),
                }
            );
        }
        // The .wsl cache is content-addressed — an arch filter is
        // meaningless there.
        let args = parse(&["cm", "-l", "--cache", "--arch", "amd64"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn install_resource_options() {
        let args = parse(&[
            "cm",
            "--install",
            "alpine",
            "--cpus",
            "2",
            "--memory",
            "4G",
            "--home-mount",
            "ro",
        ])
        .unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert_eq!(opts.cpus, Some(2));
        assert_eq!(opts.memory.as_deref(), Some("4G"));
        assert_eq!(opts.home_mount, Some(HomeMount::Ro));
        assert!(!opts.wants_machine());
        assert!(parse(&["cm", "--cpus", "2"]).is_err());
    }

    #[test]
    fn forward_allows_distribution() {
        let args = parse(&[
            "cm",
            "-d",
            "alpine",
            "--forward",
            "8080",
            "--forward",
            "3000:80",
        ])
        .unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Forward {
                machine: Some("alpine".into()),
                mappings: vec![
                    PortMapping {
                        host: 8080,
                        guest: 8080
                    },
                    PortMapping {
                        host: 3000,
                        guest: 80
                    },
                ],
            }
        );
        assert!(parse(&["cm", "--forward", "x"]).is_err());
        let args = parse(&["cm", "--forward", "8080", "-u", "root"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn install_distro_options() {
        let args = parse(&["cm", "--install", "alpine", "--share", "/Volumes/X:/mnt/x"]).unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert!(!opts.wants_machine());
        let args = parse(&["cm", "--install", "alpine", "--distro"]).unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert!(!opts.wants_machine());
        for flag in ["--restricted", "--untrusted"] {
            let args = parse(&["cm", "--install", "alpine", flag]).unwrap();
            let Action::Install(opts) = args.action().unwrap() else {
                panic!("expected install");
            };
            assert!(opts.restricted && !opts.wants_machine(), "{flag}");
        }
        assert!(parse(&["cm", "--publish", "8080"]).is_err());
        assert!(parse(&["cm", "--restricted"]).is_err());
    }

    #[test]
    fn shutdown_system() {
        let args = parse(&["cm", "--shutdown", "--system"]).unwrap();
        assert_eq!(args.action().unwrap(), Action::Shutdown { system: true });
        assert!(parse(&["cm", "--system"]).is_err());
    }

    #[test]
    fn update_flag() {
        let args = parse(&["cm", "--update"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Update {
                version: None,
                force: false,
                dry_run: false
            }
        );
        let args = parse(&["cm", "--update", "0.5.0", "--force", "--dry-run", "--dev"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Update {
                version: Some("0.5.0".into()),
                force: true,
                dry_run: true
            }
        );
        assert!(args.dev);
        // --dev is global (it also gates CONTAINER_CLI); --dry-run
        // still only makes sense with --update
        assert!(parse(&["cm", "--dev"]).unwrap().dev);
        assert!(parse(&["cm", "--dry-run"]).is_err());
        // Run-only options can't combine; bare --force still needs an
        // action it applies to.
        assert!(
            parse(&["cm", "--update", "-d", "x"])
                .unwrap()
                .action()
                .is_err()
        );
        assert!(
            parse(&["cm", "--update", "0.5.0", "ls"])
                .unwrap()
                .action()
                .is_err()
        );
        assert!(parse(&["cm", "--force"]).unwrap().action().is_err());
    }

    #[test]
    fn name_requires_install() {
        assert!(parse(&["cm", "--name", "dev"]).is_err());
    }

    #[test]
    fn export_takes_name_and_file() {
        let args = parse(&["cm", "--export", "alpine", "out.tar"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Export {
                name: "alpine".into(),
                file: "out.tar".into()
            }
        );
        let args = parse(&["cm", "--export", "alpine", "-"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Export {
                name: "alpine".into(),
                file: "-".into()
            }
        );
        // Both values are required; run-only options are rejected.
        assert!(parse(&["cm", "--export", "alpine"]).is_err());
        let args = parse(&["cm", "--export", "alpine", "f.tar", "-d", "x"]).unwrap();
        assert!(args.action().is_err());
        let args = parse(&["cm", "--export", "alpine", "f.tar", "ls"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn import_takes_name_location_file() {
        // WSL's three values.
        let args = parse(&["cm", "--import", "u1", "C:\\wsl", "in.tar"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Import {
                name: "u1".into(),
                install_location: Some("C:\\wsl".into()),
                file: "in.tar".into(),
                backend: Backend::Auto,
                force: false,
                arch: None,
                terminal: false,
            }
        );
        // Two values omit the ignored location.
        let args = parse(&["cm", "--import", "u1", "in.tar"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Import {
                name: "u1".into(),
                install_location: None,
                file: "in.tar".into(),
                backend: Backend::Auto,
                force: false,
                arch: None,
                terminal: false,
            }
        );
        // `--distro` selects the distro import (the default anyway); `-`
        // means stdin.
        let args = parse(&["cm", "--import", "u1", "-", "--distro"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Import {
                name: "u1".into(),
                install_location: None,
                file: "-".into(),
                backend: Backend::Distro,
                force: false,
                arch: None,
                terminal: false,
            }
        );
        // `--machine` selects the machine import.
        let args = parse(&["cm", "--import", "u1", "in.tar", "--machine"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Import {
                name: "u1".into(),
                install_location: None,
                file: "in.tar".into(),
                backend: Backend::Machine,
                force: false,
                arch: None,
                terminal: false,
            }
        );
        assert!(parse(&["cm", "--import", "u1"]).is_err());
        let args = parse(&["cm", "--import", "u1", "loc", "f", "-u", "root"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn selectors_apply_to_name_resolving_actions() {
        // Bare `cm --distro`/`cm --machine` are valid — they select the
        // default of that kind.
        assert_eq!(
            parse(&["cm", "--distro"]).unwrap().backend(),
            Backend::Distro
        );
        assert_eq!(
            parse(&["cm", "--machine"]).unwrap().backend(),
            Backend::Machine
        );
        assert_eq!(parse(&["cm"]).unwrap().backend(), Backend::Auto);
        // The flags conflict.
        assert!(parse(&["cm", "--distro", "--machine"]).is_err());
        // They apply to name-resolving actions.
        for argv in [
            &["cm", "-d", "x", "--machine", "-e", "true"][..],
            &["cm", "-t", "x", "--distro"][..],
            &["cm", "-s", "x", "--machine"][..],
            &["cm", "--unregister", "x", "--distro"][..],
            &["cm", "--export", "x", "f.tar", "--machine"][..],
            &["cm", "--forward", "8080", "--distro"][..],
        ] {
            let args = parse(argv).unwrap();
            assert!(args.action().is_ok(), "{argv:?}");
        }
        // And are rejected where no name is resolved.
        for argv in [
            &["cm", "-l", "--distro"][..],
            &["cm", "--shutdown", "--machine"][..],
            &["cm", "--status", "--distro"][..],
            &["cm", "--version", "--machine"][..],
        ] {
            let args = parse(argv).unwrap();
            assert!(args.action().is_err(), "{argv:?}");
        }
    }

    #[test]
    fn machine_rejects_distro_only_install_options() {
        for argv in [
            &["cm", "--install", "alpine", "--machine", "--restricted"][..],
            &["cm", "--install", "--machine", "--from-file", "x.wsl"][..],
            &["cm", "--install", "alpine", "--machine", "--share", "/a:/b"][..],
            &[
                "cm",
                "--install",
                "alpine",
                "--machine",
                "--publish",
                "8080",
            ][..],
        ] {
            let args = parse(argv).unwrap();
            assert!(args.action().is_err(), "{argv:?}");
        }
        // --no-ssh can't survive to action() at all — clap refuses it:
        // a machine forwards the agent unconditionally.
        assert!(parse(&["cm", "--install", "alpine", "--machine", "--no-ssh"]).is_err());
    }

    #[test]
    fn rename_takes_old_and_new() {
        let args = parse(&["cm", "--rename", "old", "new"]).unwrap();
        assert_eq!(
            args.action().unwrap(),
            Action::Rename {
                old: "old".into(),
                new: "new".into(),
                backend: Backend::Auto,
                force: false,
            }
        );
        // Selectors disambiguate a name that's both a machine and a distro.
        let args = parse(&["cm", "--rename", "old", "new", "--machine"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::Rename {
                backend: Backend::Machine,
                ..
            }
        ));
        // Both names are required; run-only options are rejected.
        assert!(parse(&["cm", "--rename", "old"]).is_err());
        let args = parse(&["cm", "--rename", "old", "new", "-d", "x"]).unwrap();
        assert!(args.action().is_err());
        let args = parse(&["cm", "--rename", "old", "new", "ls"]).unwrap();
        assert!(args.action().is_err());
    }

    #[test]
    fn force_pairs_with_name_creating_actions() {
        // Valid on --install, --import, --rename.
        let args = parse(&["cm", "--install", "alpine", "--force"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::Install(InstallOpts { force: true, .. })
        ));
        let args = parse(&["cm", "--import", "u1", "in.tar", "--force"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::Import { force: true, .. }
        ));
        let args = parse(&["cm", "--rename", "a", "b", "--force"]).unwrap();
        assert!(matches!(
            args.action().unwrap(),
            Action::Rename { force: true, .. }
        ));
        // Meaningless elsewhere.
        for argv in [
            &["cm", "--force"][..],
            &["cm", "-l", "--force"][..],
            &["cm", "-t", "x", "--force"][..],
            &["cm", "--export", "x", "f.tar", "--force"][..],
        ] {
            let args = parse(argv).unwrap();
            assert!(args.action().is_err(), "{argv:?}");
        }
    }
}
