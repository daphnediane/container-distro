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

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use cm_core::container::ContainerInfo;
use cm_core::naming::{label_key, label_lookup};
use serde::{Deserialize, Serialize};

/// Label suffixes (full keys come from [`cm_core::naming::label_key`]).
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

    /// Whether automounts under this mode are read-only.
    pub fn read_only(self) -> bool {
        self == Automount::Ro
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// An extra host directory shared into the guest: `SRC:DST[:ro|rw]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountSpec {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

impl FromStr for MountSpec {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        let (source, target, read_only) = match parts.as_slice() {
            [src, dst] => (*src, *dst, false),
            [src, dst, "ro"] => (*src, *dst, true),
            [src, dst, "rw"] => (*src, *dst, false),
            _ => bail!("invalid mount `{s}` (expected SRC:DST[:ro|rw])"),
        };
        if !source.starts_with('/') || !target.starts_with('/') {
            bail!("invalid mount `{s}`: SRC and DST must be absolute paths");
        }
        Ok(MountSpec {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
        })
    }
}

impl fmt::Display for MountSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.target)?;
        if self.read_only {
            f.write_str(":ro")?;
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

/// Everything needed to (re)create a distro container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistroSpec {
    pub name: String,
    pub image: String,
    pub cpus: Option<u64>,
    /// Memory as a `container` size (`4G`, `2048M`).
    pub memory: Option<String>,
    pub home_mount: HomeMount,
    pub mounts: Vec<MountSpec>,
    /// Keep the canonical `/Volumes/<X>` → `/mnt/<x>` mounts in sync
    /// with what's attached: reconciled on `set` and re-evaluated when
    /// a stopped distro starts. `rw`/`ro` selects the mount mode.
    #[serde(default)]
    pub automount: Automount,
    pub publish: Vec<PublishSpec>,
    /// Network to attach (`None` = `container`'s default; `"none"` =
    /// no interfaces beyond loopback).
    pub network: Option<String>,
    /// Forward the host SSH agent socket (`container create --ssh`).
    pub ssh: bool,
    /// Provision sudo/doas for the user (mounts `grant-admin.sh`).
    pub admin: bool,
    /// Emit `CONTAINER_ADMIN=1` so `init -u` grants privileges at the
    /// next boot (label value `true` vs `never`). Kept separate from
    /// `admin`: a distro created before the admin label existed still
    /// allows privileges (`admin = true`, assets mounted) but is never
    /// armed, so `set` recreates won't re-grant sudoers an admin
    /// deliberately removed — only an explicit `set --sudo` does.
    pub admin_grant: bool,
    pub user: HostUser,
}

impl DistroSpec {
    /// Whether the distro is cut off from the host conveniences: no
    /// shares, no network, no agent, no privilege grant. What
    /// `--restricted` produces; explicit flags can reopen individual
    /// holes.
    pub fn is_restricted(&self) -> bool {
        self.home_mount == HomeMount::None
            && self.mounts.is_empty()
            && self.network.as_deref() == Some("none")
            && !self.ssh
            && !self.admin
    }
}

impl DistroSpec {
    /// Arguments for `container create`, mirroring how the machine plugin
    /// configures a machine: our init as the entrypoint, all capabilities,
    /// no masked or read-only paths, SSH agent forwarding, and the host
    /// account passed through the `CONTAINER_*` environment.
    pub fn create_args(&self, assets_dir: &str, host_home: &str) -> Vec<String> {
        let mut a: Vec<String> = ["create", "--name", &self.name].map(String::from).to_vec();
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
        for p in &self.publish {
            push("--publish", p.to_string());
        }
        if let Some(n) = &self.network {
            push("--network", n.clone());
        }
        if let Some(c) = self.cpus {
            push("--cpus", c.to_string());
        }
        if let Some(m) = &self.memory {
            push("--memory", m.clone());
        }
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
        if self.admin_grant {
            push("--env", "CONTAINER_ADMIN=1".into());
        }
        if self.ssh {
            a.push("--ssh".into());
        }
        a.push(self.image.clone());
        a
    }

    /// Reconstruct the spec of an existing distro container (for `set`).
    ///
    /// The init-assets and home mounts are recognized by destination and
    /// dropped; every other mount is a user mount.
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
        let mounts = cfg
            .mounts
            .iter()
            .filter(|m| m.destination != INIT_DIR && m.destination != host_home)
            .map(|m| MountSpec {
                source: m.source.clone(),
                target: m.destination.clone(),
                read_only: m.read_only(),
            })
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
        Ok(DistroSpec {
            name,
            image: cfg
                .image
                .as_ref()
                .map(|i| i.reference.clone())
                .unwrap_or_default(),
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
            user,
        })
    }
}

impl DistroSpec {
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

/// Changes requested by `container distro set`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecChanges {
    pub cpus: Option<u64>,
    pub memory: Option<String>,
    pub home_mount: Option<HomeMount>,
    /// Set the `/Volumes` automount mode (reconciled in `ops`, which
    /// can scan the filesystem). `none` drops the automounts.
    pub automount: Option<Automount>,
    /// Replace the network attachment (`none` disables, `default`
    /// restores the usual one).
    pub network: Option<String>,
    /// Turn SSH-agent forwarding on or off.
    pub ssh: Option<bool>,
    /// Turn privilege (sudo/doas) provisioning on or off.
    pub admin: Option<bool>,
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

    /// Apply to `spec`. Adding a mount at an existing target or a port on
    /// an existing host port replaces the old entry.
    pub fn apply(&self, spec: &DistroSpec) -> Result<DistroSpec> {
        let mut s = spec.clone();
        if let Some(c) = self.cpus {
            s.cpus = Some(c);
        }
        if let Some(m) = &self.memory {
            s.memory = Some(m.clone());
        }
        if let Some(h) = self.home_mount {
            s.home_mount = h;
        }
        if let Some(v) = self.automount {
            s.automount = v;
            if !v.enabled() {
                s.mounts.retain(|m| !is_automount(m));
            }
        }
        if let Some(n) = &self.network {
            s.network = Some(n.clone());
        }
        if let Some(v) = self.ssh {
            s.ssh = v;
        }
        if let Some(v) = self.admin {
            s.admin = v;
            // An explicit `--sudo`/`--no-sudo` is a fresh decision, so
            // the grant env follows it.
            s.admin_grant = v;
        }
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
        Ok(s)
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

    fn spec() -> DistroSpec {
        DistroSpec {
            name: "d1".into(),
            image: "alpine:latest".into(),
            cpus: Some(2),
            memory: Some("4G".into()),
            home_mount: HomeMount::Ro,
            mounts: vec!["/Volumes/X:/mnt/x".parse().unwrap()],
            automount: Automount::None,
            publish: vec!["8080:80".parse().unwrap()],
            network: None,
            ssh: true,
            admin: true,
            admin_grant: true,
            user: user(),
        }
    }

    #[test]
    fn mount_spec_parse() {
        let m: MountSpec = "/a:/b:ro".parse().unwrap();
        assert!(m.read_only);
        assert_eq!(m.to_string(), "/a:/b:ro");
        assert_eq!("/a:/b".parse::<MountSpec>().unwrap().to_string(), "/a:/b");
        for bad in ["/a", "a:/b", "/a:b", "/a:/b:xx"] {
            assert!(bad.parse::<MountSpec>().is_err(), "{bad}");
        }
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
        let a = spec().create_args("/state/sbin.distro", "/Users/dp");
        let joined = a.join(" ");
        assert!(joined.starts_with(
            "create --name d1 --label io.github.daphnediane.container-distro.distro=d1"
        ));
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

    #[test]
    fn create_args_home_none_has_no_home_volume() {
        let mut s = spec();
        s.home_mount = HomeMount::None;
        let joined = s.create_args("/s", "/Users/dp").join(" ");
        assert!(!joined.contains("/Users/dp:/Users/dp"));
    }

    #[test]
    fn create_args_restricted() {
        let mut s = spec();
        s.home_mount = HomeMount::None;
        s.mounts.clear();
        s.publish.clear();
        s.network = Some("none".into());
        s.ssh = false;
        s.admin = false;
        s.admin_grant = false;
        assert!(s.is_restricted());
        let joined = s.create_args("/s", "/Users/dp").join(" ");
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
        assert!(!spec().is_restricted());
    }

    #[test]
    fn spec_serializes_for_the_set_journal() {
        let s = spec();
        let round: DistroSpec = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(round, s);
    }

    #[test]
    fn spec_round_trips_through_container_info() {
        let s = spec();
        let json = format!(
            r#"{{"configuration":{{"id":"d1",
              "labels":{{"io.github.daphnediane.container-distro.distro":"d1",
                        "io.github.daphnediane.container-distro.home-mount":"ro",
                        "io.github.daphnediane.container-distro.user":"dp:501:20",
                        "io.github.daphnediane.container-distro.admin":"true"}},
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
        assert_eq!(back.memory.as_deref(), Some("4096M"));
        assert_eq!(
            DistroSpec {
                memory: Some("4G".into()),
                ..back
            },
            s
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
        assert_eq!(back.network.as_deref(), Some("none"));
        assert!(!back.ssh && !back.admin && !back.admin_grant);
        assert_eq!(back.home_mount, HomeMount::None);
        assert!(back.is_restricted());
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
        assert!(back.admin && !back.admin_grant);
        // The recreated container is labeled `never`: still allowed,
        // still not armed — and it stays that way across plain `set`s.
        let joined = back.create_args("/s", "/h").join(" ");
        assert!(joined.contains("container-distro.admin=never"), "{joined}");
        assert!(!joined.contains("CONTAINER_ADMIN"));
        // An explicit `set --sudo` is what re-arms the grant env.
        let rearmed = SpecChanges {
            admin: Some(true),
            ..SpecChanges::default()
        }
        .apply(&back)
        .unwrap();
        assert!(rearmed.admin_grant);
        let joined = rearmed.create_args("/s", "/h").join(" ");
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
        let s = changes.apply(&spec()).unwrap();
        assert_eq!(s.cpus, Some(4));
        assert_eq!(s.network.as_deref(), Some("none"));
        assert!(!s.ssh && !s.admin && !s.admin_grant);
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
        let s = spec();
        let g = |p: &str| s.guest_path(Path::new(p), "/Users/dp");
        assert_eq!(g("/Volumes/X/repo/src").as_deref(), Some("/mnt/x/repo/src"));
        assert_eq!(g("/Volumes/X").as_deref(), Some("/mnt/x"));
        assert_eq!(g("/Users/dp/code").as_deref(), Some("/Users/dp/code"));
        assert_eq!(g("/Volumes/Xylophone"), None);
        assert_eq!(g("/tmp"), None);
        let mut none = spec();
        none.home_mount = HomeMount::None;
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
                "/Volumes/CaseSens:/mnt/CaseSens",
                "/Volumes/My Photos:/mnt/My-Photos"
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
                "/Volumes/My  Photos:/mnt/My--Photos",
                "/Volumes/My Photos:/mnt/My-Photos"
            ]
        );
    }
}
