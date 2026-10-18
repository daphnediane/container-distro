/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

mod changes;
mod intent;
mod options;
mod shares;
#[cfg(test)]
mod test_support;
mod wsl_conf;

use crate::container::ContainerInfo;
use crate::naming::{label_key, label_lookup};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub use changes::SpecChanges;
pub use intent::{Intent, Preset, ResetTarget, ResolveCtx};
pub use options::{Arch, Automount, HomeMount, HostUser};
pub use shares::{MountSpec, PublishSpec, automounts, is_automount, is_derived_automount};
pub use wsl_conf::{ConfList, WslConf, WslConfMode, mode_from_flags};

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
    /// Publish the `distrofs` socket relay for host file access.
    pub files: bool,
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
    let multiplier = match unit {
        "" | "B" => 1,
        "K" | "KiB" => 1 << 10,
        "M" | "MiB" => 1 << 20,
        "G" | "GiB" => 1 << 30,
        "T" | "TiB" => 1 << 40,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(multiplier)
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

        // The relay is inert until the host connects, so it's on by
        // default — except under `restricted`, whose "every hole needs
        // its own flag" rule covers putting guest content in front of
        // Finder. `false` is the stricter bool, so a source "off" caps.
        let files = self
            .explicit
            .files
            .or_else(|| cap(preset.files(), self.source.files, std::cmp::min))
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
                files,
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
            "files",
            on(old_r.files).into(),
            on(new.files).into(),
            on(unpinned.files).into(),
            self.explicit.files.is_some(),
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::container::ContainerInfo;

    use super::changes::SpecChanges;
    use super::options::HomeMount;
    use super::test_support::{ctx, pinned_spec, resolved, spec};

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
