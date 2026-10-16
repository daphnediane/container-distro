/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Which parts of a guest `/etc/wsl.conf` are honored.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use super::super::changes::SpecChanges;
    use super::super::intent::Preset;
    use super::super::test_support::{ctx, legacy_journal, resolved, spec};

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
}
