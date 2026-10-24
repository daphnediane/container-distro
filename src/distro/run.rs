/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `run`: `container exec` into a distro.

use super::create::ensure_running;
use super::host::host_home;
use super::journal::read_journal;
use super::resolve;
use crate::container::{self, GuestEnv, container_cmd, ensure_started};
use crate::distro::spec::{INIT_DIR, ResolvedSpec};
use anyhow::Result;
use std::env;
use std::io::{self, IsTerminal};
use std::process::Command;

/// Default working directory: the host cwd's guest path if it is shared
/// (via the home mount or an extra mount), else the guest home.
fn default_workdir(r: &ResolvedSpec, home: &str) -> String {
    env::current_dir()
        .ok()
        .and_then(|cwd| r.guest_path(&cwd, home))
        .unwrap_or_else(|| r.user.guest_home())
}

#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    pub name: Option<String>,
    pub user: Option<String>,
    pub root: bool,
    pub workdir: Option<String>,
    pub env: GuestEnv,
    pub shell: bool,
    pub no_login: bool,
    pub command: Vec<String>,
}

/// Build the `container exec` command that runs `o` in its distro,
/// booting the distro first if needed. The caller execs it.
///
/// Unlike `container machine run`, `container exec` takes an argv vector,
/// so commands arrive exactly as given (no shell re-evaluation).
pub fn run_command(o: RunOpts) -> Result<Command> {
    ensure_started()?;
    if let Some(u) = &o.user {
        container::validate_user(u)?;
    }
    let name = resolve(o.name)?;
    let info = ensure_running(&name)?;
    let home = host_home()?;
    let spec = read_journal(&name)?;
    let me = &spec.user;

    let mut cmd = container_cmd();
    cmd.args(["exec", "-i"]);
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        cmd.arg("-t");
    }
    if o.root {
        cmd.args(["--user", "0:0"]);
    } else if let Some(u) = &o.user {
        cmd.args(["--user", u]);
    } else {
        cmd.arg("--user").arg(format!("{}:{}", me.uid, me.gid));
        for (k, v) in [
            ("HOME", me.guest_home()),
            ("USER", me.name.clone()),
            ("LOGNAME", me.name.clone()),
        ] {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
    }
    let r = spec.resolve().0;
    // `~`/`~/x` name the *effective* user's guest home: the default
    // user's and root's (`/root`) are known without asking, anything
    // else is looked up in the guest's /etc/passwd — `www`'s home is
    // `/etc/www`, not `/home/www` — with conventional fallbacks if the
    // lookup can't run.
    let workdir = match o.workdir.as_deref() {
        Some(w) if w == "~" || w.starts_with("~/") => {
            let u = o.user.as_deref().map(|u| u.split(':').next().unwrap_or(u));
            let h = if o.root {
                "/root".to_string()
            } else {
                match u {
                    Some("root") => "/root".into(),
                    Some(u) => container::guest_passwd_home(info.id(), u).unwrap_or_else(|| {
                        match u.parse::<u32>() {
                            Ok(0) => "/root".into(),
                            Ok(uid) if uid == me.uid => r.user.guest_home(),
                            Ok(_) => "/".into(),
                            Err(_) => format!("/home/{u}"),
                        }
                    }),
                    None => r.user.guest_home(),
                }
            };
            match w.strip_prefix("~/") {
                Some(rest) => format!("{}/{rest}", h.trim_end_matches('/')),
                None => h,
            }
        }
        Some(w) => w.to_string(),
        None => default_workdir(&r, &home),
    };
    cmd.args(["--workdir", &workdir]);
    if let Ok(term) = env::var("TERM") {
        cmd.arg("--env").arg(format!("TERM={term}"));
    }
    o.env.push_args(&mut cmd, "--env");
    cmd.arg(info.id());
    let init = format!("{INIT_DIR}/init");
    if o.command.is_empty() {
        cmd.arg(&init).arg(if o.no_login { "-s" } else { "-l" });
    } else if o.shell {
        cmd.arg(&init).arg("-c").args(&o.command);
    } else {
        cmd.args(&o.command);
    }
    Ok(cmd)
}
