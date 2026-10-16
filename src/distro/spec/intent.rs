/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Defaults tiers and recorded intent: `Preset`, `Intent`, `ResetTarget`.

use super::options::{Automount, HomeMount};
use super::wsl_conf::{WslConf, WslConfMode};
use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The defaults tier a distro runs at: `--restricted` locks
/// everything down, `--trusted` opens the full WSL-style tier, and
/// `default` leaves the source-dependent defaults. `untrusted` is an
/// alias of `restricted`. Recorded per-distro (journal + label) so
/// `set --preset` can re-derive every option the user hasn't pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Preset {
    /// Source-dependent defaults: catalog installs get the curated
    /// (trusted-flavored) tier, everything else stays closed.
    #[default]
    Default,
    /// Locked down: no shares, no network, no agent, no privilege
    /// grant. Every hole needs its own explicit flag.
    #[value(alias = "untrusted")]
    Restricted,
    /// The mirror preset: rw home, rw `/Volumes` automount, agent on.
    Trusted,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Default => "default",
            Preset::Restricted => "restricted",
            Preset::Trusted => "trusted",
        }
    }

    /// What the preset implies for `home_mount` (`None` = unpinned).
    pub fn home_mount(self) -> Option<HomeMount> {
        match self {
            Preset::Restricted => Some(HomeMount::None),
            Preset::Trusted => Some(HomeMount::Rw),
            Preset::Default => None,
        }
    }

    /// What the preset implies for `automount`. `Restricted` pins
    /// `none` — not just "no implication": under the cap a `source`
    /// request of `rw` can't open a restricted distro's automounts
    /// (only an `explicit` flag can).
    pub fn automount(self) -> Option<Automount> {
        match self {
            Preset::Trusted => Some(Automount::Rw),
            Preset::Restricted => Some(Automount::None),
            Preset::Default => None,
        }
    }

    /// What the preset implies for `ssh`.
    pub fn ssh(self) -> Option<bool> {
        match self {
            Preset::Restricted => Some(false),
            Preset::Trusted => Some(true),
            Preset::Default => None,
        }
    }

    /// What the preset implies for `files` — `Restricted` pins it off:
    /// the relay is inert until the host connects, but mounting an
    /// untrusted rootfs puts its contents in front of Finder, Quick
    /// Look, and Spotlight.
    pub fn files(self) -> Option<bool> {
        match self {
            Preset::Restricted => Some(false),
            _ => None,
        }
    }

    /// What the preset implies for `admin`.
    pub fn admin(self) -> Option<bool> {
        match self {
            Preset::Restricted => Some(false),
            _ => None,
        }
    }

    /// What the preset implies for `network` — `Restricted` pins
    /// `none` (loopback only), the others leave it unpinned.
    pub fn network(self) -> Option<String> {
        match self {
            Preset::Restricted => Some("none".to_string()),
            _ => None,
        }
    }

    /// What the preset implies for `wsl_conf` — which `/etc/wsl.conf`
    /// features the distro honors. `Trusted` opts into everything
    /// (the image was already trusted to run code), `Restricted`
    /// contributes the empty set so a `source` ask can't lift it, and
    /// `Default` leaves it to `source` intent (catalog installs ask
    /// for all) or the empty built-in default.
    pub fn wsl_conf(self) -> Option<WslConf> {
        match self {
            Preset::Restricted => Some(WslConf::empty()),
            Preset::Trusted => Some(WslConf::ALL),
            Preset::Default => None,
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Preset {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "default" => Ok(Preset::Default),
            "restricted" | "untrusted" => Ok(Preset::Restricted),
            "trusted" => Ok(Preset::Trusted),
            _ => bail!("invalid preset `{s}` (expected default, restricted, or trusted)"),
        }
    }
}

/// Per-option intent for a distro — who asked for a value. Every field
/// is optional; absent means "inherits" (the option follows the
/// preset, its inheritance edge, the other intent, or the built-in
/// default). [`DistroSpec::explicit`] records the user's own flag
/// choices (create and `set`); [`DistroSpec::source`] records what the
/// distro's *source* asked for — a catalog entry's `HomeMount`
/// override, a machine's recorded settings on `migrate --in`, a
/// `.wsl` package's declared preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Intent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_mount: Option<HomeMount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automount: Option<Automount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<bool>,
    /// The flag token (`none`, `default`, or a network name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Which `/etc/wsl.conf` features to honor, as a *mode expression*
    /// rather than a resolved set: `explicit` records the shape the
    /// flags described (`--use-wsl-conf=hostname,fstab` pins
    /// `Only{…}`, `--ignore-wsl-conf=command` leaves `AllExcept{…}`),
    /// `source` records a package's ask (catalog installs use
    /// `AllExcept{}` — the curated set, whatever this build knows).
    /// Living modes (`AllExcept`, `Default`) follow feature growth and
    /// preset moves; `Only` is frozen at write time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wsl_conf: Option<WslConfMode>,
    /// Generate host terminal profiles (the Terminal.app settings set
    /// and iTerm2 dynamic profile). `source` records a package's
    /// `windowsterminal.enabled` ask; `explicit` records
    /// `--terminal-profile`/`--no-terminal-profile`. Absent inherits —
    /// the built-in default is `true` (WSL's own default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_profile: Option<bool>,
    /// Host file access (docs/design/host-file-access.md): publish the
    /// `distrofs` socket relay. `explicit` records `--files`/
    /// `--no-files`; absent inherits — off under `restricted`, else on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<bool>,
}

impl Intent {
    pub fn is_empty(&self) -> bool {
        *self == Intent::default()
    }

    /// An intent carrying only a home-mount request — the catalog
    /// entry's `HomeMount` override (or the catalog `rw` default).
    pub fn only_home_mount(h: HomeMount) -> Self {
        Intent {
            home_mount: Some(h),
            ..Intent::default()
        }
    }
}

/// What a `set --reset` target resets: one option back to inherited
/// (`--reset home-mount`), a list of mounts/ports back to empty, or
/// the whole distro to a defaults tier (`--reset restricted` clears
/// every `explicit` pin and sets the preset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResetTarget {
    /// Un-pin the home-mount mode
    HomeMount,
    /// Un-pin the /Volumes automount mode
    Automount,
    /// Un-pin SSH-agent forwarding
    Ssh,
    /// Un-pin the sudo/doas grant
    #[value(alias = "admin")]
    Sudo,
    /// Un-pin the network attachment
    Network,
    /// Un-pin the honored /etc/wsl.conf feature set
    WslConf,
    /// Un-pin terminal-profile generation
    TerminalProfile,
    /// Un-pin host file access
    Files,
    /// Un-pin the CPU count (back to the host-derived default)
    Cpus,
    /// Un-pin the memory size (back to the host-derived default)
    Memory,
    /// Remove all user mounts (automounts are recomputed anyway)
    Mounts,
    /// Un-publish every port
    Publish,
    /// Reset every option to the default tier
    Default,
    /// Reset every option to the restricted tier
    #[value(alias = "untrusted")]
    Restricted,
    /// Reset every option to the trusted tier
    Trusted,
}

impl ResetTarget {
    /// The preset this target resets to, when it names a tier.
    pub fn tier(self) -> Option<Preset> {
        match self {
            ResetTarget::Default => Some(Preset::Default),
            ResetTarget::Restricted => Some(Preset::Restricted),
            ResetTarget::Trusted => Some(Preset::Trusted),
            _ => None,
        }
    }
}

/// Inputs [`DistroSpec::resolve`] needs that aren't in the spec —
/// host-dependent defaults, so the spec stays pure. Built from
/// `default_resources()` in ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveCtx {
    /// CPUs when nothing configures a count.
    pub cpus: u64,
    /// Memory when nothing configures a size.
    pub memory: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::container::ContainerInfo;

    use super::super::DistroSpec;
    use super::super::changes::SpecChanges;
    use super::super::options::{Automount, HomeMount};
    use super::super::test_support::{ctx, pinned_spec, resolved, spec};
    use super::super::wsl_conf::WslConf;

    #[test]
    fn create_args_restricted() {
        let mut s = spec();
        s.explicit = Intent {
            home_mount: Some(HomeMount::None),
            network: Some("none".into()),
            ssh: Some(false),
            admin: Some(false),
            ..Intent::default()
        };
        s.mounts.clear();
        s.publish.clear();
        s.admin_grant = false;
        let (r, _) = resolved(&s);
        assert!(r.is_restricted());
        let joined = r
            .create_args(&ctx(), "id", "/s", "/Users/dp", &[])
            .join(" ");
        for want in [
            "--label io.github.daphnediane.container-distro.admin=false",
            "--label io.github.daphnediane.container-distro.home-mount=none",
            "--network none",
        ] {
            assert!(joined.contains(want), "missing `{want}` in `{joined}`");
        }
        for absent in ["--ssh", "--publish", "/Users/dp:/Users/dp"] {
            assert!(
                !joined.contains(absent),
                "unexpected `{absent}` in `{joined}`"
            );
        }
        assert!(!resolved(&spec()).0.is_restricted());
    }

    #[test]
    fn spec_round_trips_through_container_info() {
        let json = format!(
            r#"{{"configuration":{{"id":"d1",
              "labels":{{"io.github.daphnediane.container-distro.distro":"d1",
                        "io.github.daphnediane.container-distro.home-mount":"ro",
                        "io.github.daphnediane.container-distro.user":"dp:501:20",
                        "io.github.daphnediane.container-distro.admin":"true",
                        "io.github.daphnediane.container-distro.wsl-conf":"all",
                        "io.github.daphnediane.container-distro.preset":"trusted"}},
              "mounts":[{{"source":"/state/sbin.distro","destination":"/sbin.distro","options":["ro"]}},
                        {{"source":"/Users/dp","destination":"/Users/dp","options":["ro"]}},
                        {{"source":"/Volumes/X","destination":"/mnt/x","options":[]}}],
              "publishedPorts":[{{"hostAddress":"127.0.0.1","hostPort":8080,"containerPort":80,"proto":"tcp"}}],
              "networks":[{{"network":"default"}}],
              "ssh":true,
              "resources":{{"cpus":2,"memoryInBytes":{}}},
              "image":{{"reference":"alpine:latest"}}}},
              "status":{{"state":"stopped"}}}}"#,
            4u64 << 30
        );
        let info: ContainerInfo = serde_json::from_str(&json).unwrap();
        let back = DistroSpec::from_container(&info, "/Users/dp").unwrap();
        // The live values that differ from the tier's derivation pin
        // as `explicit` — home ro, automount none (the `/mnt/x` mount
        // lacks the automount options, so it's a user mount), and the
        // recorded resources (`4096M`, the container's own spelling).
        // The trusted preset supplies ssh and the wsl-conf set so
        // none of them pin.
        let want = Intent {
            home_mount: Some(HomeMount::Ro),
            automount: Some(Automount::None),
            cpus: Some(2),
            memory: Some("4096M".into()),
            ..Intent::default()
        };
        assert_eq!(back.explicit, want);
        assert_eq!(back.preset, Preset::Trusted);
        let (r, _) = resolved(&back);
        assert!(r.ssh && r.wsl_conf == WslConf::ALL);
        assert_eq!(
            back.mounts,
            spec().mounts,
            "the user mount survives; init/home/automount-derived don't"
        );
    }

    #[test]
    fn preset_round_trips_and_aliases() {
        for (s, want) in [
            ("default", Preset::Default),
            ("restricted", Preset::Restricted),
            ("untrusted", Preset::Restricted),
            ("trusted", Preset::Trusted),
        ] {
            let p: Preset = s.parse().unwrap();
            assert_eq!(p, want, "{s}");
        }
        assert!("bogus".parse::<Preset>().is_err());
        // Journals carry the canonical name.
        assert_eq!(
            serde_json::to_string(&Preset::Restricted).unwrap(),
            "\"restricted\""
        );
    }

    /// `--reset <option>` un-pins one option; it falls back to the
    /// derivation.
    #[test]
    fn reset_option_unpins() {
        let s = pinned_spec();
        let (new, _) = SpecChanges {
            reset: vec![ResetTarget::HomeMount],
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert!(new.explicit.home_mount.is_none());
        let (r, _) = resolved(&new);
        assert_eq!(r.home_mount, HomeMount::None);
        // Everything else stayed pinned.
        assert_eq!(r.cpus, Some(2));
        // The default attach.
        assert_eq!(r.network, None);
    }

    /// `--reset <tier>` clears every pin and sets the preset.
    #[test]
    fn reset_tier_clears_every_pin() {
        let s = pinned_spec();
        let (new, _) = SpecChanges {
            reset: vec![ResetTarget::Trusted],
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(new.preset, Preset::Trusted);
        assert!(new.explicit.is_empty());
        let (r, _) = resolved(&new);
        assert_eq!(r.home_mount, HomeMount::Rw);
        assert_eq!(r.automount, Automount::Rw);
        // Resources have no intent — the host (or `machine create`)
        // picks at materialize.
        assert_eq!(r.cpus, None);
        assert_eq!(r.memory, None);
    }

    /// A `--reset` tier conflicting with `--preset` is an error; the
    /// same tier twice resets.
    #[test]
    fn reset_tier_conflicts() {
        let err = SpecChanges {
            preset: Some(Preset::Trusted),
            reset: vec![ResetTarget::Restricted],
            ..SpecChanges::default()
        }
        .apply(&spec())
        .unwrap_err();
        assert!(err.to_string().contains("conflicting"), "{err}");

        let (new, _) = SpecChanges {
            preset: Some(Preset::Trusted),
            reset: vec![ResetTarget::Trusted],
            ..SpecChanges::default()
        }
        .apply(&pinned_spec())
        .unwrap();
        assert!(new.explicit.is_empty() && new.preset == Preset::Trusted);
    }

    /// `source` caps what the derivation raises: a catalog/package
    /// asking for less keeps it even under --trusted — but fills the
    /// slot when the derivation produced nothing (the catalog `rw`).
    #[test]
    fn source_caps_and_fills() {
        // Fill: no derivation → the catalog's rw request applies.
        let mut s = spec();
        s.explicit.home_mount = None;
        s.source.home_mount = Some(HomeMount::Rw);
        let (r, _) = resolved(&s);
        assert_eq!(r.home_mount, HomeMount::Rw);
        // Cap: trusted would give rw home, but the source asked for ro.
        let mut s = spec();
        s.explicit.home_mount = None;
        s.preset = Preset::Trusted;
        s.source.home_mount = Some(HomeMount::Ro);
        let (r, _) = resolved(&s);
        assert_eq!(r.home_mount, HomeMount::Ro);
        // And restricted's `none` can't be lifted by a source's rw.
        let mut s = spec();
        s.explicit.home_mount = None;
        s.preset = Preset::Restricted;
        s.source.home_mount = Some(HomeMount::Rw);
        let (r, _) = resolved(&s);
        assert_eq!(r.home_mount, HomeMount::None);
        // Explicit still beats every cap.
        let mut s = spec();
        s.preset = Preset::Trusted;
        s.source.home_mount = Some(HomeMount::Ro);
        s.explicit.home_mount = Some(HomeMount::None);
        let (r, _) = resolved(&s);
        assert_eq!(r.home_mount, HomeMount::None);
        // Resources describe, not cap: a machine's recorded cpus carry.
        let mut s = spec();
        s.explicit.cpus = None;
        s.source.cpus = Some(16);
        let (r, _) = resolved(&s);
        assert_eq!(r.cpus, Some(16));
    }

    /// A reset returns to `source` intent — the catalog's request
    /// resurfaces once the pin is gone.
    #[test]
    fn reset_falls_back_to_source() {
        let mut s = spec();
        s.source.home_mount = Some(HomeMount::Ro);
        s.explicit.home_mount = Some(HomeMount::None);
        assert_eq!(resolved(&s).0.home_mount, HomeMount::None);
        let (s, _) = SpecChanges {
            reset: vec![ResetTarget::HomeMount],
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(resolved(&s).0.home_mount, HomeMount::Ro);
    }

    /// `--reset mounts` clears the user list; automount intent is
    /// untouched — the derived shares come back at materialize.
    #[test]
    fn reset_mounts_clears_user_mounts() {
        let mut s = spec();
        s.explicit.automount = Some(Automount::Rw);
        let (s, _) = SpecChanges {
            reset: vec![ResetTarget::Mounts],
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert!(s.mounts.is_empty());
        assert_eq!(resolved(&s).0.automount, Automount::Rw);
    }

    /// `set` reports the tier move, the effective changes, and the
    /// pins still overriding the new tier.
    #[test]
    fn change_report_names_changes_and_pins() {
        let s = pinned_spec();
        let (new, _) = SpecChanges {
            preset: Some(Preset::Restricted),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        let report = new.change_report(&s);
        assert!(
            report.iter().any(|l| l == "preset: default → restricted"),
            "{report:?}"
        );
        // ssh is pinned `on` — it overrides the restricted tier.
        assert!(
            report.iter().any(|l| l.contains("ssh: on (explicit")),
            "{report:?}"
        );
        assert!(
            report.iter().any(|l| l.starts_with("network: ")),
            "{report:?}"
        );
    }
}
