/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Mounts, published ports, and `/Volumes` automounts.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

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
}
