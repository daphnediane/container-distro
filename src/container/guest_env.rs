/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Environment handed to a guest process: `--env` entries and
//! `--env-file` paths, plus the "is that a secret on argv?" nudge.

use std::io::IsTerminal;
use std::process::Command;

/// `--env`/`--env-file` arguments for `container exec` and `machine run`.
///
/// Both forms are resolved by the `container` CLI itself (`Parser.allEnv`
/// upstream): a bare `KEY` is looked up in *its* environment, and files
/// are read host-side — so neither puts a value on any argv. Only the
/// literal `KEY=VALUE` form does, which is what [`GuestEnv::warn_secrets`]
/// is about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestEnv {
    /// `KEY=VALUE`, or `KEY` to inherit from the host.
    pub vars: Vec<String>,
    /// Files of `KEY=VALUE` lines (`#` comments and blank lines ignored;
    /// a bare `KEY` line inherits). FIFOs work, so `<(...)` does too.
    pub files: Vec<String>,
}

impl GuestEnv {
    #[must_use]
    pub fn vars<I: IntoIterator<Item = S>, S: Into<String>>(vars: I) -> Self {
        Self {
            vars: vars.into_iter().map(Into::into).collect(),
            files: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty() && self.files.is_empty()
    }

    /// Append `--env-file`/`--env` (or `-e` for `machine run`) arguments.
    /// Order between the two is irrelevant: upstream merges files first,
    /// then `--env`, later wins per key.
    pub fn push_args(&self, cmd: &mut Command, env_flag: &str) {
        for f in &self.files {
            cmd.args(["--env-file", f]);
        }
        for e in &self.vars {
            cmd.args([env_flag, e]);
        }
    }

    /// Names of `KEY=VALUE` entries that look like credentials — see
    /// [`looks_secret`]. Bare `KEY` entries never ride argv, so they are
    /// not reported even when the name matches.
    #[must_use]
    pub fn secret_literals(&self) -> Vec<&str> {
        self.vars
            .iter()
            .filter_map(|e| e.split_once('='))
            .filter(|(k, v)| looks_secret(k, v))
            .map(|(k, _)| k)
            .collect()
    }

    /// One stderr line per invocation naming the suspicious entries
    /// (never their values) and the two argv-free alternatives.
    ///
    /// Interactive only: the heuristic has false positives and no
    /// opt-out, so a script's captured stderr shouldn't carry it —
    /// the command was almost certainly tried by hand first.
    pub fn warn_secrets(&self, prog: &str) {
        if !std::io::stderr().is_terminal() {
            return;
        }
        let names = self.secret_literals();
        if names.is_empty() {
            return;
        }
        eprintln!(
            "{prog}: warning: --env {}: the value is on the command line, visible to other users via `ps`; pass the bare name (--env {}) to inherit it, or use --env-file",
            names.join(", "),
            names[0]
        );
    }
}

/// Name components that mark a credential wherever they appear
/// (`DB_PASSWORD`, `MY_SECRET_X`) — matched as a whole `_`-separated
/// token or as its suffix (`DBPASSWORD`, `APIKEY`).
const SECRET_TOKENS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "SECRETS",
    "TOKEN",
    "CREDENTIAL",
    "CREDENTIALS",
    "CREDS",
    "APIKEY",
    "PRIVKEY",
];

/// Short tokens matched exactly only — `MONKEY`, `OAUTH2_URL`, and
/// `KEYBOARD` are not secrets.
const SECRET_EXACT: &[&str] = &["KEY", "AUTH", "PASS"];

/// A sibling token that says the variable names a *reference* to a
/// credential, not the credential: `AWS_ACCESS_KEY_ID`, `TOKEN_FILE`,
/// `SSH_KEY_PATH`, `OAUTH_URL`.
const REFERENCE_TOKENS: &[&str] = &[
    "ID", "FILE", "PATH", "DIR", "NAME", "URL", "URI", "HOST", "PORT", "TYPE", "ENV",
];

/// Value prefixes that are credentials regardless of the name: PEM
/// private keys and well-known token formats (GitHub, Slack, OpenAI,
/// GitLab, JWTs). AWS's `AKIA…` is the *public* access key ID, so it
/// is deliberately absent.
const SECRET_VALUE_PREFIXES: &[&str] = &[
    "-----BEGIN",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "sk-",
    "glpat-",
    "eyJ",
];

/// Heuristic: does a `KEY=VALUE` literal look like a credential? Tuned
/// to stay quiet for `TERM=xterm`, `PWD=/x`, `AWS_ACCESS_KEY_ID=…`,
/// and to speak up for `PASSWD=…`, `GITHUB_TOKEN=…`,
/// `AWS_SECRET_ACCESS_KEY=…`, or any name carrying a PEM block. Empty
/// values are never secrets.
#[must_use]
pub fn looks_secret(key: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if SECRET_VALUE_PREFIXES.iter().any(|p| value.starts_with(p)) {
        return true;
    }
    let upper = key.to_ascii_uppercase();
    let tokens: Vec<&str> = upper
        .split(['_', '-', '.'])
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.iter().any(|t| REFERENCE_TOKENS.contains(t)) {
        return false;
    }
    tokens
        .iter()
        .any(|t| SECRET_EXACT.contains(t) || SECRET_TOKENS.iter().any(|s| t == s || t.ends_with(s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn push_args_emits_files_then_vars() {
        let env = GuestEnv {
            vars: vec!["A=1".into(), "B".into()],
            files: vec!["/tmp/x.env".into()],
        };
        let mut cmd = Command::new("container");
        env.push_args(&mut cmd, "--env");
        assert_eq!(
            args_of(&cmd),
            ["--env-file", "/tmp/x.env", "--env", "A=1", "--env", "B"]
        );
        let mut cmd = Command::new("container");
        env.push_args(&mut cmd, "-e");
        assert_eq!(
            args_of(&cmd),
            ["--env-file", "/tmp/x.env", "-e", "A=1", "-e", "B"]
        );
    }

    #[test]
    fn secret_names() {
        for k in [
            "PASSWD",
            "PASSWORD",
            "DB_PASSWORD",
            "db_password",
            "DBPASSWORD",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "API_KEY",
            "APIKEY",
            "NPM_TOKEN",
            "AUTH",
            "PASS",
            "MY_CREDENTIALS",
            "SSH_PASSPHRASE",
        ] {
            assert!(looks_secret(k, "x"), "{k} should look secret");
        }
    }

    #[test]
    fn benign_names() {
        for k in [
            "TERM",
            "HOME",
            "DISPLAY",
            "MONKEY",
            "KEYBOARD",
            "AWS_ACCESS_KEY_ID",
            "TOKEN_FILE",
            "SSH_KEY_PATH",
            "OAUTH_URL",
            "PASSWORD_ENV",
            "SECRETS_DIR",
            "KEY_NAME",
            "AUTH_HOST",
            "PASSWORDLESS",
        ] {
            assert!(!looks_secret(k, "x"), "{k} should not look secret");
        }
    }

    #[test]
    fn secret_values_regardless_of_name() {
        assert!(looks_secret("FOO", "-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(looks_secret("GH", "ghp_abcdef"));
        assert!(looks_secret("JWT", "eyJhbGciOi"));
        assert!(!looks_secret("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"));
        assert!(!looks_secret("PASSWORD", ""));
    }

    #[test]
    fn literals_only_bare_names_exempt() {
        let env = GuestEnv::vars(["PASSWD=hunter2", "GITHUB_TOKEN", "TERM=xterm", "TOKEN=t"]);
        assert_eq!(env.secret_literals(), ["PASSWD", "TOKEN"]);
        assert!(
            GuestEnv::vars(["GITHUB_TOKEN"])
                .secret_literals()
                .is_empty()
        );
    }
}
