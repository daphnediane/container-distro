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

use anyhow::{Result, bail};
use clap::{ArgAction, Parser, ValueEnum};
use cm_core::forward::PortMapping;
pub use container_distro::spec::HomeMount;
use container_distro::spec::{MountSpec, PublishSpec};

/// Options for `--install`.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallOpts {
    pub image: String,
    pub name: Option<String>,
    pub no_launch: bool,
    pub cpus: Option<u32>,
    pub memory: Option<String>,
    pub home_mount: Option<HomeMount>,
    /// Create a `container distro` instead of a machine.
    pub distro: bool,
    /// Restricted defaults (no mounts, network, agent, sudo). Implies
    /// `--distro` — machines can't be restricted.
    pub restricted: bool,
    pub shares: Vec<MountSpec>,
    pub publish: Vec<PublishSpec>,
}

impl InstallOpts {
    /// `--distro`, or any distro-only option, selects a distro.
    pub fn wants_distro(&self) -> bool {
        self.distro || self.restricted || !self.shares.is_empty() || !self.publish.is_empty()
    }
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

    /// Show a NAME/STATE/VERSION table (with --list); repeat for more columns
    #[arg(
        short = 'v',
        long = "verbose",
        action = ArgAction::Count,
        requires = "list",
        conflicts_with = "quiet"
    )]
    pub verbose: u8,

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

    /// Delete a machine and its persistent storage
    #[arg(long = "unregister", value_name = "MACHINE")]
    pub unregister: Option<String>,

    /// Create a machine from a container image and boot it
    #[arg(long = "install", value_name = "IMAGE")]
    pub install: Option<String>,

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

    /// Home directory mount for the machine created by --install
    #[arg(
        long = "home-mount",
        requires = "install",
        value_enum,
        value_name = "MODE"
    )]
    pub home_mount: Option<HomeMount>,

    /// Create a distro (`container distro`) instead of a machine with --install
    #[arg(long = "distro", requires = "install")]
    pub distro: bool,

    /// Restricted defaults for a distro created by --install: no home or
    /// extra mounts, no network, no SSH agent, no sudo/doas (implies
    /// --distro). A convenience preset, not a sandbox.
    #[arg(long = "restricted", visible_alias = "untrusted", requires = "install")]
    pub restricted: bool,

    /// Share a host directory into a distro created by --install:
    /// SRC:DST[:ro] (repeatable; implies --distro)
    #[arg(long = "share", requires = "install", value_name = "SRC:DST[:ro]")]
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
    /// WSL-shaped machine list.
    List {
        running_only: bool,
        quiet: bool,
        verbosity: u8,
    },
    /// Stop every running machine; with `system`, also stop services.
    Shutdown { system: bool },
    /// `container system status`.
    Status,
    /// `container machine set-default`.
    SetDefault(String),
    /// `container machine stop`.
    Terminate(String),
    /// `container machine rm`.
    Unregister(String),
    /// `container machine create`, then optionally open a shell.
    Install(InstallOpts),
    /// Forward localhost ports to a machine.
    Forward {
        machine: Option<String>,
        mappings: Vec<PortMapping>,
    },
    /// Print cm and container versions.
    Version,
}

impl Args {
    /// Resolve the parsed arguments into a single validated action.
    pub fn action(&self) -> Result<Action> {
        let action = if self.version {
            Action::Version
        } else if self.status {
            Action::Status
        } else if let Some(m) = &self.set_default {
            Action::SetDefault(m.clone())
        } else if let Some(m) = &self.terminate {
            Action::Terminate(m.clone())
        } else if let Some(m) = &self.unregister {
            Action::Unregister(m.clone())
        } else if let Some(i) = &self.install {
            Action::Install(InstallOpts {
                image: i.clone(),
                name: self.name.clone(),
                no_launch: self.no_launch,
                cpus: self.cpus,
                memory: self.memory.clone(),
                home_mount: self.home_mount,
                distro: self.distro,
                restricted: self.restricted,
                shares: self.shares.clone(),
                publish: self.publish.clone(),
            })
        } else if !self.forward.is_empty() {
            let action = Action::Forward {
                machine: self.distribution.clone(),
                mappings: self.forward.clone(),
            };
            self.check_no_run_args_except_distribution()?;
            return Ok(action);
        } else if self.shutdown {
            Action::Shutdown {
                system: self.system,
            }
        } else if self.list {
            Action::List {
                running_only: self.running,
                quiet: self.quiet,
                verbosity: self.verbose,
            }
        } else {
            return self.validate_run();
        };
        self.check_no_run_args()?;
        Ok(action)
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
                verbosity: 0
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
                image: "alpine:latest".into(),
                name: Some("dev".into()),
                no_launch: false,
                cpus: None,
                memory: None,
                home_mount: None,
                distro: false,
                restricted: false,
                shares: vec![],
                publish: vec![],
            })
        );
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
        assert!(!opts.wants_distro());
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
        assert!(opts.wants_distro());
        let args = parse(&["cm", "--install", "alpine", "--distro"]).unwrap();
        let Action::Install(opts) = args.action().unwrap() else {
            panic!("expected install");
        };
        assert!(opts.wants_distro());
        for flag in ["--restricted", "--untrusted"] {
            let args = parse(&["cm", "--install", "alpine", flag]).unwrap();
            let Action::Install(opts) = args.action().unwrap() else {
                panic!("expected install");
            };
            assert!(opts.restricted && opts.wants_distro(), "{flag}");
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
    fn name_requires_install() {
        assert!(parse(&["cm", "--name", "dev"]).is_err());
    }
}
