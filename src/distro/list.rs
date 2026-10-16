/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `list` / `inspect`: distro summaries.

use super::adopt::ensure_journal;
use super::converge::{converge, dismantle, materialize};
use super::create::auto_shares;
use super::default::default_name;
use super::host::host_home;
use super::journal::{
    DistroLock, distro_busy, distro_exists, journal_fs_path, journal_names, preserved_dir,
    preserved_journal, read_journal,
};
use super::{distro_name, distros, find, run_container};
use crate::container::{self, ContainerInfo, ensure_started};
use crate::distro::spec::{
    DistroOrigin, DistroSpec, HomeMount, JournalSpec, LABEL_HOME_MOUNT, LABEL_IMPORTED_FROM,
    OriginSource, ResolvedSpec, size_bytes,
};
use crate::naming::{label_key, label_lookup};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// One distro, as reported by [`summaries`] (and `list --format json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistroSummary {
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
    /// Host disk space used by the root filesystem, in bytes.
    #[serde(default)]
    pub disk_size: Option<u64>,
    /// ISO 8601 UTC.
    #[serde(default)]
    pub created_date: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub home_mount: Option<String>,
    /// User mounts as `SRC:DST[:ro]` — the journaled shares only.
    #[serde(default)]
    pub mounts: Vec<String>,
    /// The derived shares: the `/Volumes` automounts plus the home
    /// mount — computed, never journaled.
    #[serde(default)]
    pub automounts: Vec<String>,
    /// The full mount set passed to `container create`: `mounts` +
    /// `automounts` with user mounts winning target collisions.
    #[serde(default)]
    pub effective_mounts: Vec<String>,
    /// The `/Volumes` automount mode (`rw`/`ro`/`none`).
    #[serde(default)]
    pub automount: Option<String>,
    /// The defaults tier (`default`/`restricted`/`trusted`).
    #[serde(default)]
    pub preset: Option<String>,
    /// Published ports as `IP:HOST:GUEST/PROTO`.
    #[serde(default)]
    pub ports: Vec<String>,
    /// No shares, no network, no agent, no privilege grant — what
    /// `--restricted` produces.
    #[serde(default)]
    pub restricted: bool,
    /// Provenance — what the distro was created from (journal `origin`).
    #[serde(default)]
    pub origin: Option<DistroOrigin>,
    /// Display form of the source for imported distros (`file:x.wsl`,
    /// `catalog:Name`) — set when `image` is a generated
    /// `local/distro-*` snapshot that hides where it really came from.
    /// List output prefers it over `image`.
    #[serde(default)]
    pub source: Option<String>,
}

impl DistroSummary {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

fn summarize(c: &ContainerInfo, default: Option<&str>, app_root: Option<&Path>) -> DistroSummary {
    let cfg = &c.configuration;
    let home = host_home().unwrap_or_default();
    let spec = DistroSpec::from_container(c, &home).ok();
    let name = distro_name(c).unwrap_or_else(|| c.id());
    let journal = read_journal(name).ok();
    // The journal is the truth; the container is the fallback for a
    // pre-journal distro awaiting adoption.
    let spec = journal.as_ref().or(spec.as_ref());
    let resolved = spec.map(|s| s.resolve().0);
    let origin = journal.as_ref().and_then(|s| s.origin.clone());
    let (automounts_list, effective_mounts) =
        resolved.as_ref().map(share_lists).unwrap_or_default();
    // The journal's `origin.created` is the distro's real creation
    // date; a recreated container's `creationDate` would say "when it
    // last materialized" instead.
    let created_date = origin
        .as_ref()
        .map(|o| o.created.clone())
        .or_else(|| cfg.creation_date.clone());
    DistroSummary {
        id: name.to_string(),
        status: c.status.state.clone(),
        default: default == Some(name),
        ip_address: c.ipv4().map(str::to_string),
        cpus: cfg.resources.as_ref().and_then(|r| r.cpus),
        memory: cfg.resources.as_ref().and_then(|r| r.memory_in_bytes),
        disk_size: app_root.and_then(|r| container::container_disk_usage(r, c.id())),
        created_date,
        image: cfg.image.as_ref().map(|i| i.reference.clone()),
        // A stopped distro has no container to report a platform; the
        // journal's arch is what it materializes as.
        platform: cfg
            .platform
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| resolved.as_ref().map(|r| format!("linux/{}", r.arch))),
        home_mount: label_lookup(&cfg.labels, LABEL_HOME_MOUNT).cloned(),
        mounts: resolved
            .as_ref()
            .map(|r| r.mounts.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        automounts: automounts_list,
        effective_mounts,
        automount: resolved.as_ref().map(|r| r.automount.as_str().to_string()),
        preset: resolved.as_ref().map(|r| r.preset.as_str().to_string()),
        ports: resolved
            .as_ref()
            .map(|r| r.publish.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        restricted: resolved.is_some_and(|r| r.is_restricted()),
        source: origin
            .as_ref()
            .and_then(|o| origin_source_display(&o.source)),
        origin,
    }
}

/// The derived shares for a `list` row: the `/Volumes` automounts the
/// spec's mode would mount now, then the home mount rendered as
/// `HOME:HOME[:ro]`. Everything `container create` mounts beyond the
/// user's own list.
fn share_lists(r: &ResolvedSpec) -> (Vec<String>, Vec<String>) {
    let mut auto: Vec<String> = auto_shares(r)
        .unwrap_or_default()
        .iter()
        .map(ToString::to_string)
        .collect();
    if r.home_mount != HomeMount::None
        && let Ok(home) = host_home()
    {
        let ro = if r.home_mount == HomeMount::Ro {
            ":ro"
        } else {
            ""
        };
        auto.push(format!("{home}:{home}{ro}"));
    }
    let mut effective: Vec<String> = r.mounts.iter().map(ToString::to_string).collect();
    effective.extend(auto.iter().cloned());
    (auto, effective)
}

/// A `list` row for a journaled (stopped) distro — assembled from its
/// spec, since there is no container to inspect.
fn journal_summary(name: &str, default: Option<&str>) -> Option<DistroSummary> {
    let spec = read_journal(name).ok()?;
    let disk = journal_fs_path(name)
        .ok()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| std::os::unix::fs::MetadataExt::blocks(&m) * 512);
    let (r, _) = spec.resolve();
    let (automounts_list, effective_mounts) = share_lists(&r);
    Some(DistroSummary {
        id: name.to_string(),
        status: if distro_busy(name) {
            "busy".into()
        } else {
            "stopped".into()
        },
        default: default == Some(name),
        cpus: r.cpus,
        memory: r.memory.as_deref().and_then(size_bytes),
        disk_size: disk,
        image: Some(spec.image.clone()),
        home_mount: Some(r.home_mount.as_str().to_string()),
        mounts: spec.mounts.iter().map(ToString::to_string).collect(),
        automounts: automounts_list,
        effective_mounts,
        automount: Some(r.automount.as_str().to_string()),
        preset: Some(r.preset.as_str().to_string()),
        ports: spec.publish.iter().map(ToString::to_string).collect(),
        restricted: r.is_restricted(),
        created_date: spec.origin.as_ref().map(|o| o.created.clone()),
        source: spec
            .origin
            .as_ref()
            .and_then(|o| origin_source_display(&o.source)),
        origin: spec.origin.clone(),
        ..Default::default()
    })
}

/// The `list` source string for a journal `origin` — `file:`/`catalog:`
/// prefixes make a replaced image ref self-documenting. `Image`,
/// `Machine`, and `Adopted` keep the image reference (a real ref is
/// the better description).
fn origin_source_display(s: &OriginSource) -> Option<String> {
    match s {
        OriginSource::File { path } => Some(format!("file:{}", source_basename(path))),
        OriginSource::Download { name, .. } | OriginSource::CatalogImage { name } => {
            Some(format!("catalog:{name}"))
        }
        _ => None,
    }
}

/// The trailing component of a recorded source — a local path's file
/// name or a URL's last segment; `-` (stdin) reads as `stdin`.
fn source_basename(s: &str) -> String {
    if s == "-" {
        return "stdin".to_string();
    }
    s.rsplit('/')
        .next()
        .filter(|b| !b.is_empty())
        .unwrap_or(s)
        .to_string()
}

/// Whether `image` is one of `import`'s generated refs
/// (`local/distro-<name>:imported-<ts>`) — a name that carries no
/// provenance, so `list` shows the source instead.
fn is_generated_image(image: Option<&str>) -> bool {
    image.is_some_and(|i| i.starts_with("local/distro-") && i.contains(":imported-"))
}

/// Journals predating `origin`: a generated `local/distro-*` image
/// ref hides the real source — the imported image's own
/// `imported-from` label still knows it (a local path or the catalog
/// URL). One `image list` for however many rows need it.
fn fill_legacy_sources(rows: &mut [DistroSummary]) {
    let needs = |r: &DistroSummary| r.source.is_none() && is_generated_image(r.image.as_deref());
    if !rows.iter().any(needs) {
        return;
    }
    let Some(images) = container::list_images() else {
        return;
    };
    for r in rows.iter_mut().filter(|r| needs(r)) {
        if let Some(from) = images
            .iter()
            .find(|i| Some(&i.configuration.name) == r.image.as_ref())
            .and_then(|i| i.labels().get(&label_key(LABEL_IMPORTED_FROM)).cloned())
        {
            r.source = Some(format!("file:{}", source_basename(&from)));
        }
    }
}

/// All distros, sorted by name — running or stopped-with-container from
/// `container list`, stopped ones from their journals.
pub fn summaries(running_only: bool) -> Result<Vec<DistroSummary>> {
    let default = default_name();
    // DISK sizes are read from `container`'s internal storage layout.
    container::warn_unverified_version();
    let app_root = container::app_root();
    let mut rows: Vec<DistroSummary> = distros()?
        .iter()
        .filter(|c| !running_only || c.is_running())
        .map(|c| summarize(c, default.as_deref(), app_root.as_deref()))
        .collect();
    if !running_only {
        for name in journal_names()? {
            if !rows.iter().any(|r| r.id == name)
                && let Some(s) = journal_summary(&name, default.as_deref())
            {
                rows.push(s);
            }
        }
        // Filesystems staged by `set`s on pre-journal builds still get
        // pseudo-rows so they can be `rm`'d from the listing.
        let orphans: Vec<DistroSummary> = staged_summaries()
            .into_iter()
            .filter(|s| !rows.iter().any(|r| r.id == s.id))
            .collect();
        rows.extend(orphans);
    }
    fill_legacy_sources(&mut rows);
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}

/// Pseudo-summaries for `preserved/` leftovers whose distro is gone —
/// staging left by an interrupted `set` on a pre-journal build.
/// "interrupted" since nothing can still be holding their lock.
fn staged_summaries() -> Vec<DistroSummary> {
    let Ok(dir) = preserved_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".ext4"))
                .map(String::from)
        })
        .map(|id| {
            let image = preserved_journal(&id)
                .ok()
                .and_then(|p| fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<JournalSpec>(&b).ok())
                .map(|j| j.into_spec().0.image);
            DistroSummary {
                id,
                status: "interrupted".into(),
                image,
                ..Default::default()
            }
        })
        .collect()
}

/// Materialize `name`'s container without booting (for `inspect`), then
/// dismantle it again — a read shouldn't change the resting state.
fn peek(name: &str) -> Result<()> {
    ensure_journal(name)?;
    let lock = DistroLock::acquire(name)?;
    if converge(name, &lock)?.is_some() {
        return Ok(()); // running — inspectable as-is
    }
    let spec = lock.spec()?;
    let id = materialize(name, &spec, &lock)?;
    let r = run_container(&["inspect", &id]);
    dismantle(name, &id, &lock)?;
    r
}

pub fn inspect(names: &[String]) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    for n in names {
        // A running distro inspects as-is; anything else converges to
        // the journal view first so the output shows the configuration
        // the next start will actually apply.
        if let Some(info) = find(n).ok().filter(|i| i.is_running()) {
            run_container(&["inspect", info.id()])?;
        } else if distro_exists(n)? {
            peek(n)?;
        } else {
            bail!("no distro named `{n}`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::distro::spec::OriginSource;

    #[test]
    fn source_display() {
        let disp = |s: &OriginSource| origin_source_display(s).unwrap();
        assert_eq!(
            disp(&OriginSource::File {
                path: "/tmp/Rocky-9.wsl".into()
            }),
            "file:Rocky-9.wsl"
        );
        assert_eq!(disp(&OriginSource::File { path: "-".into() }), "file:stdin");
        assert_eq!(
            disp(&OriginSource::Download {
                url: "https://x/d.wsl".into(),
                sha256: "ab".into(),
                name: "RLCPlus-9".into()
            }),
            "catalog:RLCPlus-9"
        );
        // Real references stay in the image column.
        for s in [
            OriginSource::Image,
            OriginSource::Machine { name: "poc".into() },
            OriginSource::Adopted,
        ] {
            assert!(origin_source_display(&s).is_none());
        }
        // Basenames: URLs and paths alike.
        assert_eq!(
            source_basename("https://x/y/RLCPlus-9.aarch64.wsl"),
            "RLCPlus-9.aarch64.wsl"
        );
        assert_eq!(source_basename("/a/b/rootfs.tar.gz"), "rootfs.tar.gz");
        assert!(is_generated_image(Some("local/distro-r9:imported-1")));
        assert!(!is_generated_image(Some("local/distro-r9:snap-1")));
        assert!(!is_generated_image(Some("docker.io/library/alpine:latest")));
    }
}
