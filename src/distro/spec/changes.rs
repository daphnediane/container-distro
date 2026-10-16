/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `SpecChanges`: the edits `set` applies to a spec.

use super::DistroSpec;
use super::intent::{Intent, Preset, ResetTarget};
use super::options::{Automount, HomeMount};
use super::shares::{MountSpec, PublishSpec};
use super::wsl_conf::{ConfList, mode_from_flags};
use anyhow::{Result, bail};

/// Changes requested by `container distro set`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecChanges {
    /// Change the defaults tier: unpinned options re-derive, `explicit`
    /// pins survive (`--preset`, or the `--restricted`/`--trusted`
    /// aliases).
    pub preset: Option<Preset>,
    /// Un-pin an option back to inherited (`--reset home-mount`),
    /// clear a list (`--reset mounts`), or reset everything to a
    /// tier (`--reset restricted` — also clears every `explicit` pin).
    pub reset: Vec<ResetTarget>,
    pub cpus: Option<u64>,
    pub memory: Option<String>,
    pub home_mount: Option<HomeMount>,
    /// Set the `/Volumes` automount mode.
    pub automount: Option<Automount>,
    /// Replace the network attachment (`none` disables, `default`
    /// restores the usual one).
    pub network: Option<String>,
    /// Turn SSH-agent forwarding on or off.
    pub ssh: Option<bool>,
    /// Turn privilege (sudo/doas) provisioning on or off.
    pub admin: Option<bool>,
    /// Turn terminal-profile generation on or off — an explicit
    /// `--terminal-profile` also re-arms a profile the user deleted
    /// by hand.
    pub terminal_profile: Option<bool>,
    /// Turn host file access (the `distrofs` relay) on or off.
    pub files: Option<bool>,
    /// The `--use-wsl-conf` operand — a feature list that may carry
    /// `all`/`none` (bare `--use-wsl-conf` is `all`; `--no-wsl-conf`
    /// arrives here as `none`).
    pub wsl_conf: Option<ConfList>,
    /// The `--ignore-wsl-conf` operand — features to deny.
    pub wsl_conf_ignore: Option<ConfList>,
    pub add_mounts: Vec<MountSpec>,
    /// Guest paths of mounts to remove.
    pub remove_mounts: Vec<String>,
    pub add_publish: Vec<PublishSpec>,
    /// Host ports to unpublish.
    pub remove_publish: Vec<u16>,
}

impl SpecChanges {
    pub fn is_empty(&self) -> bool {
        *self == SpecChanges::default()
    }

    /// Apply to `spec` and re-resolve: preset and resets first (a
    /// `--reset <tier>` clears every `explicit` pin), then the flag
    /// pins land on top, then [`DistroSpec::resolve`] re-derives the
    /// unpinned options in dependency order — so `set --preset
    /// restricted` drops every un-pinned option to the locked tier
    /// while keeping the user's own flags. Returns the new spec and
    /// `resolve`'s warnings.
    ///
    /// Adding a mount at an existing target or a port on an existing
    /// host port replaces the old entry.
    pub fn apply(&self, spec: &DistroSpec) -> Result<(DistroSpec, Vec<String>)> {
        let mut s = spec.clone();

        // Tier selection. `--reset <tier>` clears every pin;
        // `--preset` keeps them. Both naming the same tier behaves as
        // a reset; differing tiers conflict.
        let mut tier = self.preset;
        let mut reset_all = false;
        for r in &self.reset {
            let Some(t) = r.tier() else { continue };
            match tier {
                Some(prev) if prev != t => {
                    bail!("conflicting presets: --preset {prev} vs --reset {t}")
                }
                _ => {
                    tier = Some(t);
                    // A tier named via --reset clears every pin.
                    reset_all = true;
                }
            }
        }
        if reset_all {
            s.explicit = Intent::default();
        }
        if let Some(t) = tier {
            s.preset = t;
        }

        // Per-option resets un-pin; list resets clear.
        for r in &self.reset {
            match r {
                ResetTarget::HomeMount => s.explicit.home_mount = None,
                ResetTarget::Automount => s.explicit.automount = None,
                ResetTarget::Ssh => s.explicit.ssh = None,
                ResetTarget::Sudo => s.explicit.admin = None,
                ResetTarget::Network => s.explicit.network = None,
                ResetTarget::WslConf => s.explicit.wsl_conf = None,
                ResetTarget::TerminalProfile => s.explicit.terminal_profile = None,
                ResetTarget::Files => s.explicit.files = None,
                ResetTarget::Cpus => s.explicit.cpus = None,
                ResetTarget::Memory => s.explicit.memory = None,
                ResetTarget::Mounts => s.mounts.clear(),
                ResetTarget::Publish => s.publish.clear(),
                _ => {}
            }
        }

        // Flags pin on top of whatever the tier left.
        if let Some(c) = self.cpus {
            s.explicit.cpus = Some(c);
        }
        if let Some(m) = &self.memory {
            s.explicit.memory = Some(m.clone());
        }
        if let Some(h) = self.home_mount {
            s.explicit.home_mount = Some(h);
        }
        if let Some(v) = self.automount {
            s.explicit.automount = Some(v);
        }
        if let Some(n) = &self.network {
            s.explicit.network = Some(n.clone());
        }
        if let Some(v) = self.ssh {
            s.explicit.ssh = Some(v);
        }
        if let Some(v) = self.admin {
            s.explicit.admin = Some(v);
            // An explicit `--sudo`/`--no-sudo` is a fresh decision, so
            // the grant env follows it.
            s.admin_grant = v;
        }
        if let Some(v) = self.terminal_profile {
            s.explicit.terminal_profile = Some(v);
        }
        if let Some(v) = self.files {
            s.explicit.files = Some(v);
        }
        // `--use`/`--ignore` fold into the stored mode expression —
        // `all`/`none` switch it, named features accumulate (use
        // allows, ignore denies); `Only` stays a pin, `AllExcept`
        // stays "everything but", `Default` stays tier-relative.
        if self.wsl_conf.is_some() || self.wsl_conf_ignore.is_some() {
            s.explicit.wsl_conf = mode_from_flags(
                self.wsl_conf.as_ref(),
                self.wsl_conf_ignore.as_ref(),
                s.explicit.wsl_conf.as_ref(),
            );
        }

        let (_, warnings) = s.resolve();

        for t in &self.remove_mounts {
            let before = s.mounts.len();
            s.mounts.retain(|m| &m.target != t);
            if s.mounts.len() == before {
                bail!("no mount at `{t}`");
            }
        }
        for m in &self.add_mounts {
            s.mounts.retain(|x| x.target != m.target);
            s.mounts.push(m.clone());
        }
        for p in &self.remove_publish {
            let before = s.publish.len();
            s.publish.retain(|x| x.host_port != *p);
            if s.publish.len() == before {
                bail!("no published host port {p}");
            }
        }
        for p in &self.add_publish {
            s.publish
                .retain(|x| !(x.host_port == p.host_port && x.proto == p.proto));
            s.publish.push(p.clone());
        }
        Ok((s, warnings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::{resolved, spec};

    #[test]
    fn set_changes_apply() {
        let changes = SpecChanges {
            cpus: Some(4),
            network: Some("none".into()),
            ssh: Some(false),
            admin: Some(false),
            add_mounts: vec!["/Volumes/Y:/mnt/x:ro".parse().unwrap()],
            remove_publish: vec![8080],
            add_publish: vec!["9000".parse().unwrap()],
            ..SpecChanges::default()
        };
        let (s, _) = changes.apply(&spec()).unwrap();
        let (r, _) = resolved(&s);
        assert_eq!(r.cpus, Some(4));
        assert_eq!(r.network.as_deref(), Some("none"));
        assert!(!r.ssh && !r.admin && !s.admin_grant);
        assert_eq!(s.mounts.len(), 1);
        assert_eq!(s.mounts[0].source, "/Volumes/Y");
        assert_eq!(s.publish.len(), 1);
        assert_eq!(s.publish[0].host_port, 9000);

        let bad = SpecChanges {
            remove_mounts: vec!["/nope".into()],
            ..SpecChanges::default()
        };
        assert!(bad.apply(&spec()).is_err());
        assert!(SpecChanges::default().is_empty());
    }
}
