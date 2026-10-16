/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro run`.

use std::os::unix::process::CommandExt;

use anyhow::Result;
use clap::Args;

use crate::distro::{self, RunOpts};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Distro (default distro if omitted)
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Run as USER (name or uid[:gid]); default: your account
    #[arg(short, long, conflicts_with = "root")]
    pub user: Option<String>,
    /// Run as root
    #[arg(long)]
    pub root: bool,
    /// Working directory (default: the current directory if it is
    /// under the shared home, else the guest home)
    #[arg(short = 'w', long = "workdir", visible_alias = "cwd")]
    pub workdir: Option<String>,
    /// Environment variable KEY=VALUE (or KEY to inherit)
    #[arg(short, long)]
    pub env: Vec<String>,
    /// Run the command line through the user's shell (`$SHELL -c`)
    #[arg(long)]
    pub shell: bool,
    /// Interactive shell is non-login
    #[arg(long)]
    pub no_login: bool,
    /// Command and arguments (default: a login shell)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

/// Replace this process with `container exec` in the distro.
pub(super) fn run(args: RunArgs) -> Result<()> {
    let RunArgs {
        name,
        user,
        root,
        workdir,
        env,
        shell,
        no_login,
        command,
    } = args;
    let mut cmd = distro::run_command(RunOpts {
        name,
        user,
        root,
        workdir,
        env,
        shell,
        no_login,
        command,
    })?;
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container exec`"))
}
