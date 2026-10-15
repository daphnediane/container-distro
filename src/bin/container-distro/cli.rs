/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` command line: docker/podman-style subcommands.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use container_distro::ops::CreateOptions;
use container_distro::spec::{
    Arch, Automount, ConfList, HomeMount, MountSpec, Preset, PublishSpec, ResetTarget,
};

#[derive(Debug, Parser)]
#[command(
    name = "container distro",
    version,
    about = "Machine-like Linux distros for Apple `container`, with extra mounts and published ports",
    long_about = "Creates long-lived, machine-like containers (\"distros\"): your macOS account is \
                  provisioned inside, your home directory is shared at the same path, and the \
                  image's init system runs as PID 1 — like `container machine`, plus host \
                  directories outside $HOME, published ports, export/import, and `set` for \
                  changing settings after creation."
)]
pub struct Cli {
    /// Enable dev-mode overrides for this run — a non-default
    /// CONTAINER_CLI binary and the CONTAINER_DISTRO_RELEASES_URL
    /// fixture server (testing only — DEV.md)
    #[arg(long, global = true)]
    pub dev: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    /// List the installable-distro catalog
    #[command(visible_alias = "ls")]
    List {
        /// Show local cache/image/instance state; repeat for source refs
        #[arg(short, long, action = ArgAction::Count)]
        verbose: u8,
        /// Use FILE instead of the built-in catalog
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
        /// List entries installable on ARCH — amd64 selects x86-64
        /// sources (Rosetta-translated on Apple silicon)
        #[arg(long, value_enum, value_name = "ARCH")]
        arch: Option<Arch>,
    },

    /// Show one catalog entry and its local state
    Info {
        /// Catalog entry to inspect (`catalog list` shows the choices)
        #[arg(value_name = "DISTRO")]
        distro: String,
        /// Use FILE instead of the built-in catalog
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },

    /// Install a distro from the catalog (default: Ubuntu)
    Install {
        /// Catalog entry to install (`catalog list` shows the choices)
        #[arg(value_name = "DISTRO")]
        distro: Option<String>,
        /// Distro name (default: the catalog entry's NAME, or the .wsl
        /// package's oobe.defaultName)
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Use FILE instead of the built-in catalog
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
        #[command(flatten)]
        opts: Box<CreateOptions>,
    },

    /// List cached .wsl catalog downloads
    Cache {
        /// Use FILE instead of the built-in catalog for entry names
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
    },

    /// Delete cached .wsl catalog downloads
    Purge,
}

/// `container distro fs`: host-side access through the distro's
/// `distrofs` server. Guest paths are absolute from the distro root;
/// symlinks resolve inside the distro, never on the host.
#[derive(Debug, Subcommand)]
pub enum FsCommand {
    /// List a directory (`-l` for owner, mode, size, and link targets)
    Ls {
        name: String,
        /// Guest path (default: /)
        #[arg(default_value = "/")]
        path: String,
        #[arg(short, long)]
        long: bool,
    },
    /// Show one entry's attributes (`-L` follows a final symlink)
    Stat {
        name: String,
        path: String,
        #[arg(short = 'L', long)]
        dereference: bool,
    },
    /// Copy a guest file to DEST (`-` or omitted: stdout)
    Get {
        name: String,
        path: String,
        dest: Option<PathBuf>,
    },
    /// Copy a host file into the guest at PATH (`-` as SRC: stdin);
    /// new files belong to your distro account
    Put {
        name: String,
        src: PathBuf,
        path: String,
    },
    /// Check the relay, peer identity, and handshake
    Check { name: Option<String> },
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a distro from an image and boot it
    Create {
        /// Distro name (default: derived from the image)
        #[arg(short = 'n', long)]
        name: Option<String>,
        #[command(flatten)]
        opts: Box<CreateOptions>,
        /// Image reference, e.g. alpine:latest
        image: String,
    },

    /// List and install catalog distributions
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },

    /// List distros
    #[command(visible_alias = "ls")]
    List {
        /// Only names
        #[arg(short, long)]
        quiet: bool,
        /// Only running distros
        #[arg(long)]
        running: bool,
        /// Only distros of this architecture
        #[arg(long, value_enum, value_name = "ARCH")]
        arch: Option<Arch>,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },

    /// Show distros' container configuration as JSON
    Inspect {
        #[arg(required = true)]
        names: Vec<String>,
    },

    /// Boot a stopped distro
    Start { name: String },

    /// Stop running distros
    Stop {
        #[arg(required = true)]
        names: Vec<String>,
    },

    /// Delete distros and their storage
    #[command(visible_alias = "rm")]
    Delete {
        /// Stop running distros first
        #[arg(short, long)]
        force: bool,
        /// Delete every distro — uninstall-style cleanup
        #[arg(long, conflicts_with = "names")]
        all: bool,
        #[arg(required_unless_present = "all")]
        names: Vec<String>,
    },

    /// Run a command or interactive shell in a distro, booting it if needed
    Run {
        /// Distro (default distro if omitted)
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Run as USER (name or uid[:gid]); default: your account
        #[arg(short, long, conflicts_with = "root")]
        user: Option<String>,
        /// Run as root
        #[arg(long)]
        root: bool,
        /// Working directory (default: the current directory if it is
        /// under the shared home, else the guest home)
        #[arg(short = 'w', long = "workdir", visible_alias = "cwd")]
        workdir: Option<String>,
        /// Environment variable KEY=VALUE (or KEY to inherit)
        #[arg(short, long)]
        env: Vec<String>,
        /// Run the command line through the user's shell (`$SHELL -c`)
        #[arg(long)]
        shell: bool,
        /// Interactive shell is non-login
        #[arg(long)]
        no_login: bool,
        /// Command and arguments (default: a login shell)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Change cpus/memory/mounts/ports (recreates the container, keeping
    /// its filesystem)
    Set {
        name: String,
        /// Change the defaults tier — re-derives every option you
        /// haven't explicitly pinned
        #[arg(long, value_enum, conflicts_with_all = ["restricted", "trusted"])]
        preset: Option<Preset>,
        /// Restricted tier — `--preset restricted` keeping your pinned
        /// options
        #[arg(long, visible_alias = "untrusted", conflicts_with = "trusted")]
        restricted: bool,
        /// Trusted tier — `--preset trusted` keeping your pinned options
        #[arg(long)]
        trusted: bool,
        /// Un-pin an option back to inherited (`--reset home-mount`),
        /// clear a list (`--reset mounts`), or reset everything to a
        /// tier (`--reset restricted` — also sets the preset)
        #[arg(long, value_enum)]
        reset: Vec<ResetTarget>,
        #[arg(long)]
        cpus: Option<u64>,
        #[arg(long)]
        memory: Option<String>,
        #[arg(long, value_enum)]
        home_mount: Option<HomeMount>,
        /// Attach to network NAME ("none" disables, "default" restores)
        #[arg(long, value_name = "NAME")]
        network: Option<String>,
        /// Forward the host SSH agent socket into the distro
        #[arg(long, conflicts_with = "no_ssh")]
        ssh: bool,
        /// Stop forwarding the host SSH agent socket
        #[arg(long)]
        no_ssh: bool,
        /// Grant the provisioned user passwordless sudo/doas
        #[arg(long, conflicts_with = "no_sudo")]
        sudo: bool,
        /// Stop provisioning sudo/doas (does not remove existing grants
        /// inside the guest)
        #[arg(long)]
        no_sudo: bool,
        /// Auto-manage /Volumes mounts at /mnt/<name>, rescanning now and
        /// at each start; `--automount ro` mounts read-only
        #[arg(long, value_enum, num_args = 0..=1, require_equals = true,
              default_missing_value = "rw", value_name = "rw|ro|none",
              conflicts_with = "no_automount")]
        automount: Option<Automount>,
        /// Stop auto-managing /Volumes mounts (removes the automounts)
        #[arg(long)]
        no_automount: bool,
        /// Honor the guest's /etc/wsl.conf — optionally a comma list
        /// of features: hostname, fstab, init, command, automount, or
        /// `all`/`none` (a bare flag means `all`)
        #[arg(long, value_name = "FEATURE,...|all|none", num_args = 0..=1,
              require_equals = true, default_missing_value = "all")]
        use_wsl_conf: Option<ConfList>,
        /// Stop honoring /etc/wsl.conf entirely (drops the snapshots)
        #[arg(long, conflicts_with_all = ["use_wsl_conf", "ignore_wsl_conf"])]
        no_wsl_conf: bool,
        /// Deny parts of /etc/wsl.conf — subtracted from what would
        /// resolve (`all` drops the snapshots too)
        #[arg(long, value_name = "FEATURE,...|all|none")]
        ignore_wsl_conf: Option<ConfList>,
        /// Add (or replace, by DST) a mount: SRC:DST[:ro]
        #[arg(long = "add-volume", value_name = "SRC:DST[:ro]")]
        add_volumes: Vec<MountSpec>,
        /// Remove the mount at guest path DST
        #[arg(long = "rm-volume", value_name = "DST")]
        rm_volumes: Vec<String>,
        /// Publish a port (replaces one on the same host port)
        #[arg(long = "publish", value_name = "SPEC")]
        publish: Vec<PublishSpec>,
        /// Stop publishing HOST_PORT
        #[arg(long = "unpublish", value_name = "HOST_PORT")]
        unpublish: Vec<u16>,
        /// Generate the terminal-profile launchers (the `.terminal`
        /// file and iTerm2 dynamic profile) — open the distro in
        /// Terminal with `container distro terminal NAME`
        #[arg(long, conflicts_with = "no_terminal_profile")]
        terminal_profile: bool,
        /// Don't generate terminal profiles for this distro
        #[arg(long)]
        no_terminal_profile: bool,
        /// Publish the host file-access relay (`container distro fs`)
        #[arg(long, conflicts_with = "no_files")]
        files: bool,
        /// Stop publishing the host file-access relay
        #[arg(long)]
        no_files: bool,
    },

    /// Browse and copy files inside a distro from the host (boots it if
    /// needed) — the `\\wsl$` building block
    Fs {
        #[command(subcommand)]
        command: FsCommand,
    },

    /// Regenerate the `.terminal` launchers and iTerm2 dynamic
    /// profiles for every distro — repairs files missing after an
    /// upgrade or cleanup, honoring recorded opt-outs, and prunes
    /// artifacts for distros that no longer exist
    SyncProfiles,

    /// Open a distro in Terminal.app — imports/updates its profile
    /// and launches a window (`cm --terminal` does the same)
    Terminal { name: String },

    /// Set (or with --clear, unset) the default distro
    SetDefault {
        #[arg(required_unless_present = "clear")]
        name: Option<String>,
        #[arg(long, conflicts_with = "name")]
        clear: bool,
    },

    /// Write a distro's root filesystem as a tar
    Export {
        name: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Create a distro from a rootfs tar or tar.gz (`-` reads stdin)
    Import {
        name: String,
        file: PathBuf,
        #[command(flatten)]
        opts: Box<CreateOptions>,
    },

    /// Rename a distro — its filesystem, settings, and default marker
    /// move to the new name; a running distro is restarted under it
    Rename {
        /// Current distro name
        old: String,
        /// New distro name
        new: String,
        /// Allow naming the distro after an existing `container
        /// machine` — `cm` resolves a shared name to the distro,
        /// shadowing the machine
        #[arg(long)]
        force: bool,
    },

    /// Migrate between a `container machine` and a distro, carrying the
    /// root filesystem across; the source is removed unless `--keep`.
    /// Naming a pre-journal distro container adopts it instead — its
    /// live config becomes the journal
    Migrate {
        /// Machine to migrate in (with --out, the distro to migrate
        /// out). A pre-journal distro is adopted, not migrated
        name: String,
        /// Machine → distro (the default direction)
        #[arg(long = "in", conflicts_with = "out")]
        inbound: bool,
        /// Distro → machine
        #[arg(long)]
        out: bool,
        /// Name for the result (default: the same name)
        #[arg(short = 'n', long)]
        target_name: Option<String>,
        /// Keep the source instead of removing it
        #[arg(long)]
        keep: bool,
        /// Options for the new distro — extra mounts, published ports,
        /// and resource/home-mount overrides to the machine's settings.
        /// With --out, only the `machine create` subset applies:
        /// --cpus, --memory, --home-mount, --no-boot, --set-default
        #[command(flatten)]
        opts: Box<CreateOptions>,
    },

    /// Register as a `container` CLI plugin so `container distro`
    /// works (offers to install `container` first when missing;
    /// self-elevates with sudo when the plugin dir needs root)
    InstallPlugin {
        /// Plugin directory (default: <container prefix>/libexec/container-plugins)
        #[arg(long)]
        plugin_dir: Option<PathBuf>,
        /// Binary to install (default: the running executable). Use when
        /// updating via an already-installed copy.
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,
    },

    /// Install the `container-distro(1)` man pages (one per subcommand)
    InstallMan {
        /// Man directory (default: the `share/man/man1` next to the
        /// binary's `bin` directory — e.g. $CARGO_HOME/share/man/man1
        /// after `cargo install`)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },

    /// Remove man pages installed by `install-man` (only files that
    /// still look like our generated pages are removed)
    UninstallMan {
        /// Man directory (default: the same binary-derived location
        /// `install-man` uses)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },

    /// Install bash/zsh/fish completions for `container-distro`
    InstallCompletions {
        /// Share root (default: the `share` next to the binary's `bin`
        /// directory — e.g. $CARGO_HOME/share after `cargo install`)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },

    /// Remove completions installed by `install-completions` (only
    /// files that still look like generated scripts are removed)
    UninstallCompletions {
        /// Share root (default: the same binary-derived location
        /// `install-completions` uses)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },

    /// Remove the `container distro` plugin registration
    /// (self-elevates with sudo when the plugin dir needs root)
    UninstallPlugin {
        #[arg(long)]
        plugin_dir: Option<PathBuf>,
    },

    /// Update container-distro itself: download a release .pkg (the
    /// latest, or VERSION like `0.5.0`) and hand it to Installer.app.
    /// Downgrades and unsigned packages ask first; --force skips that
    Update {
        /// Release to install (default: the latest)
        #[arg(value_name = "VERSION")]
        version: Option<String>,
        /// Skip the interactive confirmations (reinstall, downgrade,
        /// unsigned package, a different signing team)
        #[arg(long)]
        force: bool,
        /// Report what would be installed without installing it
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(argv)
    }

    #[test]
    fn catalog_subcommands() {
        let cli = parse(&[
            "distro",
            "catalog",
            "list",
            "-v",
            "-v",
            "--catalog",
            "cat.json",
        ])
        .unwrap();
        let Command::Catalog {
            command:
                CatalogCommand::List {
                    verbose,
                    catalog,
                    arch,
                },
        } = cli.command
        else {
            panic!("expected catalog list");
        };
        assert_eq!(verbose, 2);
        assert_eq!(catalog.as_deref(), Some(Path::new("cat.json")));
        assert_eq!(arch, None);

        let cli = parse(&["distro", "catalog", "list", "--arch", "amd64"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Catalog {
                command: CatalogCommand::List {
                    arch: Some(Arch::Amd64),
                    ..
                }
            }
        ));

        let cli = parse(&[
            "distro",
            "catalog",
            "info",
            "alpine",
            "--catalog",
            "cat.json",
            "--format",
            "json",
        ])
        .unwrap();
        let Command::Catalog {
            command:
                CatalogCommand::Info {
                    distro,
                    catalog,
                    format,
                },
        } = cli.command
        else {
            panic!("expected catalog info");
        };
        assert_eq!(distro, "alpine");
        assert_eq!(catalog.as_deref(), Some(Path::new("cat.json")));
        assert_eq!(format, Format::Json);

        let cli = parse(&[
            "distro",
            "catalog",
            "install",
            "alpine",
            "-n",
            "dev",
            "--restricted",
            "--no-boot",
        ])
        .unwrap();
        let Command::Catalog {
            command:
                CatalogCommand::Install {
                    distro,
                    name,
                    catalog,
                    opts,
                },
        } = cli.command
        else {
            panic!("expected catalog install");
        };
        assert_eq!(distro.as_deref(), Some("alpine"));
        assert_eq!(name.as_deref(), Some("dev"));
        assert_eq!(catalog, None);
        assert!(opts.restricted && opts.no_boot);

        assert!(matches!(
            parse(&["distro", "catalog", "cache", "--catalog", "cat.json"])
                .unwrap()
                .command,
            Command::Catalog {
                command: CatalogCommand::Cache { .. }
            }
        ));
        assert!(matches!(
            parse(&["distro", "catalog", "purge"]).unwrap().command,
            Command::Catalog {
                command: CatalogCommand::Purge
            }
        ));
    }

    #[test]
    fn create_with_mounts_and_ports() {
        let cli = parse(&[
            "distro",
            "create",
            "-n",
            "d1",
            "-v",
            "/Volumes/X:/mnt/x:ro",
            "-p",
            "8080",
            "--cpus",
            "2",
            "alpine",
        ])
        .unwrap();
        let Command::Create { name, opts, image } = cli.command else {
            panic!("expected create");
        };
        assert_eq!(name.as_deref(), Some("d1"));
        assert_eq!(image, "alpine");
        assert!(opts.volumes[0].read_only);
        assert_eq!(opts.publish[0].host_port, 8080);
        assert_eq!(opts.home_mount, None);
    }

    #[test]
    fn create_restricted_and_alias() {
        for flag in ["--restricted", "--untrusted"] {
            let cli = parse(&["distro", "create", flag, "alpine"]).unwrap();
            let Command::Create { opts, .. } = cli.command else {
                panic!("expected create");
            };
            assert!(opts.restricted, "{flag}");
        }
        // Granular overrides combine with it; conflicting pairs don't.
        let cli = parse(&["distro", "create", "--restricted", "--ssh", "alpine"]).unwrap();
        let Command::Create { opts, .. } = cli.command else {
            panic!("expected create");
        };
        assert!(opts.ssh);
        assert!(parse(&["distro", "create", "--ssh", "--no-ssh", "alpine"]).is_err());
        assert!(parse(&["distro", "create", "--sudo", "--no-sudo", "alpine"]).is_err());
    }

    #[test]
    fn run_passes_command_verbatim() {
        let cli = parse(&["distro", "run", "-n", "d1", "--", "ls", "-la"]).unwrap();
        let Command::Run { command, .. } = cli.command else {
            panic!("expected run");
        };
        assert_eq!(command, ["ls", "-la"]);
        assert!(parse(&["distro", "run", "--root", "-u", "x"]).is_err());
    }

    #[test]
    fn set_default_clear() {
        assert!(parse(&["distro", "set-default"]).is_err());
        assert!(parse(&["distro", "set-default", "--clear"]).is_ok());
        assert!(parse(&["distro", "set-default", "d1", "--clear"]).is_err());
    }

    #[test]
    fn bad_volume_rejected() {
        assert!(parse(&["distro", "create", "-v", "relative:/x", "alpine"]).is_err());
    }

    #[test]
    fn rename_old_new_force() {
        let cli = parse(&["distro", "rename", "old", "new"]).unwrap();
        let Command::Rename { old, new, force } = cli.command else {
            panic!("expected rename");
        };
        assert_eq!((old.as_str(), new.as_str(), force), ("old", "new", false));
        let cli = parse(&["distro", "rename", "old", "new", "--force"]).unwrap();
        let Command::Rename { force, .. } = cli.command else {
            panic!("expected rename");
        };
        assert!(force);
    }

    #[test]
    fn migrate_direction() {
        let cli = parse(&["distro", "migrate", "m1"]).unwrap();
        let Command::Migrate { inbound, out, .. } = cli.command else {
            panic!("expected migrate");
        };
        assert!(!inbound && !out);
        let cli = parse(&["distro", "migrate", "--in", "m1"]).unwrap();
        let Command::Migrate { inbound, out, .. } = cli.command else {
            panic!("expected migrate");
        };
        assert!(inbound && !out);
        let cli = parse(&["distro", "migrate", "--out", "d1", "-n", "m2"]).unwrap();
        let Command::Migrate {
            name,
            out,
            target_name,
            ..
        } = cli.command
        else {
            panic!("expected migrate");
        };
        assert!(out && name == "d1" && target_name.as_deref() == Some("m2"));
        assert!(parse(&["distro", "migrate", "--in", "--out", "x"]).is_err());
    }

    #[test]
    fn update_subcommand() {
        let cli = parse(&["distro", "update"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Update {
                version: None,
                force: false,
                dry_run: false
            }
        ));
        let cli = parse(&["distro", "update", "0.5.0", "--force", "--dry-run", "--dev"]).unwrap();
        assert!(cli.dev); // global flag — also accepted before the subcommand
        assert!(parse(&["distro", "--dev", "update"]).unwrap().dev);
        let Command::Update {
            version,
            force,
            dry_run,
        } = cli.command
        else {
            panic!("expected update");
        };
        assert_eq!(version.as_deref(), Some("0.5.0"));
        assert!(force && dry_run);
    }
}
