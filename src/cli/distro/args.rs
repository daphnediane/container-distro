/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` command line: docker/podman-style subcommands.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

pub use super::catalog::CatalogCommand;
pub use super::list::ListArgs;
pub use super::run::RunArgs;
pub use super::set::SetArgs;
use crate::distro::CreateOptions;

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
    List(ListArgs),

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
    Run(RunArgs),

    /// Change cpus/memory/mounts/ports (recreates the container, keeping
    /// its filesystem)
    Set(SetArgs),

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
    use crate::distro::spec::Arch;
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
        let Command::Run(RunArgs { command, .. }) = cli.command else {
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
