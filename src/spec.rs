/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! A distro's desired configuration, and its translation to and from
//! `container create` arguments / `container inspect` output.
//!
//! Everything here is pure, so the mapping can be unit-tested without a
//! running `container` service.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::container::ContainerInfo;
use crate::naming::{label_key, label_lookup};
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Label suffixes (full keys come from [`crate::naming::label_key`]).
///
/// Marks a container as a distro; the value is the distro name.
pub const LABEL_DISTRO: &str = "distro";
/// The home-directory mount mode (`rw`/`ro`/`none`).
pub const LABEL_HOME_MOUNT: &str = "home-mount";
/// The provisioned account as `name:uid:gid`.
pub const LABEL_USER: &str = "user";
/// Whether the provisioned account gets sudo/doas: `true` (armed — the
/// `CONTAINER_ADMIN` env is set), `false` (restricted assets mounted),
/// or `never` (the distro predates the grant env, so `init -u` never
/// re-creates privilege files an admin removed).
///
/// Not recoverable from `container inspect`, unlike `ssh` and
/// `networks`, so it persists as a label; absent means "never".
pub const LABEL_ADMIN: &str = "admin";
/// Guest path where the init assets are mounted.
pub const INIT_DIR: &str = "/sbin.distro";
/// Image label: where an `import`ed rootfs came from — a local path
/// (or `-` for stdin), or the URL for a catalog `.wsl` download. Set on
/// images `import` loads (alongside the `distro` label) so cleanup
/// removes them by ownership, not by name prefix.
pub const LABEL_IMPORTED_FROM: &str = "imported-from";
/// Image label: the verified SHA-256 of a catalog-downloaded `.wsl`
/// rootfs. Matches the cache filename (`<sha256>.wsl`), linking an
/// imported image back to its cached download.
pub const LABEL_ROOTFS_SHA256: &str = "rootfs-sha256";
/// Image label: the machine a `cm --import`-loaded rootfs image was
/// created for. Deliberately not `distro` — that label is what
/// `distro rm`/`set` treat as image-cleanup ownership, and machine
/// images are not distro images.
pub const LABEL_MACHINE: &str = "machine";
/// Container label on the short-lived scratch container `export_machine`
/// creates to hold a clone of a machine's rootfs for `container export`;
/// the value is the machine name. Marks leftovers if an export is
/// SIGKILLed mid-run (a normal exit deletes it via drop).
pub const LABEL_EXPORT_SCRATCH: &str = "export-scratch";
/// Whether the distro auto-manages `/Volumes` mounts, and how:
/// `rw`/`ro`/`none`. Not recoverable from `container inspect` — mounts
/// and volumes look alike — so it persists as a label.
pub const LABEL_AUTOMOUNT: &str = "automount";
/// The defaults tier the distro runs at — `default`/`restricted`/
/// `trusted`. The resolved options live in the spec and the other
/// labels; this records *why*, so a future `set --preset` — and any
/// option added later — can re-derive intent instead of re-guessing it.
pub const LABEL_PRESET: &str = "preset";
/// Which `/etc/wsl.conf` features the distro honors — `all`, `none`,
/// or a comma list of feature names. Not recoverable from
/// `container inspect` (env isn't exposed there), so it persists as
/// a label; absent means `none` — distros predating the feature keep
/// their old behavior until `set --use-wsl-conf` opts in.
pub const LABEL_WSL_CONF: &str = "wsl-conf";

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
        crate::oci::host_arch().parse().unwrap_or(Arch::Arm64)
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

/// The subset of `/etc/wsl.conf` features a distro honors — a bit set,
/// one bit per feature group. This is the *resolved* value: the intent
/// that produces it is a [`WslConfMode`] expression (see there), and
/// resolution meets the preset's implication and the source's ask at
/// *intersection* (a set's "more restrictive" is fewer features), with
/// the built-in default empty — honoring is opt-in.
///
/// Stored forms (journal `wslConf` field, `wsl-conf` label) serialize
/// as the expanded comma list of feature names — never the literal
/// `all`, which would silently grow as features are added. Parsing is
/// lenient: `all`/`none` are accepted, and unknown names are ignored
/// (a journal outliving a feature is not a read error). Display stays
/// terse — `all`/`none`/list — for `set` reports and drift lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WslConf(u8);

impl WslConf {
    /// `[network] hostname` — init writes it to `/etc/hostname`.
    pub const HOSTNAME: WslConf = WslConf(1);
    /// `[automount] mountFsTab` — `mount -a` for guest-local fstab
    /// entries at boot.
    pub const FSTAB: WslConf = WslConf(2);
    /// `[boot] systemd` / `boot.openrc` — pick the guest's init.
    pub const INIT: WslConf = WslConf(4);
    /// `[boot] command` — run image-supplied shell as root once per
    /// boot. The dangerous bit: it executes, unlike everything else.
    pub const COMMAND: WslConf = WslConf(8);
    /// `[automount] enabled=false` — lower automount/home-mount at
    /// import. Host-side only — it never reaches the guest env.
    pub const AUTOMOUNT: WslConf = WslConf(16);
    /// Every feature this build knows.
    pub const ALL: WslConf = WslConf(31);
    /// The features the guest init reads the file for — everything
    /// but the host-side import lowering.
    pub const GUEST: WslConf = WslConf(15);

    pub const fn empty() -> WslConf {
        WslConf(0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every bit in `other` is set here.
    pub fn contains(self, other: WslConf) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two sets — the `--use-wsl-conf` pin's operand.
    pub fn union(self, other: WslConf) -> WslConf {
        WslConf(self.0 | other.0)
    }

    /// The intersection — a set's `more_restrictive`: a `source` ask
    /// can only ever narrow what the preset implies.
    fn intersect(self, other: WslConf) -> WslConf {
        WslConf(self.0 & other.0)
    }

    /// `self` minus `other` — the `--ignore-wsl-conf` operand.
    pub fn difference(self, other: WslConf) -> WslConf {
        WslConf(self.0 & !other.0)
    }

    /// The `cap()` merge for sets: intersection.
    pub fn more_restrictive(self, other: WslConf) -> WslConf {
        self.intersect(other)
    }

    /// The expanded comma list of feature names — the stored spelling
    /// (journal, label). `none` for the empty set; `all` is never
    /// emitted, so a stored set can't silently grow when a feature is
    /// added.
    pub fn names_list(self) -> String {
        if self.is_empty() {
            return "none".to_string();
        }
        WSL_CONF_NAMES
            .iter()
            .filter(|(bit, _)| self.contains(*bit))
            .map(|(_, n)| *n)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Feature bits in display/parse order.
const WSL_CONF_NAMES: &[(WslConf, &str)] = &[
    (WslConf::HOSTNAME, "hostname"),
    (WslConf::FSTAB, "fstab"),
    (WslConf::INIT, "init"),
    (WslConf::COMMAND, "command"),
    (WslConf::AUTOMOUNT, "automount"),
];

impl fmt::Display for WslConf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("none");
        }
        if *self == WslConf::ALL {
            return f.write_str("all");
        }
        let names: Vec<&str> = WSL_CONF_NAMES
            .iter()
            .filter(|(bit, _)| self.contains(*bit))
            .map(|(_, n)| *n)
            .collect();
        f.write_str(&names.join(","))
    }
}

impl FromStr for WslConf {
    type Err = anyhow::Error;
    /// Storage-parse: lenient. `all`/`none` are accepted for hand
    /// edits and pre-release journals; unknown names are ignored — a
    /// feature added or removed later never breaks a stored set.
    /// Strict validation lives in [`ConfList`], the flag operand.
    fn from_str(s: &str) -> Result<Self> {
        let mut set = WslConf::empty();
        for tok in s.split(',') {
            match tok.trim() {
                "" | "none" => {}
                "all" => set = WslConf::ALL,
                name => {
                    if let Some((bit, _)) = WSL_CONF_NAMES.iter().find(|(_, n)| *n == name) {
                        set = set.union(*bit);
                    }
                }
            }
        }
        Ok(set)
    }
}

/// A feature name to its `WslConf` bit — unknown names contribute
/// nothing (storage-parse leniency for [`WslConfMode`] sets).
fn names_to_bits(names: &BTreeSet<String>) -> WslConf {
    names.iter().fold(WslConf::empty(), |set, name| {
        WSL_CONF_NAMES
            .iter()
            .find(|(_, n)| *n == name.as_str())
            .map(|(bit, _)| set.union(*bit))
            .unwrap_or(set)
    })
}

impl Serialize for WslConf {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.names_list())
    }
}

impl<'de> Deserialize<'de> for WslConf {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// How a distro's `/etc/wsl.conf` honoring is *expressed* — the intent
/// stored in `explicit`/`source`, as opposed to [`WslConf`], the
/// resolved feature set it produces. Keeping the expression (rather
/// than the resolved list) is what gives upgrade semantics a shape:
///
/// - `Only{allow}` is **frozen** — the features named are honored,
///   period; a feature added in a later build never appears.
/// - `AllExcept{deny}` is **living-minus** — everything this build
///   knows except `deny`; new features are honored automatically.
/// - `Default{allow,deny}` is **tier-relative** — `(tier set ∪ allow)
///   − deny`; it follows `set --preset` moves and tier growth, which
///   is what "the default, but with these adjustments" means.
///
/// `Unset` isn't a variant: absence (`None` on [`Intent::wsl_conf`])
/// is unset. Sets are name-keyed (`BTreeSet<String>`), so a stored
/// expression outlives feature additions *and* removals — unknown
/// names are ignored at resolve and kept in storage.
///
/// Serialized as a single comma list: `none` → `Only{}`, bare names →
/// `Only{…}`, `all` optionally followed by `-name` items →
/// `AllExcept{…}`, `+name`/`-name` items → `Default{…}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WslConfMode {
    /// `+allow/-deny` adjustments to the tier-derived set.
    Default {
        allow: BTreeSet<String>,
        deny: BTreeSet<String>,
    },
    /// Everything except `deny` — `all` (optionally `-deny` items).
    AllExcept { deny: BTreeSet<String> },
    /// Exactly `allow` — a bare name list (`none` when empty).
    Only { allow: BTreeSet<String> },
}

impl WslConfMode {
    /// `AllExcept{}` — "honor everything this build knows" (the
    /// catalog ask; serializes as `all`).
    pub fn all() -> Self {
        WslConfMode::AllExcept {
            deny: BTreeSet::new(),
        }
    }

    /// `Only{…}` covering exactly `set`'s features — the frozen pin
    /// back-fill produces.
    pub fn only(set: WslConf) -> Self {
        WslConfMode::Only {
            allow: WSL_CONF_NAMES
                .iter()
                .filter(|(bit, _)| set.contains(*bit))
                .map(|(_, n)| n.to_string())
                .collect(),
        }
    }

    /// The feature set this expression means against `base`, the
    /// tier-derived set (preset implication ∩ source ask). `Only` and
    /// `AllExcept` ignore `base`; `Default` adjusts it.
    pub fn resolve_with(&self, base: WslConf) -> WslConf {
        match self {
            WslConfMode::Default { allow, deny } => base
                .union(names_to_bits(allow))
                .difference(names_to_bits(deny)),
            WslConfMode::AllExcept { deny } => WslConf::ALL.difference(names_to_bits(deny)),
            WslConfMode::Only { allow } => names_to_bits(allow),
        }
    }

    /// Fold one flag-list name into the mode (`side` = the flag that
    /// carried it): use adds/allows, ignore denies/removes — the same
    /// per-mode processing regardless of where the mode came from.
    fn apply_name(&mut self, name: &str, side_use: bool) {
        match self {
            WslConfMode::Default { allow, deny } => {
                if side_use {
                    deny.remove(name);
                    allow.insert(name.to_string());
                } else {
                    allow.remove(name);
                    deny.insert(name.to_string());
                }
            }
            WslConfMode::AllExcept { deny } => {
                if side_use {
                    deny.remove(name);
                } else {
                    deny.insert(name.to_string());
                }
            }
            WslConfMode::Only { allow } => {
                if side_use {
                    allow.insert(name.to_string());
                } else {
                    allow.remove(name);
                }
            }
        }
    }
}

/// A `--use-wsl-conf`/`--ignore-wsl-conf` operand: a comma list of
/// feature names plus at most one `all`/`none` keyword. Strictly
/// validated — a typo'd feature name is a flag error, unlike the
/// lenient storage parse of stored sets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfList {
    /// Feature names in the list (never contains `all`/`none`).
    pub names: BTreeSet<String>,
    /// `Some(true)` when the list has `all`, `Some(false)` for `none`.
    pub keyword: Option<bool>,
}

impl ConfList {
    /// The `none` keyword list — what `--no-wsl-conf` lowers into.
    pub fn none() -> Self {
        ConfList {
            names: BTreeSet::new(),
            keyword: Some(false),
        }
    }
}

impl FromStr for ConfList {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let mut list = ConfList::default();
        for tok in s.split(',') {
            match tok.trim() {
                "" => {}
                "all" | "none" => {
                    let kw = tok.trim() == "all";
                    match list.keyword {
                        Some(k) if k != kw => {
                            bail!("`all` and `none` can't appear in the same list")
                        }
                        Some(_) => bail!("`{tok}` appeared twice"),
                        None => list.keyword = Some(kw),
                    }
                }
                name if WSL_CONF_NAMES.iter().any(|(_, n)| *n == name) => {
                    list.names.insert(name.to_string());
                }
                name => bail!(
                    "invalid wsl.conf feature `{name}` (expected hostname, \
                     fstab, init, command, automount, all, or none)"
                ),
            }
        }
        Ok(list)
    }
}

/// Compute the stored [`WslConfMode`] from the `--use-wsl-conf` /
/// `--ignore-wsl-conf` lists, folding into `existing` (the stored mode
/// when `set` adjusts one) when the lists don't switch the mode.
///
/// First the `all`/`none` keywords pick a fresh mode — `all` in
/// `--use` or `none` in `--ignore` → `AllExcept`, `none` in `--use` or
/// `all` in `--ignore` → `Only`. Otherwise a still-unset mode starts
/// as `Default` when both flags are present (an adjustment to whatever
/// would resolve), `Only` for `--use` alone, `AllExcept` for
/// `--ignore` alone; with a stored mode the flags just fold into it.
/// Named features then apply per mode: use allows/un-denies, ignore
/// denies/removes — an ignore mention wins a name listed in both.
pub fn mode_from_flags(
    use_list: Option<&ConfList>,
    ignore_list: Option<&ConfList>,
    existing: Option<&WslConfMode>,
) -> Option<WslConfMode> {
    if use_list.is_none() && ignore_list.is_none() {
        return existing.cloned();
    }
    let has_kw = |l: Option<&ConfList>, all: bool| l.is_some_and(|l| l.keyword == Some(all));
    let mut mode = if has_kw(use_list, true) || has_kw(ignore_list, false) {
        WslConfMode::all()
    } else if has_kw(use_list, false) || has_kw(ignore_list, true) {
        WslConfMode::Only {
            allow: BTreeSet::new(),
        }
    } else {
        existing
            .cloned()
            .unwrap_or_else(|| match (use_list.is_some(), ignore_list.is_some()) {
                (true, true) => WslConfMode::Default {
                    allow: BTreeSet::new(),
                    deny: BTreeSet::new(),
                },
                (true, false) => WslConfMode::Only {
                    allow: BTreeSet::new(),
                },
                (false, true) => WslConfMode::all(),
                (false, false) => unreachable!("a flag is Some"),
            })
    };
    if let Some(l) = use_list {
        for name in &l.names {
            mode.apply_name(name, true);
        }
    }
    if let Some(l) = ignore_list {
        for name in &l.names {
            mode.apply_name(name, false);
        }
    }
    // An empty Default resolves exactly like Unset and has no serial
    // form — normalize it away rather than store it.
    match mode {
        WslConfMode::Default {
            ref allow,
            ref deny,
        } if allow.is_empty() && deny.is_empty() => None,
        _ => Some(mode),
    }
}

impl fmt::Display for WslConfMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WslConfMode::Only { allow } if allow.is_empty() => f.write_str("none"),
            WslConfMode::Only { allow } => {
                f.write_str(&allow.iter().cloned().collect::<Vec<_>>().join(","))
            }
            WslConfMode::AllExcept { deny } => {
                f.write_str("all")?;
                for d in deny {
                    write!(f, ",-{d}")?;
                }
                Ok(())
            }
            WslConfMode::Default { allow, deny } => {
                let mut first = true;
                for (set, sign) in [(allow, "+"), (deny, "-")] {
                    for n in set {
                        if !first {
                            f.write_str(",")?;
                        }
                        first = false;
                        write!(f, "{sign}{n}")?;
                    }
                }
                if first {
                    // Empty Default resolves exactly like Unset — it
                    // has no serial form, so it must not be stored.
                    f.write_str("none")?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for WslConfMode {
    type Err = anyhow::Error;
    /// Storage-parse for journals/labels: the grammar Display emits.
    /// Unknown names are kept (round-trip fidelity); resolve ignores
    /// them. A base keyword (`all`/`none`) must lead; `+`/`-` prefixed
    /// names select `Default`, bare names `Only` — the two can't mix.
    fn from_str(s: &str) -> Result<Self> {
        let toks: Vec<&str> = s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        match toks.first() {
            Some(&"none") => {
                if toks.len() > 1 {
                    bail!("`none` can't combine with other tokens in `{s}`")
                }
                Ok(WslConfMode::Only {
                    allow: BTreeSet::new(),
                })
            }
            Some(&"all") => {
                let mut deny = BTreeSet::new();
                for t in &toks[1..] {
                    match t.strip_prefix('-').or_else(|| t.strip_prefix('+')) {
                        Some(n) if n == "all" || n == "none" => {
                            bail!("`{t}` can't repeat the base keyword in `{s}`")
                        }
                        Some(n) => {
                            if t.starts_with('-') {
                                deny.insert(n.to_string());
                            } else {
                                deny.remove(n);
                            }
                        }
                        None if *t == "all" || *t == "none" => {
                            bail!("`{t}` can't repeat in `{s}`")
                        }
                        // A bare name after `all` un-denies it — the
                        // flag rule is the same (use removes from deny).
                        None => {
                            deny.remove(*t);
                        }
                    }
                }
                Ok(WslConfMode::AllExcept { deny })
            }
            _ if toks.iter().any(|t| t.starts_with(['+', '-'])) => {
                let (mut allow, mut deny) = (BTreeSet::new(), BTreeSet::new());
                for t in toks {
                    match t.strip_prefix('+').or_else(|| t.strip_prefix('-')) {
                        Some(n) if t.starts_with('+') => {
                            deny.remove(n);
                            allow.insert(n.to_string());
                        }
                        Some(n) => {
                            allow.remove(n);
                            deny.insert(n.to_string());
                        }
                        None => bail!("`{t}`: bare names can't mix with +x/-x tokens"),
                    }
                }
                Ok(WslConfMode::Default { allow, deny })
            }
            _ => {
                let mut allow = BTreeSet::new();
                for t in toks {
                    match t {
                        "all" | "none" => bail!("`{t}` must be the first token in `{s}`"),
                        n => {
                            allow.insert(n.to_string());
                        }
                    }
                }
                Ok(WslConfMode::Only { allow })
            }
        }
    }
}

impl Serialize for WslConfMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for WslConfMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
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

    fn to_label(&self) -> String {
        format!("{}:{}:{}", self.name, self.uid, self.gid)
    }

    fn from_label(s: &str) -> Result<Self> {
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

/// Extra guest-side mount flags accepted in the `[:opts]` field of a
/// mount — the subset of vminitd's `mount(2)` option table meaningful
/// for a virtiofs share. Unknown flags would be passed through to the
/// filesystem's mount data and EINVAL the whole mount, so anything
/// outside this list is a parse error. These are *guest*-enforced:
/// `ro` is also applied host-side by VZ, but the rest can be remounted
/// away by guest root — defense-in-depth, not a boundary.
const MOUNT_OPTION_FLAGS: &[&str] = &[
    "suid",
    "nosuid",
    "dev",
    "nodev",
    "exec",
    "noexec",
    "atime",
    "noatime",
    "diratime",
    "nodiratime",
    "relatime",
    "norelatime",
    "strictatime",
    "nostrictatime",
    "sync",
    "async",
    "dirsync",
];

/// An extra host directory shared into the guest: `SRC:DST[:opt,…]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountSpec {
    pub source: String,
    pub target: String,
    pub read_only: bool,
    /// Extra guest-side mount flags (`nosuid`, `nodev`, …) beyond
    /// `ro`/`rw`. Absent in journals written before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

impl FromStr for MountSpec {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        let (source, target, opts) = match parts.as_slice() {
            [src, dst] => (*src, *dst, ""),
            [_, _, ""] => bail!("invalid mount `{s}`: empty options (expected SRC:DST[:opt,…])"),
            [src, dst, opts] => (*src, *dst, *opts),
            _ => bail!("invalid mount `{s}` (expected SRC:DST[:opt,…])"),
        };
        if !source.starts_with('/') || !target.starts_with('/') {
            bail!("invalid mount `{s}`: SRC and DST must be absolute paths");
        }
        let mut read_only = false;
        let mut rw = false;
        let mut options = Vec::new();
        for o in opts.split(',').filter(|o| !o.is_empty()) {
            match o {
                "ro" => read_only = true,
                "rw" => rw = true,
                _ if MOUNT_OPTION_FLAGS.contains(&o) => options.push(o.to_string()),
                _ => bail!("invalid mount option `{o}` in `{s}`"),
            }
        }
        if read_only && rw {
            bail!("invalid mount `{s}`: `ro` and `rw` conflict");
        }
        Ok(MountSpec {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
            options,
        })
    }
}

impl fmt::Display for MountSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.target)?;
        let mut opts: Vec<&str> = Vec::new();
        if self.read_only {
            opts.push("ro");
        }
        opts.extend(self.options.iter().map(String::as_str));
        if !opts.is_empty() {
            write!(f, ":{}", opts.join(","))?;
        }
        Ok(())
    }
}

/// A published port: `[HOST_IP:]HOST_PORT:GUEST_PORT[/PROTO]`.
///
/// Unlike `container`'s default, an omitted host IP means `127.0.0.1` —
/// WSL forwards to localhost, not to every interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishSpec {
    pub host_ip: String,
    pub host_port: u16,
    pub guest_port: u16,
    pub proto: String,
}

impl FromStr for PublishSpec {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let (ports, proto) = match s.split_once('/') {
            Some((p, proto @ ("tcp" | "udp"))) => (p, proto),
            Some(_) => bail!("invalid protocol in `{s}` (expected tcp or udp)"),
            None => (s, "tcp"),
        };
        let port = |p: &str| -> Result<u16> {
            match p.parse::<u16>() {
                Ok(0) | Err(_) => bail!("invalid port `{p}` in `{s}`"),
                Ok(n) => Ok(n),
            }
        };
        let parts: Vec<&str> = ports.rsplitn(3, ':').collect();
        let (host_ip, host_port, guest_port) = match parts.as_slice() {
            [g] => ("127.0.0.1", port(g)?, port(g)?),
            [g, h] => ("127.0.0.1", port(h)?, port(g)?),
            [g, h, ip] => {
                if ip.parse::<std::net::IpAddr>().is_err() {
                    bail!("invalid host IP `{ip}` in `{s}`");
                }
                (*ip, port(h)?, port(g)?)
            }
            _ => bail!("invalid publish spec `{s}`"),
        };
        Ok(PublishSpec {
            host_ip: host_ip.to_string(),
            host_port,
            guest_port,
            proto: proto.to_string(),
        })
    }
}

impl PublishSpec {
    /// Whether the host bind address is loopback (the default). Anything
    /// else — `0.0.0.0`, a LAN address — is reachable beyond this host,
    /// the one flag that changes exposure for other machines too.
    pub fn is_loopback(&self) -> bool {
        self.host_ip
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback())
    }
}

impl fmt::Display for PublishSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}/{}",
            self.host_ip, self.host_port, self.guest_port, self.proto
        )
    }
}

/// What a distro was created from, recorded in the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OriginSource {
    /// `create` / `cm --install` from an OCI image (`image` is the
    /// reference).
    Image,
    /// `import` / `cm --from-file` / `cm --import` of a local rootfs
    /// tar or `.wsl` package — the path as given (`-` for stdin).
    File { path: String },
    /// A catalog `.wsl` download: the URL, the SHA-256 the download
    /// was verified against, and the catalog entry's name. Only ever
    /// produced by catalog installs — the name is kept (rather than
    /// `CatalogDownload`) because existing journals serialize this
    /// variant and renaming would break them.
    Download {
        url: String,
        sha256: String,
        name: String,
    },
    /// A catalog image install: the entry's name. Recorded whether the
    /// catalog was built-in or a `--catalog FILE` — provenance, not a
    /// trust statement. The catalog file itself is not recorded: the
    /// trust decision was made at install time and the file can be
    /// replaced or moved afterwards, so its path would mean little.
    CatalogImage { name: String },
    /// `migrate --in` from a `container machine` of this name.
    Machine { name: String },
    /// `migrate` adopting a pre-journal distro container — its live
    /// configuration became the journal under explicit opt-in.
    Adopted,
}

/// Provenance written once at journal creation and never rewritten —
/// `set` clones it through untouched. Informational only: nothing keys
/// behavior off it. `created`/`created_with` record the *initial*
/// creation, not the last writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistroOrigin {
    /// ISO 8601 UTC creation time (matches `container`'s
    /// `creationDate` format).
    pub created: String,
    /// The container-distro version that created the distro.
    pub created_with: String,
    /// What the distro was made from.
    pub source: OriginSource,
}

/// A distro's recorded intent and state — what the journal models.
/// The *effective* option values live nowhere on this struct:
/// [`DistroSpec::resolve`] computes them from `preset` + `explicit` +
/// `source` on demand, so a derived value can never be *set* — change
/// the intent and the resolution follows automatically.
///
/// The serialized form is [`JournalSpec`], which additionally carries
/// the effective values at write time (a self-describing journal and
/// a compatibility record for pre-intent journals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistroSpec {
    pub name: String,
    pub image: String,
    /// The guest architecture the rootfs is built for, like `image`
    /// pinned at create: `amd64` runs under Rosetta. `None` is the
    /// host arch. Not `set`-table — a recreate swaps containers, never
    /// the filesystem's arch.
    pub arch: Option<Arch>,
    /// User-specified `--mount`-style shares. Automounts are *not*
    /// stored here — they're recomputed from `/Volumes` at
    /// materialize time.
    pub mounts: Vec<MountSpec>,
    pub publish: Vec<PublishSpec>,
    pub user: HostUser,
    /// The defaults tier this distro runs at. Pre-preset journals and
    /// containers re-derive it on load (see [`JournalSpec::into_spec`]).
    pub preset: Preset,
    /// Options the user explicitly picked (`--home-mount`, `set
    /// --ssh`, …). They win over the preset and every other intent,
    /// and survive `set --preset` re-derivation.
    pub explicit: Intent,
    /// What the distro's source asked for — a catalog entry's
    /// `HomeMount` override, a machine's settings on `migrate --in`.
    /// Lower precedence than `explicit`; a reset returns to it.
    pub source: Intent,
    /// Emit `CONTAINER_ADMIN=1` so `init -u` grants privileges at the
    /// next boot (label value `true` vs `never`). Kept separate from
    /// `admin`: a distro created before the admin label existed still
    /// allows privileges (`admin = true`, assets mounted) but is never
    /// armed, so `set` recreations won't re-grant sudoers an admin
    /// deliberately removed — only an explicit `set --sudo` does.
    pub admin_grant: bool,
    /// Where the distro came from — written once at creation. Absent on
    /// journals that predate it (and on specs reconstructed from a live
    /// container, which can't recover it).
    pub origin: Option<DistroOrigin>,
}

/// A `DistroSpec`'s options resolved to the values materialization
/// uses — the output of [`DistroSpec::resolve`]. Plain fields, no
/// intent: what you read is what the container gets. `cpus`/`memory`
/// keep the intent (`None` when neither explicit nor source set a
/// value): a distro container fills from `ResolveCtx`'s host defaults
/// at materialize (`create_args`), a `container machine`
/// lets `machine create` pick its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpec {
    pub name: String,
    pub image: String,
    /// The guest architecture — `spec.arch` or the host's.
    pub arch: Arch,
    pub user: HostUser,
    /// User mounts — automounts are computed at materialize, never
    /// stored or resolved.
    pub mounts: Vec<MountSpec>,
    pub publish: Vec<PublishSpec>,
    /// The journaled tier.
    pub preset: Preset,
    /// See `DistroSpec::admin_grant` — state, not derived.
    pub admin_grant: bool,
    /// Explicit-or-source CPUs.
    pub cpus: Option<u64>,
    /// Explicit-or-source memory (`container` size: `4G`, `2048M`).
    pub memory: Option<String>,
    pub home_mount: HomeMount,
    /// Whether `/Volumes` auto-manages mounts, and how.
    pub automount: Automount,
    /// Network to attach (`None` = `container`'s default; `"none"` =
    /// no interfaces beyond loopback).
    pub network: Option<String>,
    /// Forward the host SSH agent socket (`container create --ssh`).
    pub ssh: bool,
    /// Provision sudo/doas for the user (mounts `grant-admin.sh`).
    pub admin: bool,
    /// Which `/etc/wsl.conf` features the distro honors — empty means
    /// init never opens the file and no snapshot lands in the journal.
    pub wsl_conf: WslConf,
    /// Generate the host terminal profiles (`explicit` > `source`'s
    /// `windowsterminal.enabled` > `true`).
    pub terminal_profile: bool,
}

impl ResolvedSpec {
    /// Whether the distro is cut off from the host conveniences: no
    /// shares, no network, no agent, no privilege grant — what
    /// `--restricted` produces (explicit flags can reopen individual
    /// holes).
    pub fn is_restricted(&self) -> bool {
        self.home_mount == HomeMount::None
            && self.mounts.is_empty()
            && self.network.as_deref() == Some("none")
            && !self.ssh
            && !self.admin
    }
}

/// The serialized journal shape — the record `spec.json` keeps. It
/// carries the effective values *at write time* alongside the intent
/// that produced them: the intent is authoritative on read (a
/// mismatch between them is drift, reported by [`into_spec`]), and
/// for pre-intent journals the recorded values are the ground truth
/// the intent is back-computed from.
///
/// [`into_spec`]: JournalSpec::into_spec
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JournalSpec {
    pub name: String,
    pub image: String,
    /// The pinned guest arch; absent means the host's — and is how
    /// pre-arch journals read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<Arch>,
    pub cpus: Option<u64>,
    pub memory: Option<String>,
    pub home_mount: HomeMount,
    pub mounts: Vec<MountSpec>,
    #[serde(default)]
    pub automount: Automount,
    pub publish: Vec<PublishSpec>,
    pub network: Option<String>,
    pub ssh: bool,
    pub admin: bool,
    pub admin_grant: bool,
    /// The resolved honored-feature set at write time — absent on
    /// pre-feature journals reads as empty (opt in via `set`).
    #[serde(default)]
    pub wsl_conf: WslConf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    #[serde(default, skip_serializing_if = "Intent::is_empty")]
    pub explicit: Intent,
    #[serde(default, skip_serializing_if = "Intent::is_empty")]
    pub source: Intent,
    pub user: HostUser,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DistroOrigin>,
}

/// The effective values a journal recorded — the comparison bundle
/// for legacy back-fill and drift detection.
struct Observed {
    cpus: Option<u64>,
    memory: Option<String>,
    home_mount: HomeMount,
    automount: Automount,
    network: Option<String>,
    ssh: bool,
    admin: bool,
    wsl_conf: WslConf,
}

impl Observed {
    /// `"{field}: recorded X, resolves Y"` for every recorded value
    /// that doesn't match the current resolution — intent is
    /// authoritative, and the journal's copy is rewritten on the next
    /// change.
    fn drift(&self, r: &ResolvedSpec) -> Vec<String> {
        let mut lines = Vec::new();
        let mut diff = |name: &str, rec: String, res: String| {
            if rec != res {
                lines.push(format!(
                    "{name}: journal records {rec}, resolves {res} — the journal rewrites on the next change"
                ));
            }
        };
        diff(
            "home-mount",
            self.home_mount.as_str().into(),
            r.home_mount.as_str().into(),
        );
        diff(
            "automount",
            self.automount.as_str().into(),
            r.automount.as_str().into(),
        );
        let on = |b: bool| if b { "on" } else { "off" };
        diff("ssh", on(self.ssh).into(), on(r.ssh).into());
        diff("sudo", on(self.admin).into(), on(r.admin).into());
        diff(
            "wsl-conf",
            self.wsl_conf.to_string(),
            r.wsl_conf.to_string(),
        );
        diff(
            "network",
            self.network.clone().unwrap_or_else(|| "default".into()),
            r.network.clone().unwrap_or_else(|| "default".into()),
        );
        // `None` resolves to "the host picks" — still a recordable
        // value, so a mismatch drifts either way: a journal value
        // intent won't reproduce is as stale as a missing one.
        diff(
            "cpus",
            self.cpus
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            r.cpus.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
        );
        if !same_memory(self.memory.as_deref(), r.memory.as_deref()) {
            lines.push(format!(
                "memory: journal records {}, resolves {} — the journal rewrites on the next change",
                self.memory.as_deref().unwrap_or("-"),
                r.memory.as_deref().unwrap_or("-"),
            ));
        }
        lines
    }
}

impl JournalSpec {
    /// The recorded spec's intent-and-state half as a [`DistroSpec`]
    /// — before any legacy back-fill or drift check.
    fn base_spec(&self) -> DistroSpec {
        DistroSpec {
            name: self.name.clone(),
            image: self.image.clone(),
            arch: self.arch,
            mounts: self
                .mounts
                .iter()
                .filter(|m| !is_derived_automount(m))
                .cloned()
                .collect(),
            publish: self.publish.clone(),
            user: self.user.clone(),
            preset: self.preset.unwrap_or(Preset::Default),
            explicit: self.explicit.clone(),
            source: self.source.clone(),
            admin_grant: self.admin_grant,
            origin: self.origin.clone(),
        }
    }

    /// The recorded effective values as an [`Observed`] — the bundle
    /// legacy back-fill and drift detection compare against.
    fn observed(&self) -> Observed {
        Observed {
            cpus: self.cpus,
            memory: self.memory.clone(),
            home_mount: self.home_mount,
            automount: self.automount,
            network: self.network.clone(),
            ssh: self.ssh,
            admin: self.admin,
            wsl_conf: self.wsl_conf,
        }
    }

    /// Whether the recorded effective values *look* restricted — the
    /// pre-preset tier back-derivation.
    fn recorded_restricted(&self) -> bool {
        self.home_mount == HomeMount::None
            && self.mounts.is_empty()
            && self.network.as_deref() == Some("none")
            && !self.ssh
            && !self.admin
    }

    /// Deserialize a journal into the internal spec. Intent is
    /// authoritative: a journal carrying `preset` resolves through
    /// `explicit`/`source` alone, and every recorded effective value
    /// that doesn't match what that resolution produces is reported
    /// as drift — a semantics change (or a hand edit) the user should
    /// hear about rather than silently inherit. Pre-preset journals
    /// instead treat the recorded values as truth and back-compute
    /// the intent that reproduces them.
    ///
    /// Returns the spec plus drift/migration notes for the caller to
    /// surface.
    pub(crate) fn into_spec(self) -> (DistroSpec, Vec<String>) {
        let mut spec = self.base_spec();
        if self.preset.is_none() {
            spec.preset = if self.recorded_restricted() {
                Preset::Restricted
            } else {
                Preset::Default
            };
            spec.backfill_source(&self);
            spec.backfill_pins(&self.observed());
        }
        let (resolved, _) = spec.resolve();
        let notes = self
            .observed()
            .drift(&resolved)
            .into_iter()
            .map(|line| format!("distro `{}`: {line}", spec.name))
            .collect();
        (spec, notes)
    }
}

impl ResolvedSpec {
    /// Arguments for `container create`, mirroring how the machine plugin
    /// configures a machine: our init as the entrypoint, all capabilities,
    /// no masked or read-only paths, SSH agent forwarding, and the host
    /// account passed through the `CONTAINER_*` environment.
    /// The `container create` argv for this spec. `runtime_id` is the
    /// ephemeral container's id (`<name>-<suffix>`) — the distro's
    /// logical `name` only reaches the container through labels and
    /// `CONTAINER_MACHINE_ID` (which `init` writes to /etc/hostname).
    ///
    /// `automounts` is the `/Volumes` set computed at materialize
    /// time — it's appended after the user's own mounts and is never
    /// journaled. `ctx` fills the host defaults for resources intent
    /// left unset.
    pub fn create_args(
        &self,
        ctx: &ResolveCtx,
        runtime_id: &str,
        assets_dir: &str,
        host_home: &str,
        automounts: &[MountSpec],
    ) -> Vec<String> {
        let mut a: Vec<String> = ["create", "--name", runtime_id].map(String::from).to_vec();
        let mut push = |k: &str, v: String| {
            a.push(k.to_string());
            a.push(v);
        };
        push(
            "--label",
            format!("{}={}", label_key(LABEL_DISTRO), self.name),
        );
        push(
            "--label",
            format!(
                "{}={}",
                label_key(LABEL_HOME_MOUNT),
                self.home_mount.as_str()
            ),
        );
        push(
            "--label",
            format!("{}={}", label_key(LABEL_USER), self.user.to_label()),
        );
        push(
            "--label",
            format!(
                "{}={}",
                label_key(LABEL_ADMIN),
                if !self.admin {
                    "false"
                } else if self.admin_grant {
                    "true"
                } else {
                    "never"
                }
            ),
        );
        push(
            "--label",
            format!("{}={}", label_key(LABEL_AUTOMOUNT), self.automount.as_str()),
        );
        push(
            "--label",
            format!("{}={}", label_key(LABEL_PRESET), self.preset.as_str()),
        );
        push(
            "--label",
            format!(
                "{}={}",
                label_key(LABEL_WSL_CONF),
                self.wsl_conf.names_list()
            ),
        );
        push("--entrypoint", format!("{INIT_DIR}/init"));
        push("--volume", format!("{assets_dir}:{INIT_DIR}:ro"));
        match self.home_mount {
            HomeMount::Rw => push("--volume", format!("{host_home}:{host_home}")),
            HomeMount::Ro => push("--volume", format!("{host_home}:{host_home}:ro")),
            HomeMount::None => {}
        }
        for m in &self.mounts {
            push("--volume", m.to_string());
        }
        for m in automounts {
            push("--volume", m.to_string());
        }
        for p in &self.publish {
            push("--publish", p.to_string());
        }
        if let Some(n) = &self.network {
            push("--network", n.clone());
        }
        push("--cpus", self.cpus.unwrap_or(ctx.cpus).to_string());
        push(
            "--memory",
            self.memory.clone().unwrap_or_else(|| ctx.memory.clone()),
        );
        // `container` auto-enables Rosetta when the platform is amd64
        // on an arm64 host — no `--rosetta` needed (and it would error
        // on a non-arm64 host).
        push("--arch", self.arch.as_str().into());
        push("--cap-add", "ALL".into());
        push("--masked-path", "NONE".into());
        push("--read-only-path", "NONE".into());
        for (k, v) in [
            ("CONTAINER_MACHINE_ID", self.name.clone()),
            ("CONTAINER_USER", self.user.name.clone()),
            ("CONTAINER_UID", self.user.uid.to_string()),
            ("CONTAINER_GID", self.user.gid.to_string()),
            ("CONTAINER_HOME", self.user.guest_home()),
        ] {
            push("--env", format!("{k}={v}"));
        }
        if self.admin && self.admin_grant {
            push("--env", "CONTAINER_ADMIN=1".into());
        }
        // The honored guest-side features — `init` treats an absent or
        // empty list as "never open the file", so distros that opted
        // out emit nothing and read nothing.
        let guest_feats = self.wsl_conf.difference(WslConf::AUTOMOUNT);
        if !guest_feats.is_empty() {
            push("--env", format!("CONTAINER_WSL_CONF={guest_feats}"));
        }
        if self.ssh {
            a.push("--ssh".into());
        }
        a.push(self.image.clone());
        a
    }
}

impl DistroSpec {
    /// Reconstruct the spec of an existing distro container (for `set`,
    /// adoption, drift checks, `list`).
    ///
    /// The init-assets and home mounts are recognized by destination
    /// and dropped; canonical automounts (shape + options fingerprint)
    /// are dropped too — they're recomputed at materialize — and every
    /// other mount is a user mount.
    pub fn from_container(info: &ContainerInfo, host_home: &str) -> Result<Self> {
        let cfg = &info.configuration;
        let name = label_lookup(&cfg.labels, LABEL_DISTRO)
            .with_context(|| format!("`{}` is not a distro", cfg.id))?
            .clone();
        let user = HostUser::from_label(
            label_lookup(&cfg.labels, LABEL_USER)
                .with_context(|| format!("distro `{name}` has no user label"))?,
        )?;
        let home_mount =
            label_lookup(&cfg.labels, LABEL_HOME_MOUNT).map_or(Ok(HomeMount::Rw), |s| s.parse())?;
        // Only an explicit `true` re-arms the grant env: `never` and a
        // missing label (distros predating the label) keep `admin` —
        // the assets are mounted — but `init -u` won't re-create
        // privilege files an admin removed.
        let admin_label = label_lookup(&cfg.labels, LABEL_ADMIN);
        let admin = admin_label.is_none_or(|s| s != "false");
        let admin_grant = admin_label.is_some_and(|s| s == "true");
        // "true" is the pre-tri-state label form — treat it as `rw`.
        let automount =
            label_lookup(&cfg.labels, LABEL_AUTOMOUNT).map_or(Ok(Automount::None), |s| {
                if s == "true" {
                    Ok(Automount::Rw)
                } else {
                    s.parse()
                }
            })?;
        let preset = label_lookup(&cfg.labels, LABEL_PRESET).and_then(|s| s.parse::<Preset>().ok());
        // An absent label (a pre-feature distro) reads as the empty
        // set — matching the serde default on the journaled field.
        let wsl_conf = label_lookup(&cfg.labels, LABEL_WSL_CONF)
            .map(|s| s.parse::<WslConf>())
            .transpose()?
            .unwrap_or_default();
        let mounts = cfg
            .mounts
            .iter()
            .filter(|m| m.destination != INIT_DIR && m.destination != host_home)
            .map(|m| MountSpec {
                source: m.source.clone(),
                target: m.destination.clone(),
                read_only: m.read_only(),
                options: m
                    .options
                    .iter()
                    .filter(|o| o.as_str() != "ro" && o.as_str() != "rw")
                    .cloned()
                    .collect(),
            })
            // A canonical automount was derived state, not a user
            // mount — drop it; the scan re-adds it at materialize.
            .filter(|m| !is_derived_automount(m))
            .collect();
        let publish = cfg
            .published_ports
            .iter()
            .map(|p| PublishSpec {
                host_ip: p
                    .host_address
                    .clone()
                    .unwrap_or_else(|| "0.0.0.0".to_string()),
                host_port: p.host_port,
                guest_port: p.container_port,
                proto: p.proto.clone().unwrap_or_else(|| "tcp".to_string()),
            })
            .collect();
        // `container create` attaches the default network unless told
        // otherwise; an empty list is what `--network none` leaves. The
        // default attachment normalizes to `None` — the same as never
        // having passed `--network`.
        let network = match cfg.networks.first() {
            None => Some("none".to_string()),
            Some(n) if n.network == "default" => None,
            Some(n) => Some(n.network.clone()),
        };
        let resources = cfg.resources.as_ref();
        // The live configuration is the recorded truth — pins are
        // back-computed from it regardless of any preset label, since
        // labels can't carry `explicit`/`source` intent.
        let recorded = JournalSpec {
            name,
            image: cfg
                .image
                .as_ref()
                .map(|i| i.reference.clone())
                .unwrap_or_default(),
            // The running container's platform is the arch's recorded
            // truth — a spec reconstructed from it keeps it.
            arch: cfg
                .platform
                .as_ref()
                .map(|p| p.architecture.parse())
                .transpose()?,
            cpus: resources.and_then(|r| r.cpus),
            memory: resources
                .and_then(|r| r.memory_in_bytes)
                .map(|b| format!("{}M", b / (1024 * 1024))),
            home_mount,
            mounts,
            automount,
            publish,
            network,
            ssh: cfg.ssh,
            admin,
            admin_grant,
            wsl_conf,
            preset,
            explicit: Intent::default(),
            source: Intent::default(),
            user,
            // A live container can't tell us how the distro was made;
            // adoption stamps its own origin.
            origin: None,
        };
        let mut spec = recorded.base_spec();
        spec.preset = recorded.preset.unwrap_or({
            if recorded.recorded_restricted() {
                Preset::Restricted
            } else {
                Preset::Default
            }
        });
        spec.backfill_pins(&recorded.observed());
        Ok(spec)
    }
}

impl ResolvedSpec {
    /// Where host path `p` appears in the guest, if it is shared: under an
    /// extra mount (longest source wins) or the same-path home mount.
    pub fn guest_path(&self, p: &Path, host_home: &str) -> Option<String> {
        let best = self
            .mounts
            .iter()
            .filter_map(|m| {
                p.strip_prefix(&m.source).ok().map(|rest| {
                    let target = Path::new(&m.target);
                    let g = if rest.as_os_str().is_empty() {
                        target.to_path_buf()
                    } else {
                        target.join(rest)
                    };
                    (m.source.len(), g)
                })
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, g)| g);
        let path = best.or_else(|| {
            (self.home_mount != HomeMount::None && p.starts_with(host_home))
                .then(|| p.to_path_buf())
        })?;
        Some(path.to_string_lossy().into_owned())
    }
}

/// A network flag token as a stored value: `default` is the
/// container's own choice, recorded as `None` (the same as never
/// having passed `--network`).
fn normalize_network(token: &str) -> Option<String> {
    (token != "default").then(|| token.to_string())
}

/// A `container` size string (`4096M`, `8G`, `512KiB`, bare bytes) as
/// bytes — used to compare memory values whose spelling differs and
/// to render `list`'s MEMORY column. Unknown spellings → `None`.
pub fn size_bytes(s: &str) -> Option<u64> {
    let (num, unit) = s
        .trim()
        .find(|c: char| c.is_ascii_alphabetic())
        .map_or((s.trim(), ""), |i| s.trim().split_at(i));
    let mult = match unit {
        "" | "B" => 1,
        "K" | "KiB" => 1 << 10,
        "M" | "MiB" => 1 << 20,
        "G" | "GiB" => 1 << 30,
        "T" | "TiB" => 1 << 40,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// Whether two memory values name the same size — by bytes when both
/// parse, else by spelling.
fn same_memory(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => match (size_bytes(a), size_bytes(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
        _ => a == b,
    }
}

impl DistroSpec {
    /// Re-derive every unpinned option's effective value under the
    /// stored preset — the single definition of what each tier
    /// implies and which options inherit from others:
    ///
    /// **`explicit` > min(preset → inherit, `source`) > default**, in
    /// dependency order so a resolved value feeds the ones after it.
    /// `source` is a cap *and* a fallback: it lowers an exposure the
    /// preset or inheritance would raise, and supplies the value when
    /// they produce nothing (the catalog's `home-mount` request).
    ///
    /// | option      | preset                                  | inherits from            |
    /// |-------------|-----------------------------------------|--------------------------|
    /// | `automount` | trusted → `rw`, restricted → `none`     | —                        |
    /// | `home_mount`| restricted → `none`, trusted → `rw`     | effective `automount`    |
    /// | `ssh`       | restricted → off, trusted → on          | `home_mount == rw`       |
    /// | `admin`     | restricted → off                        | — (default on)           |
    /// | `network`   | restricted → `none`                     | — (default attach)       |
    /// | `cpus`/`mem`| —                                       | — (host-derived default) |
    ///
    /// Home inheriting the *effective* automount is the same-access
    /// rule: a distro sharing every volume already shares the volume
    /// home lives on, so the warning accompanies it. Preset still
    /// outranks the edge — `--restricted --automount rw` keeps home
    /// `none`. `cpus`/`memory` don't cap: a machine's recorded
    /// resources describe what it ran, not a maximum it asked for.
    ///
    /// Pure and context-free: intent is the spec's state; this
    /// returns the [`ResolvedSpec`] materialization reads plus
    /// warnings for the caller to show. `cpus`/`memory` stay
    /// intent-only — the host default is a materialization input
    /// (`create_args`/`to_journal` take the `ResolveCtx`), not part
    /// of the resolution.
    pub fn resolve(&self) -> (ResolvedSpec, Vec<String>) {
        let mut warnings = Vec::new();
        let preset = self.preset;

        let automount = self
            .explicit
            .automount
            .or_else(|| {
                cap(
                    preset.automount(),
                    self.source.automount,
                    Automount::more_restrictive,
                )
            })
            .unwrap_or_default();

        let derived_home = preset
            .home_mount()
            .or_else(|| automount.enabled().then(|| automount.home_mount()));
        let home_mount = self
            .explicit
            .home_mount
            .or_else(|| {
                cap(
                    derived_home,
                    self.source.home_mount,
                    HomeMount::more_restrictive,
                )
            })
            // The built-in default is closed — not `HomeMount`'s
            // `#[default]` (`rw` is only clap's flag spelling
            // default).
            .unwrap_or(HomeMount::None);
        if self.explicit.home_mount.is_none()
            && preset.home_mount().is_none()
            && automount.enabled()
        {
            warnings.push(format!(
                "warning: --automount {} shares the volume your home is on — \
                 home-mount resolved to {} (pass --home-mount to pin it)",
                automount.as_str(),
                home_mount.as_str()
            ));
        }

        // `false` is the more restrictive bool, so `cap` turns a
        // `source` ask of "off" into a floor even a raising preset
        // can't lift.
        let ssh = self
            .explicit
            .ssh
            .or_else(|| {
                cap(
                    preset.ssh().or(Some(home_mount == HomeMount::Rw)),
                    self.source.ssh,
                    std::cmp::min,
                )
            })
            .unwrap_or(false);

        let admin = self
            .explicit
            .admin
            .or_else(|| cap(preset.admin(), self.source.admin, std::cmp::min))
            .unwrap_or(true);

        // Config honoring: the trusted tier and catalog packages opt
        // into everything, `restricted` contributes the empty set so a
        // source ask can't lift it, and everything else stays out —
        // honoring is opt-in. The explicit pin is a mode expression
        // resolved against that tier base: `Only`/`AllExcept` ignore
        // it (absolute), `Default` adjusts it.
        let tier_base = cap(
            preset.wsl_conf(),
            self.source
                .wsl_conf
                .as_ref()
                .map(|m| m.resolve_with(WslConf::empty())),
            WslConf::more_restrictive,
        )
        .unwrap_or_default();
        let wsl_conf = self
            .explicit
            .wsl_conf
            .as_ref()
            .map(|m| m.resolve_with(tier_base))
            .unwrap_or(tier_base);

        let network = self
            .explicit
            .network
            .clone()
            .or_else(|| cap_network(preset.network(), self.source.network.clone()))
            .and_then(|t| normalize_network(&t));

        // Resources describe what ran, not a cap: the source's
        // recorded values are the default. `None` is meaningful —
        // a distro container fills the host's at materialize, a
        // `container machine` picks its own.
        let cpus = self.explicit.cpus.or(self.source.cpus);
        let memory = self
            .explicit
            .memory
            .clone()
            .or_else(|| self.source.memory.clone());

        // Host-side preference writes aren't a guest privilege, so the
        // preset doesn't gate them: flag > package ask > on.
        let terminal_profile = self
            .explicit
            .terminal_profile
            .or(self.source.terminal_profile)
            .unwrap_or(true);

        (
            ResolvedSpec {
                name: self.name.clone(),
                image: self.image.clone(),
                arch: self.arch.unwrap_or_else(Arch::host),
                user: self.user.clone(),
                mounts: self.mounts.clone(),
                publish: self.publish.clone(),
                preset: self.preset,
                admin_grant: self.admin_grant,
                cpus,
                memory,
                home_mount,
                automount,
                network,
                ssh,
                admin,
                wsl_conf,
                terminal_profile,
            },
            warnings,
        )
    }

    /// The resolution with no user pins — preset plus `source` plus
    /// inheritance plus defaults. The baseline `set`'s report
    /// measures pins against, and the legacy back-fill measures
    /// recorded values against.
    fn unpinned_resolved(&self) -> ResolvedSpec {
        let mut s = self.clone();
        s.explicit = Intent::default();
        s.resolve().0
    }

    /// Back-fill `source` intent from the recorded origin — the same
    /// mapping the create paths stamp: catalogs ask for rw home,
    /// machines carry their recorded settings. Legacy-journal only;
    /// a spec with `source` already returns unchanged.
    fn backfill_source(&mut self, recorded: &JournalSpec) {
        if !self.source.is_empty() {
            return;
        }
        match self.origin.as_ref().map(|o| &o.source) {
            Some(OriginSource::Download { .. }) | Some(OriginSource::CatalogImage { .. }) => {
                self.source.home_mount = Some(HomeMount::Rw);
            }
            Some(OriginSource::Machine { .. }) => {
                self.source.home_mount = Some(recorded.home_mount);
                self.source.cpus = recorded.cpus;
                self.source.memory = recorded.memory.clone();
            }
            _ => {}
        }
    }

    /// Pin every recorded effective value that the current intent
    /// wouldn't reproduce — the legacy back-computation. Each probe
    /// re-resolves with the pins found so far, in `resolve`'s
    /// dependency order, so `home` is measured against the *pinned*
    /// automount rather than the un-pinned one (a legacy
    /// `automount rw` + `home none` distro keeps its home pin —
    /// otherwise the next `set` would open home).
    fn backfill_pins(&mut self, obs: &Observed) {
        let derived_with = |pinned: &Intent| {
            let mut probe = self.clone();
            probe.explicit = pinned.clone();
            probe.resolve().0
        };
        let mut pinned = self.explicit.clone();
        if self.explicit.automount.is_none() && obs.automount != derived_with(&pinned).automount {
            pinned.automount = Some(obs.automount);
        }
        if self.explicit.home_mount.is_none() && obs.home_mount != derived_with(&pinned).home_mount
        {
            pinned.home_mount = Some(obs.home_mount);
        }
        if self.explicit.ssh.is_none() && obs.ssh != derived_with(&pinned).ssh {
            pinned.ssh = Some(obs.ssh);
        }
        if self.explicit.admin.is_none() && obs.admin != derived_with(&pinned).admin {
            pinned.admin = Some(obs.admin);
        }
        if self.explicit.network.is_none()
            && obs.network.as_deref().and_then(normalize_network) != derived_with(&pinned).network
        {
            // `None` (the default attach) pins as its flag token.
            pinned.network = Some(obs.network.clone().unwrap_or_else(|| "default".to_string()));
        }
        if self.explicit.wsl_conf.is_none() && obs.wsl_conf != derived_with(&pinned).wsl_conf {
            pinned.wsl_conf = Some(WslConfMode::only(obs.wsl_conf));
        }
        // Resources pin unconditionally: a pre-preset journal always
        // recorded the effective value — the freeze *is* the intent,
        // consistent with old versions pinning resources at create.
        if self.explicit.cpus.is_none() {
            pinned.cpus = obs.cpus;
        }
        if self.explicit.memory.is_none() {
            pinned.memory = obs.memory.clone();
        }
        self.explicit = pinned;
    }

    /// Serialize back to the journal shape: intent plus the resolved
    /// values at write time. `cpus`/`memory` record the *intent*
    /// (`None` = "the host picks") — not `ctx`'s host defaults: a
    /// host upgrade would turn a recorded default into phantom drift,
    /// and materialization applies the then-current default anyway.
    pub(crate) fn to_journal(&self) -> JournalSpec {
        let (r, _) = self.resolve();
        JournalSpec {
            name: self.name.clone(),
            image: self.image.clone(),
            arch: self.arch,
            cpus: r.cpus,
            memory: r.memory.clone(),
            home_mount: r.home_mount,
            mounts: self.mounts.clone(),
            automount: r.automount,
            publish: self.publish.clone(),
            network: r.network.clone(),
            ssh: r.ssh,
            admin: r.admin,
            admin_grant: self.admin_grant,
            wsl_conf: r.wsl_conf,
            preset: Some(self.preset),
            explicit: self.explicit.clone(),
            source: self.source.clone(),
            user: self.user.clone(),
            origin: self.origin.clone(),
        }
    }

    /// `set`'s after-action account: the tier move, every option
    /// whose effective value changed, and every `explicit` pin that
    /// keeps an option off the current tier's derivation.
    pub fn change_report(&self, old: &DistroSpec) -> Vec<String> {
        let mut lines = Vec::new();
        if self.preset != old.preset {
            lines.push(format!("preset: {} → {}", old.preset, self.preset));
        }
        let (new, _) = self.resolve();
        let (old_r, _) = old.resolve();
        let unpinned = self.unpinned_resolved();
        let mut change = |name: &str, o: String, n: String, unp: String, pinned: bool| {
            if o != n {
                lines.push(format!("{name}: {o} → {n}"));
            } else if pinned && n != unp {
                lines.push(format!("{name}: {n} (explicit — overrides preset)"));
            }
        };
        let on = |b: bool| if b { "on" } else { "off" };
        let net = |n: &Option<String>| n.clone().unwrap_or_else(|| "default".to_string());
        let res = |n: &Option<String>| n.clone().unwrap_or_else(|| "host".to_string());
        let cpu = |c: &Option<u64>| {
            c.map(|c| c.to_string())
                .unwrap_or_else(|| "host".to_string())
        };
        change(
            "home-mount",
            old_r.home_mount.as_str().into(),
            new.home_mount.as_str().into(),
            unpinned.home_mount.as_str().into(),
            self.explicit.home_mount.is_some(),
        );
        change(
            "automount",
            old_r.automount.as_str().into(),
            new.automount.as_str().into(),
            unpinned.automount.as_str().into(),
            self.explicit.automount.is_some(),
        );
        change(
            "ssh",
            on(old_r.ssh).into(),
            on(new.ssh).into(),
            on(unpinned.ssh).into(),
            self.explicit.ssh.is_some(),
        );
        change(
            "sudo",
            on(old_r.admin).into(),
            on(new.admin).into(),
            on(unpinned.admin).into(),
            self.explicit.admin.is_some(),
        );
        change(
            "wsl-conf",
            old_r.wsl_conf.to_string(),
            new.wsl_conf.to_string(),
            unpinned.wsl_conf.to_string(),
            self.explicit.wsl_conf.is_some(),
        );
        change(
            "network",
            net(&old_r.network),
            net(&new.network),
            net(&unpinned.network),
            self.explicit.network.is_some(),
        );
        change(
            "cpus",
            cpu(&old_r.cpus),
            cpu(&new.cpus),
            cpu(&unpinned.cpus),
            self.explicit.cpus.is_some(),
        );
        change(
            "memory",
            res(&old_r.memory),
            res(&new.memory),
            res(&unpinned.memory),
            self.explicit.memory.is_some(),
        );
        if old.mounts != self.mounts {
            lines.push(format!(
                "mounts: {} → {} entries",
                old.mounts.len(),
                self.mounts.len()
            ));
        }
        if old.publish != self.publish {
            lines.push(format!(
                "publish: {} → {} entries",
                old.publish.len(),
                self.publish.len()
            ));
        }
        lines
    }
}

/// The canonical automount target for a `/Volumes` entry name:
/// `/mnt/<name>` with spaces turned into `-` (WSL's `/mnt/<drive>`).
fn automount_target(name: &str) -> String {
    format!("/mnt/{}", name.replace(' ', "-"))
}

/// Whether `m` is a canonical automount: a `/Volumes/<X>` shared at
/// [`automount_target`] — the shape [`automounts`] emits, in either
/// mode. Only this shape is reconciled by `set --automount`; user
/// mounts at other targets are left alone.
pub fn is_automount(m: &MountSpec) -> bool {
    m.source
        .strip_prefix("/Volumes/")
        .is_some_and(|n| !n.is_empty() && !n.contains('/') && m.target == automount_target(n))
}

/// Whether `m` is a mount the automount scan produced: canonical
/// shape *and* the scan's option set (`nosuid,nodev`). The stricter
/// fingerprint keeps a user mount at an automount-looking target —
/// `/Volumes/X:/mnt/x` or `/Volumes/X:/mnt/x:ro` alone don't match —
/// so only entries indistinguishable from derived state are stripped
/// from `mounts` on read. (The accepted corner: a user mount with
/// exactly the scan's options is indistinguishable — with automount
/// enabled the scan recreates it identically anyway.)
pub fn is_derived_automount(m: &MountSpec) -> bool {
    is_automount(m) && m.options == ["nosuid".to_string(), "nodev".to_string()]
}

/// A `source` request caps the derivation for access options: when
/// both produce a value the more restrictive wins — a package or
/// catalog entry asking for less keeps it even under a raising preset
/// — and when the derivation produced nothing `source` fills the
/// slot (which is how a catalog's `home-mount` request survives the
/// `none` built-in default). The user's `explicit` pins are applied
/// by the caller, above all of this.
pub(crate) fn cap<T: Copy>(
    derived: Option<T>,
    source: Option<T>,
    more_restrictive: fn(T, T) -> T,
) -> Option<T> {
    match (derived, source) {
        (Some(d), Some(s)) => Some(more_restrictive(d, s)),
        (d, s) => d.or(s),
    }
}

/// `none` is the floor for network attachments — either side naming it
/// wins; otherwise `source`'s specific ask applies (a preset can only
/// ever produce `none`, so two differing attachments never meet).
fn cap_network(derived: Option<String>, source: Option<String>) -> Option<String> {
    match (derived, source) {
        (Some(d), Some(s)) => Some(if d == "none" || s == "none" {
            "none".to_string()
        } else {
            s
        }),
        (d, s) => d.or(s),
    }
}

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

/// Map `/Volumes` entries to `/mnt/<name>` mounts (WSL's `/mnt/<drive>`).
///
/// `entries` are `(name, is_symlink)` pairs; symlinks (the boot volume's
/// `Macintosh HD -> /`) are skipped, as are hidden names
/// (`.timemachine`) and `com.apple.*` — Apple-private mounts like the
/// Time Machine local-snapshots hierarchy, which Virtualization.framework
/// refuses to share (VZErrorDomain 2 / EPERM). Names keep their case —
/// the filesystem may be case-sensitive — with spaces turned into `-`.
/// When two volumes map to the same target (`My Photos` and
/// `My-Photos`), the alphabetically first wins and the rest are skipped
/// with a warning.
///
/// This is a pure name mapping; callers should also drop volumes whose
/// roots can't be enumerated on the host (e.g. TCC-protected Time
/// Machine destinations) — VZ refuses to share them.
///
/// `read_only` gives every mount `ro` (`automount ro`).
pub fn automounts(entries: &[(String, bool)], read_only: bool) -> Vec<MountSpec> {
    let mut names: Vec<&str> = entries
        .iter()
        .filter(|(name, is_link)| {
            !is_link && !name.starts_with('.') && !name.starts_with("com.apple.")
        })
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    let mut seen = std::collections::HashSet::new();
    let mut v = Vec::new();
    for name in names {
        let target = automount_target(name);
        if !seen.insert(target.clone()) {
            eprintln!("container-distro: skipping /Volumes/{name}: {target} already automounted");
            continue;
        }
        v.push(MountSpec {
            source: format!("/Volumes/{name}"),
            target,
            read_only,
            // External volumes never legitimately host suid binaries
            // or device nodes — free hardening from the guest flag set.
            options: vec!["nosuid".into(), "nodev".into()],
        });
    }
    v.sort_by(|a, b| a.target.cmp(&b.target));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> HostUser {
        HostUser {
            name: "dp".into(),
            uid: 501,
            gid: 20,
        }
    }

    /// An intent-only spec: the pins produce the resolved shape the
    /// old tests' effective values had — home ro, agent on, sudo on,
    /// 2 cpus/4G, `/Volumes/X` mounted, 8080 published.
    fn spec() -> DistroSpec {
        DistroSpec {
            name: "d1".into(),
            image: "alpine:latest".into(),
            arch: None,
            mounts: vec!["/Volumes/X:/mnt/x".parse().unwrap()],
            publish: vec!["8080:80".parse().unwrap()],
            user: user(),
            preset: Preset::Default,
            explicit: Intent {
                cpus: Some(2),
                memory: Some("4G".into()),
                home_mount: Some(HomeMount::Ro),
                ssh: Some(true),
                admin: Some(true),
                ..Intent::default()
            },
            source: Intent::default(),
            admin_grant: true,
            origin: None,
        }
    }

    /// `s.resolve()` — the resolved snapshot plus its warnings.
    fn resolved(s: &DistroSpec) -> (ResolvedSpec, Vec<String>) {
        s.resolve()
    }

    fn ctx() -> ResolveCtx {
        ResolveCtx {
            cpus: 4,
            memory: "8G".into(),
        }
    }

    #[test]
    fn mount_spec_parse() {
        let m: MountSpec = "/a:/b:ro".parse().unwrap();
        assert!(m.read_only);
        assert_eq!(m.to_string(), "/a:/b:ro");
        assert_eq!("/a:/b".parse::<MountSpec>().unwrap().to_string(), "/a:/b");
        for bad in ["/a", "a:/b", "/a:b", "/a:/b:xx", "/a:/b:"] {
            assert!(bad.parse::<MountSpec>().is_err(), "{bad}");
        }
    }

    #[test]
    fn mount_spec_options() {
        let m: MountSpec = "/a:/b:ro,nosuid,nodev".parse().unwrap();
        assert!(m.read_only);
        assert_eq!(m.options, ["nosuid", "nodev"]);
        assert_eq!(m.to_string(), "/a:/b:ro,nosuid,nodev");
        // Options without ro; `rw` is canonicalized away.
        let m: MountSpec = "/a:/b:noexec".parse().unwrap();
        assert!(!m.read_only);
        assert_eq!(m.to_string(), "/a:/b:noexec");
        let m: MountSpec = "/a:/b:rw,nosuid".parse().unwrap();
        assert!(!m.read_only);
        assert_eq!(m.options, ["nosuid"]);
        assert_eq!(m.to_string(), "/a:/b:nosuid");
        for bad in ["/a:/b:ro,rw", "/a:/b:bind", "/a:/b:remount", "/a:/b:foo"] {
            assert!(bad.parse::<MountSpec>().is_err(), "{bad}");
        }
    }

    #[test]
    fn mount_spec_serde_without_options() {
        // Journals predating `options` still parse.
        let m: MountSpec =
            serde_json::from_str(r#"{"source":"/a","target":"/b","readOnly":true}"#).unwrap();
        assert!(m.read_only && m.options.is_empty());
        // And the field is omitted again on the way out.
        let v = serde_json::to_value(&m).unwrap();
        assert!(v.get("options").is_none());
    }

    #[test]
    fn publish_spec_parse_defaults_to_localhost() {
        let p: PublishSpec = "8080".parse().unwrap();
        assert_eq!(p.to_string(), "127.0.0.1:8080:8080/tcp");
        let p: PublishSpec = "3000:80/udp".parse().unwrap();
        assert_eq!(p.to_string(), "127.0.0.1:3000:80/udp");
        let p: PublishSpec = "0.0.0.0:3000:80".parse().unwrap();
        assert_eq!(p.host_ip, "0.0.0.0");
        for bad in ["", "x", "0:1", "1:2/sctp", "a:1:2:3"] {
            assert!(bad.parse::<PublishSpec>().is_err(), "{bad}");
        }
    }

    #[test]
    fn publish_spec_loopback_detection() {
        for local in ["8080", "127.0.0.1:8080:80", "::1:8080:80"] {
            assert!(
                local.parse::<PublishSpec>().unwrap().is_loopback(),
                "{local}"
            );
        }
        for public in ["0.0.0.0:8080:80", "192.168.1.5:8080:80", ":::8080:80"] {
            assert!(
                !public.parse::<PublishSpec>().unwrap().is_loopback(),
                "{public}"
            );
        }
    }

    #[test]
    fn create_args_mirror_machine_config() {
        let a = resolved(&spec()).0.create_args(
            &ctx(),
            "d1-a1b2c3",
            "/state/sbin.distro",
            "/Users/dp",
            &[],
        );
        let joined = a.join(" ");
        // The runtime id names the container; the distro label and
        // CONTAINER_MACHINE_ID keep the logical name.
        assert!(joined.starts_with(
            "create --name d1-a1b2c3 --label io.github.daphnediane.container-distro.distro=d1"
        ));
        assert!(joined.contains("--env CONTAINER_MACHINE_ID=d1"));
        for want in [
            "--label io.github.daphnediane.container-distro.home-mount=ro",
            "--label io.github.daphnediane.container-distro.user=dp:501:20",
            "--label io.github.daphnediane.container-distro.admin=true",
            "--entrypoint /sbin.distro/init",
            "--volume /state/sbin.distro:/sbin.distro:ro",
            "--volume /Users/dp:/Users/dp:ro",
            "--volume /Volumes/X:/mnt/x",
            "--publish 127.0.0.1:8080:80/tcp",
            "--cpus 2 --memory 4G",
            "--cap-add ALL --masked-path NONE --read-only-path NONE",
            "--env CONTAINER_HOME=/home/dp",
        ] {
            assert!(joined.contains(want), "missing `{want}` in `{joined}`");
        }
        assert!(joined.ends_with("--ssh alpine:latest"));
    }

    /// A BTreeSet of names for mode construction in tests.
    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The wsl-conf label round-trips through create args and
    /// `from_container`, and the env gate carries the guest-side
    /// features only: the host-side `automount` bit never reaches the
    /// guest, and an empty set emits nothing — `init` reads absent as
    /// "never open the file". The label spells the resolved set as an
    /// expanded list, so a stored `all` can't silently grow.
    #[test]
    fn wsl_conf_label_and_env() {
        let mut s = spec();
        s.explicit.wsl_conf = Some(WslConfMode::all());
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::ALL);
        let joined = r
            .create_args(&ctx(), "id", "/s", "/Users/dp", &[])
            .join(" ");
        for want in [
            "container-distro.wsl-conf=hostname,fstab,init,command,automount",
            "--env CONTAINER_WSL_CONF=hostname,fstab,init,command",
        ] {
            assert!(joined.contains(want), "missing `{want}` in `{joined}`");
        }

        // spec()'s default derivation honors nothing: the label still
        // records it, and no env reaches the guest.
        let (r, _) = resolved(&spec());
        assert!(r.wsl_conf.is_empty());
        let joined = r.create_args(&ctx(), "id", "/s", "/h", &[]).join(" ");
        assert!(
            joined.contains("container-distro.wsl-conf=none"),
            "{joined}"
        );
        assert!(!joined.contains("CONTAINER_WSL_CONF"));
    }

    /// `explicit > preset ∩ source > ∅`: trusted opts into all,
    /// restricted intersects a catalog ask down to nothing, a `.wsl`
    /// package's `source` fills under `default`, and an explicit mode
    /// wins outright — `Only` pins, `Default` adjusts the tier.
    #[test]
    fn wsl_conf_resolves_by_tier_and_intent() {
        // `default` preset with no intent: honoring stays off.
        let (r, _) = resolved(&spec());
        assert!(r.wsl_conf.is_empty());
        // `trusted` opts into everything.
        let mut s = spec();
        s.preset = Preset::Trusted;
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::ALL);
        // `restricted` contributes ∅ — the catalog's ask can't lift it.
        let mut s = spec();
        s.preset = Preset::Restricted;
        s.source.wsl_conf = Some(WslConfMode::all());
        let (r, _) = resolved(&s);
        assert!(r.wsl_conf.is_empty());
        // A `.wsl` package's source intent opts in under `default`.
        let mut s = spec();
        s.source.wsl_conf = Some(WslConfMode::all());
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::ALL);
        // A narrowing source intersects what the preset implies.
        let mut s = spec();
        s.preset = Preset::Trusted;
        s.source.wsl_conf = Some(WslConfMode::only(WslConf::HOSTNAME.union(WslConf::INIT)));
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::HOSTNAME.union(WslConf::INIT));
        // An explicit pin beats the tier — a subset stays a subset.
        let mut s = spec();
        s.preset = Preset::Trusted;
        s.explicit.wsl_conf = Some(WslConfMode::only(WslConf::HOSTNAME));
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::HOSTNAME);
        // `Default` is tier-relative: deny `command` on trusted keeps
        // the rest; on `default` only the allows honor.
        let mode = WslConfMode::Default {
            allow: names(&["hostname"]),
            deny: names(&["command"]),
        };
        let mut s = spec();
        s.preset = Preset::Trusted;
        s.explicit.wsl_conf = Some(mode.clone());
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::ALL.difference(WslConf::COMMAND));
        let mut s = spec();
        s.explicit.wsl_conf = Some(mode);
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::HOSTNAME);
    }

    /// `set`'s use/ignore semantics fold into the stored mode: `use`
    /// alone pins `Only`, `ignore` alone leaves `AllExcept`, both make
    /// a tier-relative `Default`, `all`/`none` switch the mode, and
    /// named features accumulate against the stored expression.
    #[test]
    fn wsl_conf_set_use_and_ignore() {
        let list = |s: &str| Some(s.parse::<ConfList>().unwrap());
        let mut s = spec();
        s.preset = Preset::Trusted;
        // --use-wsl-conf=hostname,fstab pins exactly that set.
        let (out, _) = SpecChanges {
            wsl_conf: list("hostname,fstab"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::only(WslConf::HOSTNAME.union(WslConf::FSTAB)))
        );
        // --ignore-wsl-conf=command leaves "everything but".
        let (out, _) = SpecChanges {
            wsl_conf_ignore: list("command"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::AllExcept {
                deny: names(&["command"])
            })
        );
        assert_eq!(
            out.resolve().0.wsl_conf,
            WslConf::ALL.difference(WslConf::COMMAND)
        );
        // --use-wsl-conf=all --ignore-wsl-conf=command is the same
        // living-minus expression — it still picks up future features.
        let (out, _) = SpecChanges {
            wsl_conf: list("all"),
            wsl_conf_ignore: list("command"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::AllExcept {
                deny: names(&["command"])
            })
        );
        // Both flags with plain lists make a tier-relative Default.
        let (out, _) = SpecChanges {
            wsl_conf: list("hostname,fstab"),
            wsl_conf_ignore: list("command"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::Default {
                allow: names(&["hostname", "fstab"]),
                deny: names(&["command"]),
            })
        );
        // --ignore-wsl-conf=all is honoring's off switch.
        let (out, _) = SpecChanges {
            wsl_conf_ignore: list("all"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::Only {
                allow: BTreeSet::new()
            })
        );
        assert!(out.resolve().0.wsl_conf.is_empty());
        // A stored mode folds: use accumulates into Only, ignore
        // denies from AllExcept.
        s.explicit.wsl_conf = Some(WslConfMode::only(WslConf::HOSTNAME.union(WslConf::FSTAB)));
        let (out, _) = SpecChanges {
            wsl_conf_ignore: list("fstab"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::only(WslConf::HOSTNAME))
        );
        let (out, _) = SpecChanges {
            wsl_conf: list("init"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(
            out.explicit.wsl_conf,
            Some(WslConfMode::only(
                WslConf::HOSTNAME.union(WslConf::FSTAB).union(WslConf::INIT)
            ))
        );
        s.explicit.wsl_conf = Some(WslConfMode::AllExcept {
            deny: names(&["command"]),
        });
        let (out, _) = SpecChanges {
            wsl_conf: list("command"),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(out.explicit.wsl_conf, Some(WslConfMode::all()));
    }

    /// Pre-feature journals have no `wslConf` key — serde defaults it
    /// to the empty set, nothing pins honoring, and `set` is how they
    /// opt in.
    #[test]
    fn wsl_conf_defaults_off_in_old_journals() {
        let mut j = legacy_journal(spec().to_journal());
        j.wsl_conf = WslConf::empty();
        let (s, _) = j.into_spec();
        let (r, _) = resolved(&s);
        assert!(r.wsl_conf.is_empty());
        let (s, _) = SpecChanges {
            wsl_conf: Some("all".parse().unwrap()),
            ..SpecChanges::default()
        }
        .apply(&s)
        .unwrap();
        assert_eq!(s.explicit.wsl_conf, Some(WslConfMode::all()));
        let (r, _) = resolved(&s);
        assert_eq!(r.wsl_conf, WslConf::ALL);
    }

    /// Resolved sets serialize expanded (never `all`) and parse
    /// leniently — unknown names drop, `all`/`none` still read.
    #[test]
    fn wsl_conf_parses_and_displays() {
        assert_eq!("all".parse::<WslConf>().unwrap(), WslConf::ALL);
        assert_eq!("none".parse::<WslConf>().unwrap(), WslConf::empty());
        assert_eq!("".parse::<WslConf>().unwrap(), WslConf::empty());
        let set = "hostname, init".parse::<WslConf>().unwrap();
        assert_eq!(set, WslConf::HOSTNAME.union(WslConf::INIT));
        assert_eq!(set.to_string(), "hostname,init");
        assert_eq!(WslConf::ALL.to_string(), "all");
        assert_eq!(WslConf::empty().to_string(), "none");
        // Stored sets tolerate features this build no longer knows.
        assert_eq!(
            "hostname,bogus".parse::<WslConf>().unwrap(),
            WslConf::HOSTNAME
        );
        // …and serialize as the expanded list.
        assert_eq!(
            serde_json::to_string(&WslConf::ALL).unwrap(),
            "\"hostname,fstab,init,command,automount\""
        );
    }

    /// Mode expressions round-trip through their string grammar, and
    /// the resolution math is as documented: `Only` frozen,
    /// `AllExcept` living-minus, `Default` tier-relative.
    #[test]
    fn wsl_conf_mode_parses_displays_resolves() {
        let mode = |s: &str| s.parse::<WslConfMode>().unwrap();
        // Round-trips.
        for (text, want) in [
            ("all", WslConfMode::all()),
            (
                "all,-command",
                WslConfMode::AllExcept {
                    deny: names(&["command"]),
                },
            ),
            (
                // canonical order is the BTreeSet's (alphabetical)
                "fstab,hostname",
                WslConfMode::Only {
                    allow: names(&["hostname", "fstab"]),
                },
            ),
            (
                "none",
                WslConfMode::Only {
                    allow: BTreeSet::new(),
                },
            ),
            (
                "+hostname,-command",
                WslConfMode::Default {
                    allow: names(&["hostname"]),
                    deny: names(&["command"]),
                },
            ),
            (
                "-command",
                WslConfMode::Default {
                    allow: BTreeSet::new(),
                    deny: names(&["command"]),
                },
            ),
        ] {
            let m = mode(text);
            assert_eq!(m, want, "{text}");
            assert_eq!(m.to_string(), text, "{text} re-serializes");
        }
        // Malformed: mid-list base, none+more, mixed bare/signed.
        for bad in ["x,all", "none,hostname", "hostname,-fstab", "all,all"] {
            assert!(bad.parse::<WslConfMode>().is_err(), "{bad}");
        }
        // Unknown names keep their place but resolve to nothing.
        let m = mode("hostname,bogus");
        assert_eq!(m.resolve_with(WslConf::empty()), WslConf::HOSTNAME);
        // Resolution math.
        assert_eq!(
            mode("all,-command").resolve_with(WslConf::empty()),
            WslConf::ALL.difference(WslConf::COMMAND)
        );
        assert_eq!(
            mode("+hostname,-command").resolve_with(WslConf::ALL),
            WslConf::ALL.difference(WslConf::COMMAND)
        );
        assert_eq!(
            mode("+hostname,-command").resolve_with(WslConf::empty()),
            WslConf::HOSTNAME
        );
        assert_eq!(
            mode("hostname").resolve_with(WslConf::ALL),
            WslConf::HOSTNAME
        );
    }

    /// The flag operand is strict: unknown names and contradictory
    /// keywords are parse errors.
    #[test]
    fn conf_list_parses() {
        let l = "hostname, fstab".parse::<ConfList>().unwrap();
        assert_eq!(l.names, names(&["hostname", "fstab"]));
        assert_eq!(l.keyword, None);
        assert_eq!("all".parse::<ConfList>().unwrap().keyword, Some(true));
        assert_eq!("none".parse::<ConfList>().unwrap().keyword, Some(false));
        assert!("hostname,bogus".parse::<ConfList>().is_err());
        assert!("all,none".parse::<ConfList>().is_err());
        assert!("all,all".parse::<ConfList>().is_err());
    }

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

    /// The spec's journal form round-trips: serialize `to_journal`,
    /// re-read through `into_spec`, and the intent comes back
    /// identical with no drift notes.
    #[test]
    fn spec_serializes_for_the_set_journal() {
        let s = spec();
        let journal = s.to_journal();
        let round: JournalSpec =
            serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert_eq!(round, journal);
        let (back, notes) = round.into_spec();
        assert_eq!(back, s);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// `origin` round-trips through the journal JSON and is omitted
    /// entirely when absent — journals written before it existed read
    /// back with `None`.
    #[test]
    fn origin_round_trip_and_default() {
        let mut s = spec();
        s.origin = Some(DistroOrigin {
            created: "2026-10-10T05:00:00Z".into(),
            created_with: "0.5.0-dev".into(),
            source: OriginSource::Download {
                url: "https://example.com/d.wsl".into(),
                sha256: "ab12".into(),
                name: "Catalog-9".into(),
            },
        });
        let json = serde_json::to_vec(&s.to_journal()).unwrap();
        let round: JournalSpec = serde_json::from_slice(&json).unwrap();
        assert_eq!(round.into_spec().0, s);
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["origin"]["source"]["type"], "download");
        assert_eq!(v["origin"]["source"]["sha256"], "ab12");
        assert_eq!(v["origin"]["createdWith"], "0.5.0-dev");
        // No origin → no key at all, and it still parses.
        let bare = serde_json::to_vec(&spec().to_journal()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bare).unwrap();
        assert!(v.get("origin").is_none());
        assert_eq!(
            serde_json::from_slice::<JournalSpec>(&bare)
                .unwrap()
                .into_spec()
                .0,
            spec()
        );
        for src in [
            OriginSource::Image,
            OriginSource::File {
                path: "/tmp/r.wsl".into(),
            },
            OriginSource::CatalogImage { name: "u".into() },
            OriginSource::Machine { name: "m".into() },
            OriginSource::Adopted,
        ] {
            s.origin = Some(DistroOrigin {
                created: "x".into(),
                created_with: "y".into(),
                source: src.clone(),
            });
            let round: JournalSpec =
                serde_json::from_slice(&serde_json::to_vec(&s.to_journal()).unwrap()).unwrap();
            assert_eq!(round.into_spec().0.origin, s.origin);
        }
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
    fn restricted_round_trips_through_container_info() {
        let json = r#"{"configuration":{"id":"r1",
              "labels":{"io.github.daphnediane.container-distro.distro":"r1",
                        "io.github.daphnediane.container-distro.home-mount":"none",
                        "io.github.daphnediane.container-distro.user":"dp:501:20",
                        "io.github.daphnediane.container-distro.admin":"false"},
              "mounts":[{"source":"/state/sbin.distro.restricted","destination":"/sbin.distro","options":["ro"]}],
              "networks":[],
              "ssh":false,
              "image":{"reference":"alpine:latest"}},
              "status":{"state":"stopped"}}"#;
        let info: ContainerInfo = serde_json::from_str(json).unwrap();
        let back = DistroSpec::from_container(&info, "/Users/dp").unwrap();
        let (r, _) = resolved(&back);
        assert_eq!(r.network.as_deref(), Some("none"));
        assert!(!r.ssh && !r.admin && !back.admin_grant);
        assert_eq!(r.home_mount, HomeMount::None);
        assert!(r.is_restricted());
    }

    /// A distro created before the admin label existed keeps granting
    /// (it already did) but gets no grant env, so `set` won't recreate
    /// sudoers an admin removed.
    #[test]
    fn unlabeled_admin_keeps_grant_without_env() {
        let json = r#"{"configuration":{"id":"old",
              "labels":{"io.github.daphnediane.container-distro.distro":"old",
                        "io.github.daphnediane.container-distro.user":"dp:501:20"},
              "networks":[{"network":"default"}],
              "ssh":true,
              "image":{"reference":"alpine:latest"}},
              "status":{"state":"stopped"}}"#;
        let info: ContainerInfo = serde_json::from_str(json).unwrap();
        let back = DistroSpec::from_container(&info, "/Users/dp").unwrap();
        assert!(resolved(&back).0.admin && !back.admin_grant);
        // The recreated container is labeled `never`: still allowed,
        // still not armed — and it stays that way across plain `set`s.
        let joined = resolved(&back)
            .0
            .create_args(&ctx(), "id", "/s", "/h", &[])
            .join(" ");
        assert!(joined.contains("container-distro.admin=never"), "{joined}");
        assert!(!joined.contains("CONTAINER_ADMIN"));
        // An explicit `set --sudo` is what re-arms the grant env.
        let (rearmed, _) = SpecChanges {
            admin: Some(true),
            ..SpecChanges::default()
        }
        .apply(&back)
        .unwrap();
        assert!(rearmed.admin_grant);
        let joined = resolved(&rearmed)
            .0
            .create_args(&ctx(), "id", "/s", "/h", &[])
            .join(" ");
        assert!(joined.contains("container-distro.admin=true"), "{joined}");
        assert!(joined.contains("CONTAINER_ADMIN=1"), "{joined}");
    }

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

    #[test]
    fn automount_volumes() {
        let entries = [
            ("Macintosh HD".to_string(), true),
            ("CaseSens".to_string(), false),
            ("My Photos".to_string(), false),
            (".timemachine".to_string(), false),
            ("com.apple.TimeMachine.localsnapshots".to_string(), false),
            ("com.apple.os.update-ABCD".to_string(), false),
        ];
        let m = automounts(&entries, false);
        assert_eq!(
            m.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "/Volumes/CaseSens:/mnt/CaseSens:nosuid,nodev",
                "/Volumes/My Photos:/mnt/My-Photos:nosuid,nodev"
            ]
        );
        assert!(automounts(&entries, true).iter().all(|m| m.read_only));
    }

    #[test]
    fn is_automount_matches_canonical_shape() {
        for yes in [
            "/Volumes/X:/mnt/X",
            "/Volumes/My Photos:/mnt/My-Photos",
            // `automount ro` emits ro mounts — still canonical
            "/Volumes/X:/mnt/X:ro",
        ] {
            assert!(is_automount(&yes.parse().unwrap()), "{yes}");
        }
        for no in [
            // different target or source — a user's mount
            "/Volumes/X:/mnt/other",
            "/Volumes/X:/data",
            "/opt/x:/mnt/x",
            // nested source isn't a top-level volume
            "/Volumes/X/sub:/mnt/sub",
        ] {
            assert!(!is_automount(&no.parse().unwrap()), "{no}");
        }
    }

    /// Colliding targets keep the alphabetically first volume and drop
    /// the rest (with a warning on stderr).
    #[test]
    fn automount_target_collisions() {
        let entries = [
            ("My-Photos".to_string(), false),
            ("My Photos".to_string(), false),
            ("My  Photos".to_string(), false),
        ];
        let m = automounts(&entries, false);
        // "My  Photos" → "My--Photos" is distinct; the other two collide
        // on /mnt/My-Photos and "My Photos" sorts before "My-Photos".
        assert_eq!(
            m.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "/Volumes/My  Photos:/mnt/My--Photos:nosuid,nodev",
                "/Volumes/My Photos:/mnt/My-Photos:nosuid,nodev"
            ]
        );
    }

    /// `spec()` already pins every value it resolves to — the alias
    /// marks tests exercising pin-vs-derive semantics.
    fn pinned_spec() -> DistroSpec {
        spec()
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

    /// A pre-preset journal body — the recorded effective values as a
    /// [`JournalSpec`] with no `preset`/`explicit`/`source` — for the
    /// back-computation tests.
    fn legacy_journal(mut j: JournalSpec) -> JournalSpec {
        j.preset = None;
        j.explicit = Intent::default();
        j.source = Intent::default();
        j
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

    /// Reading a journal drops only fingerprint automounts; user
    /// mounts at automount-shaped targets — different options or
    /// target — stay.
    #[test]
    fn into_spec_drops_derived_automounts_only() {
        let mut s = spec();
        s.mounts = vec![
            // Exact derived fingerprint → dropped.
            "/Volumes/X:/mnt/X:nosuid,nodev".parse().unwrap(),
            // Canonical shape, but the user asked ro → kept.
            "/Volumes/Y:/mnt/Y:ro".parse().unwrap(),
            // Automount source at a custom target → kept.
            "/Volumes/Z:/data".parse().unwrap(),
        ];
        let (s, _) = s.to_journal().into_spec();
        assert_eq!(
            s.mounts.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["/Volumes/Y:/mnt/Y:ro", "/Volumes/Z:/data"]
        );
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

    /// A serialized journal round-trips; a pre-preset journal body
    /// still parses (missing keys fill with defaults).
    #[test]
    fn spec_journal_round_trip() {
        let s = pinned_spec();
        let json = serde_json::to_string_pretty(&s.to_journal()).unwrap();
        let (back, notes) = serde_json::from_str::<JournalSpec>(&json)
            .unwrap()
            .into_spec();
        assert_eq!(back, s);
        assert!(notes.is_empty(), "{notes:?}");

        // Missing `preset`/`explicit`/`source` keys mark a legacy
        // journal — parsed, then back-computed from recorded values.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = v.as_object_mut().unwrap();
        for key in ["preset", "explicit", "source"] {
            obj.remove(key);
        }
        let (legacy, _) = serde_json::from_value::<JournalSpec>(v)
            .unwrap()
            .into_spec();
        // Recorded home ro + agent on deviate from the default
        // derivation → pinned; recorded resources are the legacy
        // freeze → pinned as intent.
        assert_eq!(legacy.explicit.home_mount, Some(HomeMount::Ro));
        assert_eq!(legacy.explicit.ssh, Some(true));
        assert_eq!(legacy.explicit.cpus, Some(2));
        assert_eq!(legacy.explicit.memory.as_deref(), Some("4G"));
    }
}
