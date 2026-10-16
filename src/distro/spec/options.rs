/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Single-valued spec options: home mount, arch, automount, host user.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// How the macOS home directory is shared into a distro.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HomeMount {
    /// Read-write at the same path (the default, like `container machine`).
    #[default]
    Rw,
    /// Read-only.
    Ro,
    /// Not shared.
    None,
}

impl HomeMount {
    pub fn as_str(self) -> &'static str {
        match self {
            HomeMount::Rw => "rw",
            HomeMount::Ro => "ro",
            HomeMount::None => "none",
        }
    }

    /// The more restrictive of two modes: `none` < `ro` < `rw`.
    pub fn more_restrictive(self, other: HomeMount) -> HomeMount {
        let rank = |h: HomeMount| match h {
            HomeMount::None => 0,
            HomeMount::Ro => 1,
            HomeMount::Rw => 2,
        };
        if rank(self) <= rank(other) {
            self
        } else {
            other
        }
    }
}

impl FromStr for HomeMount {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "rw" => Ok(HomeMount::Rw),
            "ro" => Ok(HomeMount::Ro),
            "none" => Ok(HomeMount::None),
            _ => bail!("invalid home mount `{s}` (expected rw, ro, or none)"),
        }
    }
}

/// The architecture a distro's userspace is built for: `arm64` (the
/// Apple silicon host's own) or `amd64` (x86-64, run under Rosetta
/// translation — `container` enables it automatically when the
/// requested platform is amd64 on an arm64 host). Pinned at
/// create/import like `image` is: the journaled rootfs can't change
/// arch, so `set` has no flag for it — recreations re-apply the
/// recorded one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Arch {
    /// 64-bit ARM (`aarch64`) — native on Apple silicon.
    #[value(alias = "aarch64")]
    Arm64,
    /// x86-64 — Rosetta-translated on Apple silicon.
    #[value(alias = "x86_64")]
    Amd64,
}

impl Arch {
    /// The OCI spelling (`container`'s `--arch` value).
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::Arm64 => "arm64",
            Arch::Amd64 => "amd64",
        }
    }

    /// The host's own arch — what an unset `--arch` resolves to.
    pub fn host() -> Arch {
        crate::util::oci::host_arch().parse().unwrap_or(Arch::Arm64)
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Arch {
    type Err = anyhow::Error;
    /// `x86_64`/`aarch64` normalize to the OCI spellings — the forms
    /// `container inspect` platform fields and catalog entries use.
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "arm64" | "aarch64" => Ok(Arch::Arm64),
            "amd64" | "x86_64" => Ok(Arch::Amd64),
            _ => bail!("invalid architecture `{s}` (expected arm64 or amd64)"),
        }
    }
}

/// How a distro auto-manages `/Volumes` mounts — the same value space
/// as [`HomeMount`]. `Ro` mounts volumes read-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Automount {
    /// Not managed; `/Volumes` mounts are ordinary user mounts.
    #[default]
    None,
    /// Reconcile `/Volumes/<X>` → `/mnt/<x>` read-write.
    Rw,
    /// Reconcile `/Volumes/<X>` → `/mnt/<x>` read-only.
    Ro,
}

impl Automount {
    pub fn as_str(self) -> &'static str {
        match self {
            Automount::Rw => "rw",
            Automount::Ro => "ro",
            Automount::None => "none",
        }
    }

    /// Whether `/Volumes` mounts are auto-managed (anything but `none`).
    pub fn enabled(self) -> bool {
        self != Automount::None
    }

    /// The more restrictive of two modes: `none` < `ro` < `rw`.
    pub fn more_restrictive(self, other: Automount) -> Automount {
        let rank = |a: Automount| match a {
            Automount::None => 0,
            Automount::Ro => 1,
            Automount::Rw => 2,
        };
        if rank(self) <= rank(other) {
            self
        } else {
            other
        }
    }

    /// Whether automounts under this mode are read-only.
    pub fn read_only(self) -> bool {
        self == Automount::Ro
    }

    /// The equivalent home-mount access. An explicit `--automount`
    /// with no `--home-mount` defaults home to the same access — an
    /// `rw` volumes share is an `rw`-level host exposure anyway.
    pub fn home_mount(self) -> HomeMount {
        match self {
            Automount::Rw => HomeMount::Rw,
            Automount::Ro => HomeMount::Ro,
            Automount::None => HomeMount::None,
        }
    }
}

impl FromStr for Automount {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "rw" => Ok(Automount::Rw),
            "ro" => Ok(Automount::Ro),
            "none" => Ok(Automount::None),
            _ => bail!("invalid automount mode `{s}` (expected rw, ro, or none)"),
        }
    }
}

/// The host account mirrored into the guest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
}

impl HostUser {
    /// The guest home directory (a Linux home on the distro's own disk).
    pub fn guest_home(&self) -> String {
        format!("/home/{}", self.name)
    }

    pub(super) fn to_label(&self) -> String {
        format!("{}:{}:{}", self.name, self.uid, self.gid)
    }

    pub(super) fn from_label(s: &str) -> Result<Self> {
        let mut it = s.split(':');
        match (it.next(), it.next(), it.next(), it.next()) {
            (Some(name), Some(uid), Some(gid), None) => Ok(HostUser {
                name: name.to_string(),
                uid: uid.parse().context("bad uid in user label")?,
                gid: gid.parse().context("bad gid in user label")?,
            }),
            _ => bail!("malformed user label `{s}`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use super::super::changes::SpecChanges;
    use super::super::intent::Preset;
    use super::super::test_support::{ctx, legacy_journal, pinned_spec, resolved, spec};

    #[test]
    fn create_args_home_none_has_no_home_volume() {
        let mut s = spec();
        s.explicit.home_mount = Some(HomeMount::None);
        let joined = resolved(&s)
            .0
            .create_args(&ctx(), "id", "/s", "/Users/dp", &[])
            .join(" ");
        assert!(!joined.contains("/Users/dp:/Users/dp"));
    }

    #[test]
    fn guest_path_maps_mounts_and_home() {
        let (s, _) = resolved(&spec());
        let g = |p: &str| s.guest_path(Path::new(p), "/Users/dp");
        assert_eq!(g("/Volumes/X/repo/src").as_deref(), Some("/mnt/x/repo/src"));
        assert_eq!(g("/Volumes/X").as_deref(), Some("/mnt/x"));
        assert_eq!(g("/Users/dp/code").as_deref(), Some("/Users/dp/code"));
        assert_eq!(g("/Volumes/Xylophone"), None);
        assert_eq!(g("/tmp"), None);
        let mut none = spec();
        none.explicit.home_mount = Some(HomeMount::None);
        let (none, _) = resolved(&none);
        assert_eq!(
            none.guest_path(Path::new("/Users/dp/code"), "/Users/dp"),
            None
        );
    }

    /// `set --preset` re-derives what the user didn't pin; the pins
    /// survive.
    #[test]
    fn set_preset_rederives_unpinned() {
        let s = pinned_spec();
        let (new, _) = SpecChanges {
            preset: Some(Preset::Restricted),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(new.preset, Preset::Restricted);
        let (r, _) = resolved(&new);
        // Unpinned under restricted: automount off.
        assert_eq!(r.automount, Automount::None);
        // Pinned: survives the tier move — home stays ro, resources
        // keep their values.
        assert_eq!(r.home_mount, HomeMount::Ro);
        assert_eq!(r.cpus, Some(2));
        // No network pin → restricted's `none` derivation applies.
        assert_eq!(r.network.as_deref(), Some("none"));
        assert!(new.explicit.home_mount == Some(HomeMount::Ro));
        // Pinned ssh/admin survive the tier move — pins always win;
        // `--reset` is how they're unpinned.
        assert!(r.ssh && r.admin);
        assert!(new.explicit.ssh.is_some() && new.explicit.admin.is_some());
    }

    /// Home follows the *effective* automount — an explicit flag and
    /// a preset-supplied mode alike — and warns when the edge feeds
    /// the value.
    #[test]
    fn home_follows_effective_automount() {
        // Explicit --automount under the default preset: home
        // inherits rw, warned — the explicit home pin is dropped for
        // the probe.
        let mut s = spec();
        s.explicit.home_mount = None;
        s.explicit.automount = Some(Automount::Rw);
        let (r, warnings) = resolved(&s);
        assert_eq!(r.home_mount, HomeMount::Rw);
        assert!(warnings.iter().any(|w| w.contains("automount")));
        // The trusted preset supplies automount rw *and* home rw —
        // the edge doesn't run, so there's no warning.
        let mut s = spec();
        s.explicit.home_mount = None;
        s.preset = Preset::Trusted;
        let (r, warnings) = resolved(&s);
        assert_eq!(r.automount, Automount::Rw);
        assert_eq!(r.home_mount, HomeMount::Rw);
        assert!(warnings.is_empty(), "{warnings:?}");
        // Preset still outranks the edge: restricted + automount
        // keeps home locked down.
        let mut s = spec();
        s.explicit.home_mount = None;
        s.preset = Preset::Restricted;
        s.explicit.automount = Some(Automount::Rw);
        let (r, _) = resolved(&s);
        assert_eq!(r.home_mount, HomeMount::None);
    }

    /// A pre-preset journal is back-computed: restricted-looking →
    /// restricted with no pins; deviations pin as `explicit`.
    #[test]
    fn into_spec_back_computes_intent() {
        let mut j = legacy_journal(spec().to_journal());
        j.home_mount = HomeMount::None;
        j.mounts.clear();
        j.publish.clear();
        j.network = Some("none".into());
        j.ssh = false;
        j.admin = false;
        j.automount = Automount::None;
        j.cpus = None;
        j.memory = None;
        let (s, _) = j.into_spec();
        assert_eq!(s.preset, Preset::Restricted);
        assert!(s.explicit.is_empty(), "{:?}", s.explicit);

        // A default-ish spec with an ro home + agent reads as explicit
        // choices — not "restricted".
        let j = legacy_journal(spec().to_journal());
        let (s, _) = j.into_spec();
        assert_eq!(s.preset, Preset::Default);
        assert_eq!(s.explicit.home_mount, Some(HomeMount::Ro));
        assert_eq!(s.explicit.ssh, Some(true));
        // admin true is the derivation — not pinned; the recorded
        // resources are the old freeze, which counts as intent.
        assert_eq!(s.explicit.admin, None);
        assert_eq!(s.explicit.cpus, Some(2));
        // An old journal that ran automounts: the mode pins, and home
        // `none` pins too (measured against the pinned automount).
        let mut j = legacy_journal(spec().to_journal());
        j.automount = Automount::Rw;
        j.home_mount = HomeMount::None;
        j.ssh = false;
        let (s, _) = j.into_spec();
        assert_eq!(s.explicit.automount, Some(Automount::Rw));
        assert_eq!(s.explicit.home_mount, Some(HomeMount::None));
        // Idempotent: re-serializing and re-reading gives the same
        // spec and reports no drift.
        let (s2, notes) = s.to_journal().into_spec();
        assert_eq!(s2, s);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// Disabling automount leaves a user mount at an
    /// automount-looking target alone.
    #[test]
    fn automount_off_keeps_user_mounts() {
        let mut s = spec();
        s.mounts = vec!["/Volumes/X:/mnt/x".parse().unwrap()];
        s.explicit.automount = Some(Automount::Rw);
        let (s, _) = SpecChanges {
            automount: Some(Automount::None),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(resolved(&s).0.automount, Automount::None);
        assert_eq!(
            s.mounts.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["/Volumes/X:/mnt/x"]
        );
    }
}
