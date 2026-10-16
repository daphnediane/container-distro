/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Serde types for the `container` CLI's JSON output.

use serde::{Deserialize, Serialize};

/// A container machine as reported by `container machine list --format json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(default)]
    pub cpus: Option<u64>,
    #[serde(default)]
    pub memory: Option<u64>,
    #[serde(default)]
    pub disk_size: Option<u64>,
}

impl Machine {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// Extra per-machine fields reported only by `container machine inspect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineDetail {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub container_id: Option<String>,
    #[serde(default)]
    pub user_setup: Option<UserSetup>,
    #[serde(default)]
    pub home_mount: Option<String>,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default)]
    pub image: Option<ImageInfo>,
    #[serde(default)]
    pub cpus: Option<u64>,
    /// Bytes.
    #[serde(default)]
    pub memory: Option<u64>,
}

impl MachineDetail {
    pub fn is_running(&self) -> bool {
        self.status.as_deref() == Some("running")
    }
}

/// The host account a machine provisioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSetup {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
}

/// An OCI platform (`os`/`architecture`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.os, self.architecture)
    }
}

/// The image a machine was created from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageInfo {
    pub reference: String,
}

/// A container as reported by `container list --format json` / `inspect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub configuration: ContainerConfig,
    #[serde(default)]
    pub status: ContainerStatus,
}

impl ContainerInfo {
    pub fn id(&self) -> &str {
        &self.configuration.id
    }

    pub fn is_running(&self) -> bool {
        self.status.state == "running"
    }

    /// The first IPv4 address, without its prefix length.
    pub fn ipv4(&self) -> Option<&str> {
        self.status
            .networks
            .iter()
            .find_map(|n| n.ipv4_address.as_deref())
            .and_then(|a| a.split('/').next())
    }
}

/// The subset of a container's configuration that `cm` cares about.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerConfig {
    pub id: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub mounts: Vec<MountInfo>,
    #[serde(default)]
    pub published_ports: Vec<PublishedPort>,
    /// Network attachments; empty when created with `--network none`.
    #[serde(default)]
    pub networks: Vec<NetworkAttachment>,
    /// Whether the SSH agent socket is forwarded in (`--ssh` at create).
    #[serde(default)]
    pub ssh: bool,
    #[serde(default)]
    pub resources: Option<Resources>,
    #[serde(default)]
    pub image: Option<ImageInfo>,
    #[serde(default)]
    pub platform: Option<Platform>,
    /// ISO 8601 UTC, e.g. `2026-10-03T23:35:14Z`.
    #[serde(default)]
    pub creation_date: Option<String>,
}

/// A filesystem mount in a container's configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountInfo {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub options: Vec<String>,
}

impl MountInfo {
    pub fn read_only(&self) -> bool {
        self.options.iter().any(|o| o == "ro")
    }
}

/// A network attachment in a container's configuration. Additional
/// per-attachment options (hostname, mtu, …) are not modeled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkAttachment {
    pub network: String,
}

/// A published (host → container) port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedPort {
    #[serde(default)]
    pub host_address: Option<String>,
    pub host_port: u16,
    pub container_port: u16,
    #[serde(default)]
    pub proto: Option<String>,
}

/// CPU and memory allocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    #[serde(default)]
    pub cpus: Option<u64>,
    #[serde(default)]
    pub memory_in_bytes: Option<u64>,
}

/// Runtime status of a container.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatus {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub networks: Vec<NetworkStatus>,
}

/// One network attachment of a running container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    #[serde(default)]
    pub ipv4_address: Option<String>,
}

/// An image as reported by `container image list --format json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageListEntry {
    pub configuration: ImageListConfig,
    #[serde(default)]
    pub variants: Vec<ImageVariant>,
}

impl ImageListEntry {
    /// OCI `config.Labels` merged across variants (single-variant in
    /// practice; later entries win on collision).
    pub fn labels(&self) -> std::collections::BTreeMap<String, String> {
        let mut m = std::collections::BTreeMap::new();
        for v in &self.variants {
            if let Some(c) = v.config.as_ref().and_then(|c| c.config.as_ref()) {
                m.extend(c.labels.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        m
    }
}

/// The per-image fields of an [`ImageListEntry`].
#[derive(Debug, Clone, Deserialize)]
pub struct ImageListConfig {
    /// The image reference, e.g. `local/distro-d1:imported-1728000000`.
    pub name: String,
    /// When this tag was recorded — each reference is its own entry,
    /// so this is the tag's age, used for cache eviction ordering.
    #[serde(default, rename = "creationDate")]
    pub creation_date: Option<String>,
}

/// One platform variant of an [`ImageListEntry`].
#[derive(Debug, Clone, Deserialize)]
pub struct ImageVariant {
    #[serde(default)]
    pub config: Option<ImageVariantConfig>,
    /// Total size of this variant in bytes.
    #[serde(default)]
    pub size: i64,
}

/// A variant's image-config wrapper (`config.config` in the JSON).
#[derive(Debug, Clone, Deserialize)]
pub struct ImageVariantConfig {
    /// The OCI image config object.
    #[serde(default)]
    pub config: Option<ImageUserConfig>,
}

/// The subset of the OCI image `config` object we read. Its `Labels`
/// key is capitalized in `container`'s Go-serialized JSON.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImageUserConfig {
    #[serde(default, rename = "Labels")]
    pub labels: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use super::super::machine::machine_workdir;
    use super::super::test_support::{INSPECT_JSON, detail};

    const LIST_JSON: &str = r#"[{"id":"alpine","memory":34359738368,"createdDate":"2026-10-01T00:58:57Z","status":"running","diskSize":78872576,"default":true,"cpus":7,"ipAddress":"192.168.64.6"}]"#;

    const CONTAINER_JSON: &str = r#"[{"configuration":{"capAdd":["ALL"],"id":"d1",
        "labels":{"io.github.daphnediane.container-distro.distro":"d1"},
        "mounts":[{"destination":"/mnt/x","options":["ro"],"source":"/Volumes/X","type":{"virtiofs":{}}}],
        "publishedPorts":[{"containerPort":80,"count":1,"hostAddress":"127.0.0.1","hostPort":8080,"proto":"tcp"}],
        "resources":{"cpuOverhead":0,"cpus":4,"memoryInBytes":1073741824},
        "image":{"reference":"docker.io/library/alpine:latest"},
        "platform":{"architecture":"arm64","os":"linux"}},
        "id":"d1",
        "status":{"networks":[{"ipv4Address":"192.168.64.13/24","network":"default"}],"state":"running"}}]"#;

    #[test]
    fn test_machine_deserialize_list() {
        let machines: Vec<Machine> = serde_json::from_str(LIST_JSON).unwrap();
        assert_eq!(machines.len(), 1);
        let m = &machines[0];
        assert!(m.is_running() && m.default);
        assert_eq!(m.cpus, Some(7));
        assert_eq!(m.disk_size, Some(78872576));
        let round: Machine = serde_json::from_str(&serde_json::to_string(m).unwrap()).unwrap();
        assert_eq!(&round, m);
    }

    #[test]
    fn test_machine_deserialize_minimal() {
        let m: Machine = serde_json::from_str(r#"{"id":"x","status":"stopped"}"#).unwrap();
        assert!(!m.is_running() && !m.default && m.cpus.is_none());
    }

    #[test]
    fn test_machine_detail_deserialize() {
        let d: Vec<MachineDetail> = serde_json::from_str(INSPECT_JSON).unwrap();
        let d = &d[0];
        assert_eq!(d.container_id.as_deref(), Some("alpine-730439"));
        assert_eq!(d.platform.as_ref().unwrap().to_string(), "linux/arm64");
        assert_eq!(
            d.image.as_ref().unwrap().reference,
            "docker.io/library/alpine:latest"
        );
        let round: MachineDetail =
            serde_json::from_str(&serde_json::to_string(d).unwrap()).unwrap();
        assert_eq!(&round, d);
    }

    #[test]
    fn test_container_info_deserialize() {
        let c: Vec<ContainerInfo> = serde_json::from_str(CONTAINER_JSON).unwrap();
        let c = &c[0];
        assert_eq!(c.id(), "d1");
        assert!(c.is_running());
        assert_eq!(c.ipv4(), Some("192.168.64.13"));
        assert!(c.configuration.mounts[0].read_only());
        assert_eq!(c.configuration.published_ports[0].host_port, 8080);
        let round: ContainerInfo =
            serde_json::from_str(&serde_json::to_string(c).unwrap()).unwrap();
        assert_eq!(&round, c);
    }

    #[test]
    fn test_container_info_stopped_minimal() {
        let c: ContainerInfo =
            serde_json::from_str(r#"{"configuration":{"id":"x"},"status":{"state":"stopped"}}"#)
                .unwrap();
        assert!(!c.is_running() && c.ipv4().is_none());
    }

    #[test]
    fn machine_workdir_only_existing_paths() {
        let mut d = detail();
        let home = Some("/Users/daphnediane");
        let wd = |d: &MachineDetail, p: &str| machine_workdir(d, Some(Path::new(p)), home);
        assert_eq!(
            wd(&d, "/Users/daphnediane/src").as_deref(),
            Some("/Users/daphnediane/src")
        );
        assert_eq!(wd(&d, "/Volumes/X").as_deref(), Some("/home/daphnediane"));
        d.home_mount = Some("none".into());
        assert_eq!(
            wd(&d, "/Users/daphnediane/src").as_deref(),
            Some("/home/daphnediane")
        );
    }
}
