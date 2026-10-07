/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` command line: docker/podman-style subcommands.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use container_distro::ops::CreateOptions;
use container_distro::spec::{Automount, HomeMount, MountSpec, PublishSpec};

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
        opts: CreateOptions,
        /// Image reference, e.g. alpine:latest
        image: String,
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
        #[arg(required = true)]
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
    },

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
        opts: CreateOptions,
    },

    /// Migrate between a `container machine` and a distro, carrying the
    /// root filesystem across; the source is removed unless `--keep`
    Migrate {
        /// Machine to migrate in (with --out, the distro to migrate out)
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
        opts: CreateOptions,
    },

    /// Register as a `container` CLI plugin so `container distro` works
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
    UninstallPlugin {
        #[arg(long)]
        plugin_dir: Option<PathBuf>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(argv)
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
}
