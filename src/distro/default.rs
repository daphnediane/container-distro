/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The default distro marker.

use super::distro_name;
use super::journal::distro_exists;
use super::lock::{set_locked, unlock, write_nofollow};
use crate::container::{ContainerInfo, Machine, ensure_started};
use crate::naming::state_dir;
use anyhow::{Result, bail};
use std::fs;
use std::path::PathBuf;

fn default_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("default-distro"))
}

/// The default distro's name, if one is set.
pub fn default_name() -> Option<String> {
    let s = fs::read_to_string(default_file().ok()?).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub(super) fn write_default(name: Option<&str>) -> Result<()> {
    let path = default_file()?;
    match name {
        Some(n) => {
            fs::create_dir_all(state_dir()?)?;
            // Locked at rest like the init assets: this file picks which
            // distro a bare `cm` enters, so it shouldn't be guest-writable.
            unlock(&path);
            write_nofollow(&path, format!("{n}\n").as_bytes(), 0o644)?;
            set_locked(&path, true)?;
        }
        None => {
            unlock(&path);
            if path.exists() {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// Whether nothing is the default: no default distro among `distros` (a
/// stale one doesn't count) and no default machine.
pub(super) fn no_default(
    distro_default: Option<&str>,
    distros: &[ContainerInfo],
    machines: &[Machine],
) -> bool {
    let distro_set =
        distro_default.is_some_and(|d| distros.iter().any(|c| distro_name(c) == Some(d)));
    !distro_set && !machines.iter().any(|m| m.default)
}

pub fn set_default(name: Option<&str>) -> Result<()> {
    if let Some(n) = name {
        ensure_started()?;
        if !distro_exists(n)? {
            bail!("no distro named `{n}`");
        }
    }
    write_default(name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::container::{ContainerInfo, Machine};

    use crate::distro::spec::{Automount, HomeMount, Preset};

    use super::super::create::CreateOptions;
    use super::super::machine::reject_machine_opts;
    use super::super::test_support::distro_id;

    fn distro(name: &str) -> ContainerInfo {
        distro_id(&format!("{name}-a1b2c3"), name)
    }

    fn machine(id: &str, default: bool) -> Machine {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","status":"running","default":{default}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn no_default_rule() {
        let d = [distro("d1")];
        assert!(no_default(None, &d, &[]));
        assert!(no_default(None, &d, &[machine("m", false)]));
        assert!(!no_default(Some("d1"), &d, &[]));
        // A stale default (distro deleted) doesn't count.
        assert!(no_default(Some("gone"), &d, &[]));
        assert!(!no_default(Some("gone"), &d, &[machine("m", true)]));
        assert!(!no_default(None, &[], &[machine("m", true)]));
    }

    #[test]
    fn machine_opts_rejection() {
        // The set-style subset a machine can honor passes — resources,
        // home-mount, `--ssh`, and the tier flags.
        assert!(
            reject_machine_opts(&CreateOptions {
                cpus: Some(2),
                memory: Some("1G".into()),
                home_mount: Some(HomeMount::Ro),
                ssh: true,
                preset: Some(Preset::Restricted),
                no_boot: true,
                set_default: true,
                ..CreateOptions::default()
            })
            .is_ok()
        );
        // Options a machine can't honor are refused, named in the
        // error — including `--no-ssh`, a pin no machine can keep.
        let err = reject_machine_opts(&CreateOptions {
            volumes: vec!["/a:/b".parse().unwrap()],
            publish: vec!["8080".parse().unwrap()],
            network: Some("none".into()),
            no_ssh: true,
            sudo: true,
            ..CreateOptions::default()
        })
        .unwrap_err()
        .to_string();
        for f in [
            "--volume",
            "--publish",
            "--network",
            "--no-ssh",
            "--sudo/--no-sudo",
        ] {
            assert!(err.contains(f), "{err}");
        }
        // --automount (even bare) is refused.
        assert!(
            reject_machine_opts(&CreateOptions {
                automount: Some(Automount::Rw),
                ..CreateOptions::default()
            })
            .is_err()
        );
    }
}
