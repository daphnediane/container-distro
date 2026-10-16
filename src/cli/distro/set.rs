/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro set`.

use anyhow::Result;
use clap::Args;

use crate::ops;
use crate::spec::{
    Automount, ConfList, HomeMount, MountSpec, Preset, PublishSpec, ResetTarget, SpecChanges,
};

#[derive(Debug, Args)]
pub struct SetArgs {
    pub name: String,
    /// Change the defaults tier — re-derives every option you
    /// haven't explicitly pinned
    #[arg(long, value_enum, conflicts_with_all = ["restricted", "trusted"])]
    pub preset: Option<Preset>,
    /// Restricted tier — `--preset restricted` keeping your pinned
    /// options
    #[arg(long, visible_alias = "untrusted", conflicts_with = "trusted")]
    pub restricted: bool,
    /// Trusted tier — `--preset trusted` keeping your pinned options
    #[arg(long)]
    pub trusted: bool,
    /// Un-pin an option back to inherited (`--reset home-mount`),
    /// clear a list (`--reset mounts`), or reset everything to a
    /// tier (`--reset restricted` — also sets the preset)
    #[arg(long, value_enum)]
    pub reset: Vec<ResetTarget>,
    #[arg(long)]
    pub cpus: Option<u64>,
    #[arg(long)]
    pub memory: Option<String>,
    #[arg(long, value_enum)]
    pub home_mount: Option<HomeMount>,
    /// Attach to network NAME ("none" disables, "default" restores)
    #[arg(long, value_name = "NAME")]
    pub network: Option<String>,
    /// Forward the host SSH agent socket into the distro
    #[arg(long, conflicts_with = "no_ssh")]
    pub ssh: bool,
    /// Stop forwarding the host SSH agent socket
    #[arg(long)]
    pub no_ssh: bool,
    /// Grant the provisioned user passwordless sudo/doas
    #[arg(long, conflicts_with = "no_sudo")]
    pub sudo: bool,
    /// Stop provisioning sudo/doas (does not remove existing grants
    /// inside the guest)
    #[arg(long)]
    pub no_sudo: bool,
    /// Auto-manage /Volumes mounts at /mnt/<name>, rescanning now and
    /// at each start; `--automount ro` mounts read-only
    #[arg(long, value_enum, num_args = 0..=1, require_equals = true,
          default_missing_value = "rw", value_name = "rw|ro|none",
          conflicts_with = "no_automount")]
    pub automount: Option<Automount>,
    /// Stop auto-managing /Volumes mounts (removes the automounts)
    #[arg(long)]
    pub no_automount: bool,
    /// Honor the guest's /etc/wsl.conf — optionally a comma list
    /// of features: hostname, fstab, init, command, automount, or
    /// `all`/`none` (a bare flag means `all`)
    #[arg(long, value_name = "FEATURE,...|all|none", num_args = 0..=1,
          require_equals = true, default_missing_value = "all")]
    pub use_wsl_conf: Option<ConfList>,
    /// Stop honoring /etc/wsl.conf entirely (drops the snapshots)
    #[arg(long, conflicts_with_all = ["use_wsl_conf", "ignore_wsl_conf"])]
    pub no_wsl_conf: bool,
    /// Deny parts of /etc/wsl.conf — subtracted from what would
    /// resolve (`all` drops the snapshots too)
    #[arg(long, value_name = "FEATURE,...|all|none")]
    pub ignore_wsl_conf: Option<ConfList>,
    /// Add (or replace, by DST) a mount: SRC:DST[:ro]
    #[arg(long = "add-volume", value_name = "SRC:DST[:ro]")]
    pub add_volumes: Vec<MountSpec>,
    /// Remove the mount at guest path DST
    #[arg(long = "rm-volume", value_name = "DST")]
    pub rm_volumes: Vec<String>,
    /// Publish a port (replaces one on the same host port)
    #[arg(long = "publish", value_name = "SPEC")]
    pub publish: Vec<PublishSpec>,
    /// Stop publishing HOST_PORT
    #[arg(long = "unpublish", value_name = "HOST_PORT")]
    pub unpublish: Vec<u16>,
    /// Generate the terminal-profile launchers (the `.terminal`
    /// file and iTerm2 dynamic profile) — open the distro in
    /// Terminal with `container distro terminal NAME`
    #[arg(long, conflicts_with = "no_terminal_profile")]
    pub terminal_profile: bool,
    /// Don't generate terminal profiles for this distro
    #[arg(long)]
    pub no_terminal_profile: bool,
    /// Record host file-access intent for the planned `container
    /// distro fs`
    #[arg(long, conflicts_with = "no_files")]
    pub files: bool,
    /// Record intent against host file access
    #[arg(long)]
    pub no_files: bool,
}

impl SetArgs {
    /// The flags as spec edits — paired `--x`/`--no-x` flags become a
    /// tri-state pin.
    fn changes(self) -> SpecChanges {
        let SetArgs {
            preset,
            restricted,
            trusted,
            reset,
            cpus,
            memory,
            home_mount,
            network,
            ssh,
            no_ssh,
            sudo,
            no_sudo,
            automount,
            no_automount,
            use_wsl_conf,
            no_wsl_conf,
            ignore_wsl_conf,
            add_volumes,
            rm_volumes,
            publish,
            unpublish,
            terminal_profile,
            no_terminal_profile,
            files,
            no_files,
            ..
        } = self;
        SpecChanges {
            preset: preset.or(if restricted {
                Some(Preset::Restricted)
            } else if trusted {
                Some(Preset::Trusted)
            } else {
                None
            }),
            reset,
            cpus,
            memory,
            home_mount,
            network,
            ssh: if ssh {
                Some(true)
            } else if no_ssh {
                Some(false)
            } else {
                None
            },
            admin: if sudo {
                Some(true)
            } else if no_sudo {
                Some(false)
            } else {
                None
            },
            wsl_conf: use_wsl_conf.or(no_wsl_conf.then(ConfList::none)),
            wsl_conf_ignore: ignore_wsl_conf,
            automount: automount.or(no_automount.then_some(Automount::None)),
            add_mounts: add_volumes,
            remove_mounts: rm_volumes,
            add_publish: publish,
            remove_publish: unpublish,
            terminal_profile: if terminal_profile {
                Some(true)
            } else if no_terminal_profile {
                Some(false)
            } else {
                None
            },
            files: if files {
                Some(true)
            } else if no_files {
                Some(false)
            } else {
                None
            },
        }
    }
}

pub(super) fn run(args: SetArgs) -> Result<()> {
    let name = args.name.clone();
    ops::set(&name, &args.changes())
}
