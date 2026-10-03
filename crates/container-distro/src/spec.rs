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

/// Label suffixes (full keys come from [`cm_core::naming::label_key`]).
///
/// Marks a container as a distro; the value is the distro name.
pub const LABEL_DISTRO: &str = "distro";
/// The home-directory mount mode (`rw`/`ro`/`none`).
pub const LABEL_HOME_MOUNT: &str = "home-mount";
/// The provisioned account as `name:uid:gid`.
pub const LABEL_USER: &str = "user";
/// Guest path where the init assets are mounted.
pub const INIT_DIR: &str = "/sbin.distro";

/// How the macOS home directory is shared into a distro.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
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

/// The host account mirrored into the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistroSpec {
    pub name: String,
    pub image: String,
    pub cpus: Option<u64>,
    /// Memory as a `container` size (`4G`, `2048M`).
    pub memory: Option<String>,
    pub home_mount: HomeMount,
    pub mounts: Vec<MountSpec>,
    pub publish: Vec<PublishSpec>,
    pub user: HostUser,
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
        a.push("--ssh".into());
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
            publish,
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

/// Changes requested by `container distro set`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecChanges {
    pub cpus: Option<u64>,
    pub memory: Option<String>,
    pub home_mount: Option<HomeMount>,
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
/// `Macintosh HD -> /`) are skipped. Names are lowercased with spaces
/// turned into `-`.
pub fn automounts(entries: &[(String, bool)]) -> Vec<MountSpec> {
    let mut v: Vec<MountSpec> = entries
        .iter()
        .filter(|(name, is_link)| !is_link && !name.starts_with('.'))
        .map(|(name, _)| MountSpec {
            source: format!("/Volumes/{name}"),
            target: format!("/mnt/{}", name.to_lowercase().replace(' ', "-")),
            read_only: false,
        })
        .collect();
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
            publish: vec!["8080:80".parse().unwrap()],
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
    fn create_args_mirror_machine_config() {
        let a = spec().create_args("/state/sbin.distro", "/Users/dp");
        let joined = a.join(" ");
        assert!(joined.starts_with(
            "create --name d1 --label io.github.daphnediane.container-distro.distro=d1"
        ));
        for want in [
            "--label io.github.daphnediane.container-distro.home-mount=ro",
            "--label io.github.daphnediane.container-distro.user=dp:501:20",
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
    fn spec_round_trips_through_container_info() {
        let s = spec();
        let json = format!(
            r#"{{"configuration":{{"id":"d1",
              "labels":{{"io.github.daphnediane.container-distro.distro":"d1",
                        "io.github.daphnediane.container-distro.home-mount":"ro",
                        "io.github.daphnediane.container-distro.user":"dp:501:20"}},
              "mounts":[{{"source":"/state/sbin.distro","destination":"/sbin.distro","options":["ro"]}},
                        {{"source":"/Users/dp","destination":"/Users/dp","options":["ro"]}},
                        {{"source":"/Volumes/X","destination":"/mnt/x","options":[]}}],
              "publishedPorts":[{{"hostAddress":"127.0.0.1","hostPort":8080,"containerPort":80,"proto":"tcp"}}],
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
    fn set_changes_apply() {
        let changes = SpecChanges {
            cpus: Some(4),
            add_mounts: vec!["/Volumes/Y:/mnt/x:ro".parse().unwrap()],
            remove_publish: vec![8080],
            add_publish: vec!["9000".parse().unwrap()],
            ..SpecChanges::default()
        };
        let s = changes.apply(&spec()).unwrap();
        assert_eq!(s.cpus, Some(4));
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
        ];
        let m = automounts(&entries);
        assert_eq!(
            m.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "/Volumes/CaseSens:/mnt/casesens",
                "/Volumes/My Photos:/mnt/my-photos"
            ]
        );
    }
}
