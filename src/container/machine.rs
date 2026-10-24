/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container machine` helpers: listing, inspecting, and running commands in machines.

use super::guest_env::GuestEnv;
use super::types::{Machine, MachineDetail};
use super::{container_cmd, json_output};
use anyhow::{Context, Result};
use std::env;
use std::path::Path;
use std::process::{Command, Stdio};

/// All machines known to `container`.
pub fn list_machines() -> Result<Vec<Machine>> {
    json_output(&["machine", "list", "--format", "json"])
}

/// `container machine inspect` for one machine.
pub fn inspect_machine(id: &str) -> Result<MachineDetail> {
    inspect_machine_or_default(Some(id))
}

/// `container machine inspect` for `id`, or the default machine.
pub fn inspect_machine_or_default(id: Option<&str>) -> Result<MachineDetail> {
    let mut args = vec!["machine", "inspect"];
    args.extend(id);
    let details: Vec<MachineDetail> = json_output(&args)?;
    details
        .into_iter()
        .next()
        .with_context(|| format!("machine `{}` not found", id.unwrap_or("(default)")))
}

/// Starting directory for a machine session, matching `machine run`: the
/// host cwd when it is under a shared `$HOME`, else the guest home.
///
/// Never returns a path that may not exist in the guest — `container
/// exec -w` silently creates missing directories.
pub fn machine_workdir(
    detail: &MachineDetail,
    cwd: Option<&Path>,
    host_home: Option<&str>,
) -> Option<String> {
    let home_shared = detail.home_mount.as_deref() != Some("none");
    if let (true, Some(cwd), Some(home)) = (home_shared, cwd, host_home)
        && cwd.starts_with(home)
    {
        return Some(cwd.to_string_lossy().into_owned());
    }
    detail
        .user_setup
        .as_ref()
        .map(|u| format!("/home/{}", u.username))
}

/// Build `container exec` for an argv-exact command in a machine's
/// backing container — bypassing `machine run`'s `$SHELL -c "$*"`
/// re-evaluation (apple/container#1954) without any quoting.
///
/// Returns `None` when the inspect data lacks what's needed (no backing
/// container ID, or no provisioned user when `user` is unset); callers
/// fall back to [`run_command`] with [`ArgvMode::Exact`]. The machine
/// must already be running.
#[allow(clippy::too_many_arguments)]
pub fn machine_exec_command(
    detail: &MachineDetail,
    user: Option<&str>,
    cd: Option<&str>,
    env_vars: &GuestEnv,
    command: &[String],
    tty: bool,
    cwd: Option<&Path>,
    host_home: Option<&str>,
) -> Option<Command> {
    let cid = detail.container_id.as_deref()?;
    let mut cmd = container_cmd();
    cmd.args(["exec", "-i"]);
    if tty {
        cmd.arg("-t");
    }
    match user {
        Some(u) => {
            cmd.args(["--user", u]);
        }
        None => {
            let u = detail.user_setup.as_ref()?;
            cmd.arg("--user").arg(format!("{}:{}", u.uid, u.gid));
            for (k, v) in [
                ("HOME", format!("/home/{}", u.username)),
                ("USER", u.username.clone()),
                ("LOGNAME", u.username.clone()),
            ] {
                cmd.arg("--env").arg(format!("{k}={v}"));
            }
        }
    }
    // `~`/`~/x` name the effective user's guest home — same rules as
    // the distro path: the provisioned user's and root's are known,
    // anything else is looked up in the guest's /etc/passwd (the
    // backing container is running when this path is taken).
    let workdir = match cd {
        Some(d) if d == "~" || d.starts_with("~/") => {
            let provisioned_home = || {
                detail
                    .user_setup
                    .as_ref()
                    .map(|u| format!("/home/{}", u.username))
            };
            let h = match user.map(|u| u.split(':').next().unwrap_or(u)) {
                Some("root") => Some("/root".into()),
                Some(u) => Some(guest_passwd_home(cid, u).unwrap_or_else(
                    || match u.parse::<u32>() {
                        Ok(0) => "/root".into(),
                        Ok(uid) if detail.user_setup.as_ref().is_some_and(|s| s.uid == uid) => {
                            provisioned_home().unwrap_or_else(|| "/".into())
                        }
                        Ok(_) => "/".into(),
                        Err(_) => format!("/home/{u}"),
                    },
                )),
                None => provisioned_home(),
            };
            h.map(|h| match d.strip_prefix("~/") {
                Some(rest) => format!("{}/{rest}", h.trim_end_matches('/')),
                None => h,
            })
        }
        Some(d) => Some(d.to_string()),
        None => machine_workdir(detail, cwd, host_home),
    };
    if let Some(w) = workdir {
        cmd.args(["--workdir", &w]);
    }
    if let Ok(term) = env::var("TERM") {
        cmd.arg("--env").arg(format!("TERM={term}"));
    }
    env_vars.push_args(&mut cmd, "--env");
    cmd.arg(cid).args(command);
    Some(cmd)
}

/// `~USER`'s home inside a running container, from the sixth
/// `/etc/passwd` field — matching a name or a numeric uid. `getent`
/// is tried first (a distro may source users from NSS — LDAP, SSSD —
/// rather than the file); the `cat` fallback covers getent-less
/// images and users it somehow misses. `$1` keeps the name out of
/// the shell line. Queried live — a cached copy could go stale.
pub fn guest_passwd_home(id: &str, user: &str) -> Option<String> {
    let out = container_cmd()
        .args([
            "exec",
            id,
            "sh",
            "-c",
            "getent passwd \"$1\" 2>/dev/null || cat /etc/passwd",
            "sh",
            user,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let uid = user.parse::<u32>().ok();
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() >= 6 && (f[0] == user || uid.is_some() && uid == f[2].parse().ok()) {
            Some(f[5].to_string())
        } else {
            None
        }
    })
}

/// Quote `arg` for a POSIX shell: wrap in single quotes, escaping embedded
/// single quotes as `'\''`.
#[must_use]
pub fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// How a command line is handed to the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgvMode {
    /// Joined and evaluated by the guest shell (WSL `--` / bare command).
    Shell,
    /// Each argument delivered verbatim (WSL `-e`).
    Exact,
}

/// Build a `container machine run` command.
///
/// `executable` selects a specific program to run interactively (used for
/// `--shell-type standard`); `command` is a command line appended after
/// `--`. With neither, `container` opens the machine's login shell.
///
/// `machine run` always passes its arguments through the guest's
/// `$SHELL -c "$*"` (apple/container#1954), so [`ArgvMode::Exact`]
/// single-quotes each argument to make that evaluation a no-op. Drop the
/// quoting if upstream grows an argv-preserving mode.
///
/// `-i` is always passed: without it the guest sees EOF on stdin, so
/// piped input would silently vanish.
pub fn run_command(
    machine: Option<&str>,
    user: Option<&str>,
    cd: Option<&str>,
    env_vars: &GuestEnv,
    executable: Option<&str>,
    command: &[String],
    mode: ArgvMode,
) -> Command {
    let mut cmd = container_cmd();
    cmd.args(["machine", "run", "-i"]);
    if let Some(m) = machine {
        cmd.args(["-n", m]);
    }
    if let Some(u) = user {
        cmd.args(["-u", u]);
    }
    if let Some(d) = cd {
        cmd.args(["-w", d]);
    }
    env_vars.push_args(&mut cmd, "-e");
    if let Some(exe) = executable {
        cmd.arg("--").arg(shell_quote(exe));
    } else if !command.is_empty() {
        cmd.arg("--");
        match mode {
            ArgvMode::Shell => cmd.args(command),
            ArgvMode::Exact => cmd.args(command.iter().map(|a| shell_quote(a))),
        };
    }
    cmd
}

/// The shell probe run inside the machine to find the login shell.
const SHELL_PROBE: &str = r#"getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7"#;

/// Build the `machine run` command that evaluates [`SHELL_PROBE`].
///
/// `machine run` shell-evaluates its arguments joined into `"$*"`
/// (apple/container#1954), so the probe is one pre-joined string —
/// passing argv-style `["sh", "-c", "..."]` would be re-split and `-c`
/// would swallow only the first word.
fn shell_probe_command(machine: Option<&str>, user: Option<&str>) -> Command {
    let mut cmd = run_command(
        machine,
        user,
        None,
        &GuestEnv::default(),
        None,
        &[SHELL_PROBE.to_string()],
        ArgvMode::Shell,
    );
    cmd.stdin(Stdio::null());
    cmd
}

/// Resolve the login shell for the given user inside the machine.
///
/// Falls back to `/bin/sh` if the lookup fails for any reason.
pub fn resolve_shell(machine: Option<&str>, user: Option<&str>) -> String {
    let shell = shell_probe_command(machine, user)
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && s.starts_with('/') {
                Some(s)
            } else {
                None
            }
        });
    shell.unwrap_or_else(|| "/bin/sh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::process::Command;

    use super::super::test_support::detail;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn machine_exec_is_argv_exact() {
        let d = detail();
        let command = ["printf".to_string(), ":%s:".to_string(), "a b".to_string()];
        let env = GuestEnv {
            vars: vec!["FOO=bar".into()],
            files: vec!["/tmp/secrets.env".into()],
        };
        let cmd = machine_exec_command(
            &d,
            None,
            None,
            &env,
            &command,
            false,
            Some(Path::new("/tmp")),
            Some("/Users/daphnediane"),
        )
        .unwrap();
        let args = args_of(&cmd);
        let joined = args.join(" ");
        assert!(joined.starts_with("exec -i --user 1027:20 --env HOME=/home/daphnediane"));
        assert!(joined.contains("--workdir /home/daphnediane"));
        assert!(joined.contains("--env-file /tmp/secrets.env --env FOO=bar"));
        assert_eq!(
            &args[args.len() - 4..],
            ["alpine-730439", "printf", ":%s:", "a b"]
        );
    }

    #[test]
    fn machine_exec_needs_container_id() {
        let mut d = detail();
        d.container_id = None;
        assert!(
            machine_exec_command(
                &d,
                None,
                None,
                &GuestEnv::default(),
                &["ls".into()],
                false,
                None,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn run_command_builds_args() {
        let cmd = run_command(
            Some("dev"),
            Some("root"),
            Some("/tmp"),
            &GuestEnv {
                vars: vec!["FOO=bar".into()],
                files: vec!["x.env".into()],
            },
            None,
            &["ls".to_string(), "-la".to_string()],
            ArgvMode::Shell,
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "machine",
                "run",
                "-i",
                "-n",
                "dev",
                "-u",
                "root",
                "-w",
                "/tmp",
                "--env-file",
                "x.env",
                "-e",
                "FOO=bar",
                "--",
                "ls",
                "-la"
            ]
        );
    }

    #[test]
    fn run_command_bare_shell() {
        let cmd = run_command(
            None,
            None,
            None,
            &GuestEnv::default(),
            None,
            &[],
            ArgvMode::Shell,
        );
        assert_eq!(args_of(&cmd), ["machine", "run", "-i"]);
    }

    #[test]
    fn run_command_exact_quotes_each_arg() {
        let command = ["printf".to_string(), ":%s:".to_string(), "a b".to_string()];
        let cmd = run_command(
            None,
            None,
            None,
            &GuestEnv::default(),
            None,
            &command,
            ArgvMode::Exact,
        );
        assert_eq!(
            args_of(&cmd),
            ["machine", "run", "-i", "--", "'printf'", "':%s:'", "'a b'"]
        );
    }

    /// The probe must arrive as ONE argument — `machine run` joins and
    /// re-evals its args, so an argv-shaped probe would lose `-c`'s
    /// payload (apple/container#1954; this was the C12 bug).
    #[test]
    fn shell_probe_is_single_arg() {
        let cmd = shell_probe_command(Some("dev"), Some("root"));
        assert_eq!(
            args_of(&cmd),
            [
                "machine",
                "run",
                "-i",
                "-n",
                "dev",
                "-u",
                "root",
                "--",
                SHELL_PROBE
            ]
        );
    }

    #[test]
    fn shell_quote_escapes() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
