/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Operations on the installable-distro catalog, shared by `cm` and
//! `container distro`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::container::{self, ImageListEntry, Machine, MachineDetail};
use crate::naming;
use crate::oci;
use crate::ops::{self, CreateOptions};
use crate::spec::{
    Arch, HomeMount, Intent, LABEL_IMPORTED_FROM, LABEL_ROOTFS_SHA256, OriginSource, WslConfMode,
};
use crate::table;
use crate::wsl;
use crate::{catalog, ops::ImportSource};

/// What a catalog install resolves to on this host.
#[derive(Debug)]
pub enum Resolved<'a> {
    /// An OCI image; `name` is the catalog entry's NAME when the image
    /// came from an entry rather than a bare reference.
    Image {
        image: &'a str,
        name: Option<&'a str>,
        /// What the source asks for — the entry's `HomeMount`
        /// override (absent → `rw`: installing through a catalog
        /// entry is an act of trust in its curation, whether the
        /// catalog is built-in or a `--catalog FILE`). Empty for a
        /// bare image reference — it asked for nothing.
        requested: Intent,
    },
    /// A catalog `.wsl` download; `name` is the entry's NAME.
    Wsl {
        download: &'a catalog::Download,
        name: &'a str,
        /// What the entry asks for, as for `Image`.
        requested: Intent,
    },
}

/// Resolve `arg` against `cat`. `None` selects the catalog default; a
/// name match is case-insensitive. With `image_fallback`, a non-match is
/// treated as an OCI image reference (`cm --install DISTRO|IMAGE`);
/// without it, non-matches are an error (`container distro catalog
/// install`, where image installs already have `create`). `arch` picks
/// the `.wsl` download — `amd64` selects the x86-64 rootfs, which the
/// import tags so `container` runs it under Rosetta.
pub fn resolve<'a>(
    cat: &'a catalog::Catalog,
    arg: Option<&'a str>,
    arch: Arch,
    image_fallback: bool,
) -> Result<Resolved<'a>> {
    let entry = match arg {
        None => cat.default_entry().context("the distro catalog is empty")?,
        Some(a) => match catalog::lookup(&cat.entries, a) {
            Some(e) => e,
            None if image_fallback => {
                return Ok(Resolved::Image {
                    image: a,
                    name: None,
                    requested: Intent::default(),
                });
            }
            None => bail!("{a:?} is not in the distro catalog (use `create` for images)"),
        },
    };
    let requested = Intent::only_home_mount(entry.home_mount.unwrap_or(HomeMount::Rw));
    match entry.source(arch.as_str()) {
        Some(catalog::Source::Image(i)) => Ok(Resolved::Image {
            image: i,
            name: Some(&entry.name),
            requested,
        }),
        Some(catalog::Source::Wsl(download)) => Ok(Resolved::Wsl {
            download,
            name: &entry.name,
            requested,
        }),
        None => bail!(
            "{:?} is not available for {} (no image or .wsl download for that architecture)",
            entry.name,
            arch
        ),
    }
}

/// Resolve and install a catalog entry (or, with `image_fallback`, an
/// image reference) as a distro. Returns the distro name.
pub fn install(
    cat: &catalog::Catalog,
    arg: Option<&str>,
    requested_name: Option<String>,
    opts: &CreateOptions,
    image_fallback: bool,
) -> Result<String> {
    install_resolved(
        resolve(
            cat,
            arg,
            opts.arch.unwrap_or_else(Arch::host),
            image_fallback,
        )?,
        requested_name,
        opts,
    )
}

/// Install an already-resolved source as a distro. Image entries become
/// `create`s; `.wsl` entries are fetched, verified, cached, and imported
/// with download provenance. The resolved source carries its intent —
/// what the catalog entry asked for (`HomeMount` override, `rw` when
/// unset) — which `create`/`import` cap the derivation with.
pub fn install_resolved(
    resolved: Resolved<'_>,
    requested_name: Option<String>,
    opts: &CreateOptions,
) -> Result<String> {
    container::ensure_started()?;
    match resolved {
        Resolved::Image {
            image,
            name,
            requested,
        } => {
            let distro_name = requested_name
                .unwrap_or_else(|| container::default_machine_name(name.unwrap_or(image)));
            container::validate_name(&distro_name)?;
            let origin = match name {
                Some(n) => OriginSource::CatalogImage { name: n.into() },
                None => OriginSource::Image,
            };
            // A catalog install's image was curated and verified at
            // install time — its packaged config is trusted like
            // `--trusted`'s, as source intent (explicit flags still
            // win). Only named entries: an image-ref fallback isn't a
            // catalog install.
            let mut requested = requested;
            if name.is_some() {
                requested.wsl_conf = Some(WslConfMode::all());
            }
            ops::create(Some(distro_name), opts, image, origin, &requested)
        }
        Resolved::Wsl {
            download,
            name,
            requested,
        } => {
            // `fetched` holds a shared lock on the cache entry — purge
            // and prune wait for the import below instead of deleting
            // the file mid-flight.
            let fetched = wsl::fetch(&download.url, &download.sha256, name)?;
            import_file(
                fetched.path(),
                requested_name,
                Some(name),
                opts,
                ImportSource::Download {
                    url: &download.url,
                    sha256: &download.sha256,
                    name,
                },
                &requested,
            )
        }
    }
}

/// Derive a default distro name from a rootfs filename: drop the
/// archive suffixes (`rocky-9.wsl` → `rocky-9`, `fs.tar.gz` → `fs`).
fn file_default_name(file: &Path) -> String {
    let Some(mut stem) = file.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return "distro".to_string();
    };
    for ext in [".tar.gz", ".tgz", ".tar.xz", ".wsl", ".tar"] {
        if stem.len() > ext.len() && stem.to_lowercase().ends_with(ext) {
            stem.truncate(stem.len() - ext.len());
            break;
        }
    }
    if stem.is_empty() {
        "distro".to_string()
    } else {
        stem
    }
}

/// Import a rootfs tar/`.wsl` as a distro, deriving its name in WSL
/// order when `requested_name` is absent: the package manifest's
/// `oobe.defaultName`, then `name_hint`, then the file stem.
pub fn import_file(
    file: &Path,
    requested_name: Option<String>,
    name_hint: Option<&str>,
    opts: &CreateOptions,
    source: ImportSource<'_>,
    requested: &Intent,
) -> Result<String> {
    if !file.is_file() {
        bail!("{} does not exist", file.display());
    }
    // One scan yields the name suggestion and the packaged config
    // files `import` snapshots — no second pass over the tar.
    let pkg = wsl::package_conf(file).unwrap_or_default();
    let name = requested_name.unwrap_or_else(|| {
        container::default_machine_name(
            &pkg.default_name
                .clone()
                .or_else(|| name_hint.map(str::to_string))
                .unwrap_or_else(|| file_default_name(file)),
        )
    });
    container::validate_name(&name)?;
    // Fail a refused name before `import` stages an image (the `.wsl`
    // download is already cached, so retrying with another name skips
    // the fetch). `import` re-checks — and issues the warning — for
    // --force.
    if !opts.force {
        ops::bail_machine_shadow(&name, false)?;
    }
    ops::import(&name, file, opts, source, requested, Some(&pkg))
}

/// A `container machine` plus the inspected fields catalog reporting
/// needs (`machine list` has no image column).
#[derive(Debug)]
struct MachineState {
    machine: Machine,
    detail: Option<MachineDetail>,
}

impl MachineState {
    fn is_running(&self) -> bool {
        self.machine.is_running()
    }

    fn image(&self) -> Option<&str> {
        self.detail
            .as_ref()
            .and_then(|d| d.image.as_ref())
            .map(|i| i.reference.as_str())
    }

    fn local(&self) -> LocalMachine {
        LocalMachine {
            id: self.machine.id.clone(),
            status: self.machine.status.clone(),
            default: self.machine.default,
            ip_address: self.machine.ip_address.clone(),
            cpus: self.machine.cpus,
            memory: self.machine.memory,
            disk_size: self.machine.disk_size,
            image: self.image().map(str::to_string),
            platform: self
                .detail
                .as_ref()
                .and_then(|d| d.platform.as_ref())
                .map(|p| p.to_string()),
            home_mount: self.detail.as_ref().and_then(|d| d.home_mount.clone()),
        }
    }
}

/// Local state for a verbose catalog listing. Every field degrades to
/// empty rather than erroring or blocking.
pub struct StatusProbe {
    /// Verified `<sha256>.wsl` files in the download cache.
    cached: Vec<wsl::CacheEntry>,
    /// Images `container` knows, or empty when services are down.
    images: Vec<ImageListEntry>,
    /// Machines and their inspected image, when requested. Only ever
    /// scanned (matched by image), so a flat list is the right shape.
    machines: Vec<MachineState>,
    /// Distro rows keyed by id, so name joins and conflict checks are
    /// log-time and iteration order is deterministic.
    distros: BTreeMap<String, ops::DistroSummary>,
}

/// A SHA-256 in cache-filename form: lowercase hex, `0x` stripped — the
/// label on an imported image keeps the catalog's raw spelling.
fn normalize_sha(s: &str) -> String {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
        .to_lowercase()
}

impl StatusProbe {
    /// Gather the non-blocking local probes used by verbose catalog
    /// listings. `container` is consulted only when already running, so
    /// listing never starts services.
    pub fn gather(include_machines: bool, include_distros: bool) -> Self {
        let cached = wsl::cache_entries();
        let mut images = Vec::new();
        let mut machines = Vec::new();
        let mut distros = BTreeMap::new();
        if container::system_running() {
            images = container::list_images().unwrap_or_default();
            if include_machines {
                // `machine list` has no image column; inspect reports
                // the reference each machine was created from.
                machines = container::list_machines()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|machine| MachineState {
                        detail: container::inspect_machine(&machine.id).ok(),
                        machine,
                    })
                    .collect();
            }
            if include_distros {
                distros = ops::summaries(false)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|d| (d.id.clone(), d))
                    .collect();
            }
        }
        Self {
            cached,
            images,
            machines,
            distros,
        }
    }

    /// The `rootfs-sha256` label on a local image, normalized — the
    /// link from a distro's image back to the `.wsl` download it was
    /// imported from.
    fn rootfs_sha(&self, image_ref: &str) -> Option<String> {
        let key = naming::label_key(LABEL_ROOTFS_SHA256);
        self.images
            .iter()
            .find(|i| container::same_image(&i.configuration.name, image_ref))
            .and_then(|i| i.labels().get(&key).map(|s| normalize_sha(s)))
    }

    /// Whether `image_ref` is the local artifact produced by `source`
    /// — the catalog image itself, or an image imported from the
    /// entry's verified `.wsl` rootfs.
    fn source_matches_image(&self, source: catalog::Source<'_>, image_ref: &str) -> bool {
        match source {
            catalog::Source::Image(i) => container::same_image(image_ref, i),
            catalog::Source::Wsl(d) => self
                .rootfs_sha(image_ref)
                .is_some_and(|s| s == d.sha256_normalized()),
        }
    }

    /// The coarse local state used by catalog listings.
    fn local_state(&self, source: catalog::Source<'_>) -> catalog::Local {
        match source {
            catalog::Source::Image(i) => {
                if self
                    .images
                    .iter()
                    .any(|x| container::same_image(&x.configuration.name, i))
                {
                    catalog::Local::Yes
                } else {
                    catalog::Local::No
                }
            }
            catalog::Source::Wsl(d) => {
                let sha = d.sha256_normalized();
                if self.cached.iter().any(|e| e.sha256 == sha) {
                    catalog::Local::Yes
                } else if wsl::download_in_progress(&sha) {
                    catalog::Local::Downloading
                } else {
                    catalog::Local::No
                }
            }
        }
    }

    /// Local images matching an entry's install source.
    fn matching_images(&self, source: catalog::Source<'_>) -> Vec<LocalImage> {
        let sha_key = naming::label_key(LABEL_ROOTFS_SHA256);
        let from_key = naming::label_key(LABEL_IMPORTED_FROM);
        self.images
            .iter()
            .filter(|i| match source {
                catalog::Source::Image(image) => {
                    container::same_image(&i.configuration.name, image)
                }
                catalog::Source::Wsl(d) => i
                    .labels()
                    .get(&sha_key)
                    .is_some_and(|s| normalize_sha(s) == d.sha256_normalized()),
            })
            .map(|i| {
                let labels = i.labels();
                LocalImage {
                    reference: i.configuration.name.clone(),
                    rootfs_sha256: labels.get(&sha_key).map(|s| normalize_sha(s)),
                    imported_from: labels.get(&from_key).cloned(),
                }
            })
            .collect()
    }

    /// Whether this machine came from the entry's install source.
    fn machine_matches(&self, source: catalog::Source<'_>, m: &MachineState) -> bool {
        m.image()
            .is_some_and(|image| self.source_matches_image(source, image))
    }

    /// Whether this distro came from the entry's install source.
    /// Journal provenance (`catalog:<name>`) is authoritative when
    /// present — it still identifies the entry if the generated image
    /// or its labels are gone — otherwise the image is traced as above.
    fn distro_matches(
        &self,
        e: &catalog::Entry,
        source: catalog::Source<'_>,
        d: &ops::DistroSummary,
    ) -> bool {
        let catalog_source = format!("catalog:{}", e.name);
        d.source.as_deref() == Some(catalog_source.as_str())
            || d.image
                .as_deref()
                .is_some_and(|image| self.source_matches_image(source, image))
    }

    /// Machines matching an entry's install source.
    fn matching_machines(&self, source: catalog::Source<'_>) -> Vec<LocalMachine> {
        self.machines
            .iter()
            .filter(|m| self.machine_matches(source, m))
            .map(MachineState::local)
            .collect()
    }

    /// Distros matching an entry's install source or catalog provenance.
    fn matching_distros(
        &self,
        e: &catalog::Entry,
        source: catalog::Source<'_>,
    ) -> Vec<ops::DistroSummary> {
        self.distros
            .values()
            .filter(|d| self.distro_matches(e, source, d))
            .cloned()
            .collect()
    }

    /// Machine/distro pairs sharing a name that matters to this entry:
    /// the catalog's default install name, or a name whose machine or
    /// distro already traces to the entry's source.
    fn name_conflicts(&self, e: &catalog::Entry, source: catalog::Source<'_>) -> Vec<NameConflict> {
        let expected_name = container::default_machine_name(&e.name);
        self.machines
            .iter()
            .filter_map(|m| {
                let d = self.distros.get(&m.machine.id)?;
                let relevant = d.id == expected_name
                    || self.machine_matches(source, m)
                    || self.distro_matches(e, source, d);
                relevant.then(|| NameConflict {
                    name: d.id.clone(),
                    machine: m.local(),
                    distro: d.clone(),
                })
            })
            .collect()
    }

    /// The local state for one catalog entry, against `arch`'s
    /// resolution of it (`amd64` picks the x86-64 source).
    pub fn status_of(&self, e: &catalog::Entry, arch: Arch) -> catalog::EntryStatus {
        let mut st = catalog::EntryStatus::default();
        let Some(source) = e.source(arch.as_str()) else {
            return st;
        };
        let machines: Vec<_> = self
            .machines
            .iter()
            .filter(|m| self.machine_matches(source, m))
            .collect();
        let distros = self.matching_distros(e, source);
        // Conflict annotation needs name-set lookups in both directions;
        // build each matched-name set once rather than re-scanning.
        let distro_names: BTreeSet<&str> = distros.iter().map(|d| d.id.as_str()).collect();
        let machine_names: BTreeSet<&str> =
            machines.iter().map(|m| m.machine.id.as_str()).collect();
        for m in &machines {
            let name = if distro_names.contains(m.machine.id.as_str()) {
                format!("{} (machine)", m.machine.id)
            } else {
                m.machine.id.clone()
            };
            st.instances.push((name, m.is_running()));
        }
        for d in distros {
            let running = d.is_running();
            let name = if machine_names.contains(d.id.as_str()) {
                format!("{} (distro)", d.id)
            } else {
                d.id
            };
            st.instances.push((name, running));
        }
        st.local = self.local_state(source);
        st
    }
}

/// A catalog download in output form: the fields from the catalog plus
/// the normalized SHA-256 used for cache filenames and image labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadInfo {
    pub url: String,
    pub sha256: String,
    pub sha256_normalized: String,
}

impl From<&catalog::Download> for DownloadInfo {
    fn from(d: &catalog::Download) -> Self {
        Self {
            url: d.url.clone(),
            sha256: d.sha256.clone(),
            sha256_normalized: d.sha256_normalized(),
        }
    }
}

/// The host-selected install source for a catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SourceInfo {
    Image {
        image: String,
    },
    Wsl {
        url: String,
        sha256: String,
        #[serde(rename = "sha256Normalized")]
        sha256_normalized: String,
    },
}

impl SourceInfo {
    fn from(source: catalog::Source<'_>) -> Self {
        match source {
            catalog::Source::Image(image) => Self::Image {
                image: image.to_string(),
            },
            catalog::Source::Wsl(d) => Self::Wsl {
                url: d.url.clone(),
                sha256: d.sha256.clone(),
                sha256_normalized: d.sha256_normalized(),
            },
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Image { .. } => "image",
            Self::Wsl { .. } => ".wsl",
        }
    }
}

/// A verified `.wsl` currently in the download cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    pub sha256: String,
    pub path: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_name: Option<String>,
}

impl From<&wsl::CacheEntry> for CacheInfo {
    fn from(e: &wsl::CacheEntry) -> Self {
        Self {
            sha256: e.sha256.clone(),
            path: e.path.display().to_string(),
            size_bytes: e.size,
            modified_unix: e
                .modified
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            url: e.meta.as_ref().map(|m| m.url.clone()),
            catalog_name: e.meta.as_ref().map(|m| m.name.clone()),
        }
    }
}

/// A local image matching the catalog entry's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImage {
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rootfs_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
}

/// A local machine matching the catalog entry's source.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMachine {
    pub id: String,
    pub status: String,
    pub default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_mount: Option<String>,
}

/// A machine and distro that share an operationally relevant name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameConflict {
    pub name: String,
    pub machine: LocalMachine,
    pub distro: ops::DistroSummary,
}

/// Everything local that corresponds to a catalog entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalInfo {
    /// `pulled`, `cached`, `downloading`, or `absent`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<LocalImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machines: Vec<LocalMachine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub distros: Vec<ops::DistroSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<NameConflict>,
}

/// Catalog data for one entry plus the local state derived from it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntryInfo {
    pub name: String,
    pub friendly_name: String,
    /// The entry's own `Default` flag.
    pub default: bool,
    /// Whether the resolved catalog default selects this entry.
    pub catalog_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amd64_url: Option<DownloadInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm64_url: Option<DownloadInfo>,
    pub host_arch: String,
    pub installable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,
    pub local: LocalInfo,
}

fn info_for_entry(
    cat: &catalog::Catalog,
    entry: &catalog::Entry,
    probe: &StatusProbe,
) -> CatalogEntryInfo {
    let arch = oci::host_arch();
    let source = entry.source(arch);
    let local_state = source.map_or(catalog::Local::No, |s| probe.local_state(s));
    let state = match (local_state, source) {
        (catalog::Local::Yes, Some(catalog::Source::Image(_))) => "pulled",
        (catalog::Local::Yes, _) => "cached",
        (catalog::Local::Downloading, _) => "downloading",
        _ => "absent",
    };
    let cache = match source {
        Some(catalog::Source::Wsl(d)) => probe
            .cached
            .iter()
            .find(|e| e.sha256 == d.sha256_normalized())
            .map(CacheInfo::from),
        _ => None,
    };
    CatalogEntryInfo {
        name: entry.name.clone(),
        friendly_name: entry.friendly_name.clone(),
        default: entry.default,
        catalog_default: cat
            .default_entry()
            .is_some_and(|d| d.name.eq_ignore_ascii_case(&entry.name)),
        image: entry.image.clone(),
        amd64_url: entry.amd64_url.as_ref().map(DownloadInfo::from),
        arm64_url: entry.arm64_url.as_ref().map(DownloadInfo::from),
        host_arch: arch.to_string(),
        installable: source.is_some(),
        source: source.map(SourceInfo::from),
        local: LocalInfo {
            state,
            cache,
            images: source.map_or_else(Vec::new, |s| probe.matching_images(s)),
            machines: source.map_or_else(Vec::new, |s| probe.matching_machines(s)),
            distros: source.map_or_else(Vec::new, |s| probe.matching_distros(entry, s)),
            conflicts: source.map_or_else(Vec::new, |s| probe.name_conflicts(entry, s)),
        },
    }
}

/// Details for one catalog entry and its local cache/image/distro
/// state. Like the verbose listing, local probes are best-effort and do
/// not start `container` services. Machines are included so a same-name
/// machine/distro conflict is visible instead of shadowed.
pub fn info(cat: &catalog::Catalog, name: &str) -> Result<CatalogEntryInfo> {
    let entry = catalog::lookup(&cat.entries, name)
        .with_context(|| format!("{name:?} is not in the distro catalog"))?;
    let probe = StatusProbe::gather(true, true);
    Ok(info_for_entry(cat, entry, &probe))
}

fn download_rows(rows: &mut Vec<Vec<String>>, prefix: &str, d: Option<&DownloadInfo>) {
    match d {
        Some(d) => {
            rows.push(vec![format!("{prefix} URL"), d.url.clone()]);
            rows.push(vec![format!("{prefix} SHA-256"), d.sha256.clone()]);
        }
        None => rows.push(vec![format!("{prefix} URL"), "-".to_string()]),
    }
}

/// Text form of [`CatalogEntryInfo`].
#[must_use]
pub fn render_info(info: &CatalogEntryInfo) -> String {
    let dash = "-".to_string();
    let mut rows = vec![
        vec!["NAME".to_string(), info.name.clone()],
        vec!["FRIENDLY NAME".to_string(), info.friendly_name.clone()],
        vec!["ENTRY DEFAULT".to_string(), info.default.to_string()],
        vec![
            "CATALOG DEFAULT".to_string(),
            info.catalog_default.to_string(),
        ],
        vec!["HOST ARCH".to_string(), info.host_arch.clone()],
        vec!["INSTALLABLE".to_string(), info.installable.to_string()],
        vec![
            "SOURCE".to_string(),
            info.source
                .as_ref()
                .map_or_else(|| dash.clone(), |s| s.kind().to_string()),
        ],
        vec![
            "IMAGE".to_string(),
            info.image.clone().unwrap_or_else(|| dash.clone()),
        ],
    ];
    download_rows(&mut rows, "AMD64", info.amd64_url.as_ref());
    download_rows(&mut rows, "ARM64", info.arm64_url.as_ref());
    if let Some(SourceInfo::Wsl { url, sha256, .. }) = &info.source {
        rows.push(vec!["SOURCE URL".to_string(), url.clone()]);
        rows.push(vec!["SOURCE SHA-256".to_string(), sha256.clone()]);
    }
    rows.push(vec!["LOCAL".to_string(), info.local.state.to_string()]);
    if let Some(cache) = &info.local.cache {
        rows.push(vec!["CACHE PATH".to_string(), cache.path.clone()]);
        rows.push(vec![
            "CACHE SIZE".to_string(),
            table::human_bytes(cache.size_bytes),
        ]);
        if let Some(url) = &cache.url {
            rows.push(vec!["CACHE SOURCE".to_string(), url.clone()]);
        }
    }

    let mut out = String::new();
    for line in table::columns(&rows, 3) {
        out.push_str(&line);
        out.push('\n');
    }
    if !info.local.images.is_empty() {
        out.push_str("\nImages:\n");
        for image in &info.local.images {
            out.push_str(&format!("  {}", image.reference));
            let mut details = Vec::new();
            if let Some(sha) = &image.rootfs_sha256 {
                details.push(format!("rootfs-sha256={sha}"));
            }
            if let Some(from) = &image.imported_from {
                details.push(format!("imported-from={from}"));
            }
            if !details.is_empty() {
                out.push_str(&format!(" ({})", details.join(", ")));
            }
            out.push('\n');
        }
    }
    if !info.local.machines.is_empty() {
        out.push_str("\nMachines:\n");
        for machine in &info.local.machines {
            out.push_str(&format!("  {}: {}", machine.id, machine.status));
            let mut details = Vec::new();
            if let Some(image) = &machine.image {
                details.push(image.clone());
            }
            if let Some(ip) = &machine.ip_address {
                details.push(format!("ip={ip}"));
            }
            if !details.is_empty() {
                out.push_str(&format!(" ({})", details.join(", ")));
            }
            out.push('\n');
        }
    }
    if !info.local.distros.is_empty() {
        out.push_str("\nDistros:\n");
        for distro in &info.local.distros {
            let source = distro
                .source
                .clone()
                .or_else(|| distro.image.clone())
                .unwrap_or_else(|| dash.clone());
            out.push_str(&format!(
                "  {}: {} ({})\n",
                distro.id, distro.status, source
            ));
        }
    }
    if !info.local.conflicts.is_empty() {
        out.push_str("\nName conflicts:\n");
        for conflict in &info.local.conflicts {
            out.push_str(&format!(
                "  {}: machine ({}) and distro ({})\n",
                conflict.name, conflict.machine.status, conflict.distro.status
            ));
        }
    }
    out
}

/// Render the installable catalog for `arch`, optionally with local
/// state. A plain listing never touches `container`; verbose probes
/// are best-effort and never block.
pub fn render_online(
    catalog_file: Option<&Path>,
    verbosity: u8,
    include_machines: bool,
    include_distros: bool,
    install_command: &str,
    arch: Arch,
) -> Result<String> {
    let cat = catalog::load(catalog_file)?;
    if verbosity == 0 {
        return Ok(catalog::render_with_command(
            &cat.entries,
            arch.as_str(),
            install_command,
        ));
    }
    let probe = StatusProbe::gather(include_machines, include_distros);
    let status: Vec<catalog::EntryStatus> = cat
        .entries
        .iter()
        .map(|e| probe.status_of(e, arch))
        .collect();
    Ok(catalog::render_verbose_with_command(
        &cat.entries,
        arch.as_str(),
        &status,
        verbosity,
        install_command,
    ))
}

/// Render the `.wsl` download cache: which catalog entry each file is
/// for and which image it was imported as. Cache files are named by
/// their verified SHA-256 — the same hash lands on the imported image's
/// `rootfs-sha256` label, which is how the two link.
pub fn render_cache(catalog_file: Option<&Path>) -> String {
    let cached = wsl::cache_entries();
    let cat = catalog::load(catalog_file).ok();
    let images = container::list_images().unwrap_or_default();
    let sha_key = naming::label_key(LABEL_ROOTFS_SHA256);

    let mut out = format!(
        "Cached .wsl downloads ({})\n",
        wsl::cache_dir().map_or_else(|| "?".into(), |p| p.display().to_string())
    );
    if cached.is_empty() {
        out.push_str("  (empty)\n");
        return out;
    }
    let mut rows = vec![vec![
        "SHA256".into(),
        "SIZE".into(),
        "CATALOG ENTRY".into(),
        "IMPORTED AS".into(),
    ]];
    for e in &cached {
        // Prefer the catalog's own name for this SHA; fall back to the
        // name recorded in the lockfile at fetch time, marking it when
        // the catalog still knows the name but at a different SHA —
        // i.e. a superseded build.
        let entry_name = cat
            .as_ref()
            .and_then(|c| {
                c.entries
                    .iter()
                    .find(|en| {
                        [&en.amd64_url, &en.arm64_url]
                            .into_iter()
                            .flatten()
                            .any(|d| d.sha256_normalized() == e.sha256)
                    })
                    .map(|en| en.name.clone())
            })
            .or_else(|| {
                let meta = e.meta.as_ref()?;
                let superseded = cat.as_ref().is_some_and(|c| {
                    c.entries
                        .iter()
                        .any(|en| en.name.eq_ignore_ascii_case(&meta.name))
                });
                Some(if superseded {
                    format!("{} (superseded)", meta.name)
                } else {
                    meta.name.clone()
                })
            })
            .unwrap_or_default();
        let image_ref = images
            .iter()
            .filter(|i| i.labels().get(&sha_key) == Some(&e.sha256))
            .map(|i| i.configuration.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        rows.push(vec![
            format!("{}…", &e.sha256[..12.min(e.sha256.len())]),
            format!("{} MiB", e.size >> 20),
            entry_name,
            image_ref,
        ]);
    }
    for line in table::columns(&rows, 3) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> catalog::Catalog {
        catalog::load(None).unwrap()
    }

    #[test]
    fn catalog_install_names_after_the_entry() {
        let cat = builtin();
        // The default entry (Ubuntu) is a .wsl install on this host —
        // upstream publishes arm64+amd64 downloads.
        let Resolved::Wsl {
            name, requested, ..
        } = resolve(&cat, None, Arch::host(), false).unwrap()
        else {
            panic!("the catalog default should resolve to a .wsl download");
        };
        assert_eq!(name, "Ubuntu");
        assert_eq!(container::default_machine_name(name), "ubuntu");
        // Catalog entries relax the home-mount default to rw — a
        // `source` intent, so an explicit flag or a tighter preset
        // still beats it.
        assert_eq!(requested.home_mount, Some(HomeMount::Rw));
        for arg in ["ubuntu", "UBUNTU-24.04"] {
            let Resolved::Wsl { name, .. } = resolve(&cat, Some(arg), Arch::host(), false).unwrap()
            else {
                panic!("{arg} should resolve to a .wsl download");
            };
            assert!(!name.is_empty(), "{arg}");
        }
        // An overlay entry with no .wsl download still resolves to
        // its image.
        let Resolved::Image { image, name, .. } =
            resolve(&cat, Some("alpine"), Arch::host(), false).unwrap()
        else {
            panic!("Alpine should resolve to an image");
        };
        assert_eq!(image, "docker.io/library/alpine:latest");
        assert_eq!(name, Some("Alpine"));
    }

    #[test]
    fn image_fallback_only_when_enabled() {
        let cat = builtin();
        let Resolved::Image {
            image,
            name,
            requested,
        } = resolve(&cat, Some("quay.io/x/y:1"), Arch::host(), true).unwrap()
        else {
            panic!("a qualified ref should fall back to an image");
        };
        assert_eq!(image, "quay.io/x/y:1");
        assert_eq!(name, None);
        // A bare image asked for nothing — no source intent.
        assert!(requested.is_empty());
        let err = resolve(&cat, Some("quay.io/x/y:1"), Arch::host(), false).unwrap_err();
        assert!(err.to_string().contains("not in the distro catalog"));
    }

    #[test]
    fn wsl_catalog_entries_keep_their_name() {
        let cat = builtin();
        let Resolved::Wsl { name, .. } =
            resolve(&cat, Some("rlcplus-9"), Arch::host(), false).unwrap()
        else {
            panic!("RLCPlus-9 should resolve to a .wsl download");
        };
        assert_eq!(name, "RLCPlus-9");
    }

    #[test]
    fn entry_info_serializes_catalog_and_local_state() {
        let cat = builtin();
        // An image-sourced entry keeps the machine/distro matching
        // by image reference.
        let entry = catalog::lookup(&cat.entries, "alpine").unwrap();
        let probe = StatusProbe {
            cached: Vec::new(),
            images: Vec::new(),
            machines: vec![MachineState {
                machine: Machine {
                    id: "dev".into(),
                    status: "running".into(),
                    default: false,
                    ip_address: None,
                    cpus: None,
                    memory: None,
                    disk_size: None,
                },
                detail: Some(MachineDetail {
                    id: "dev".into(),
                    status: Some("running".into()),
                    container_id: None,
                    user_setup: None,
                    home_mount: None,
                    platform: None,
                    image: Some(container::ImageInfo {
                        reference: "docker.io/library/alpine:latest".into(),
                    }),
                    cpus: None,
                    memory: None,
                }),
            }],
            distros: BTreeMap::from([(
                "dev".to_string(),
                ops::DistroSummary {
                    id: "dev".into(),
                    status: "running".into(),
                    image: Some("docker.io/library/alpine:latest".into()),
                    ..ops::DistroSummary::default()
                },
            )]),
        };
        let info = info_for_entry(&cat, entry, &probe);
        assert_eq!(info.name, "Alpine");
        assert!(!info.catalog_default);
        assert_eq!(info.local.machines.len(), 1);
        assert_eq!(info.local.distros.len(), 1);
        assert_eq!(info.local.conflicts.len(), 1);
        assert_eq!(
            probe.status_of(entry, Arch::Arm64).instances,
            vec![
                ("dev (machine)".to_string(), true),
                ("dev (distro)".to_string(), true),
            ]
        );

        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["source"]["type"], "image");
        assert_eq!(json["source"]["image"], "docker.io/library/alpine:latest");
        assert_eq!(json["local"]["state"], "absent");
        assert_eq!(json["local"]["machines"][0]["id"], "dev");
        assert_eq!(json["local"]["distros"][0]["id"], "dev");
        assert_eq!(json["local"]["conflicts"][0]["name"], "dev");
    }

    #[test]
    fn entry_info_reports_the_selected_wsl_source_and_cache() {
        let cat = builtin();
        let entry = catalog::lookup(&cat.entries, "RLCPlus-10").unwrap();
        let catalog::Source::Wsl(download) = entry.source(oci::host_arch()).unwrap() else {
            panic!("RLCPlus-10 should have a .wsl source on this host");
        };
        let probe = StatusProbe {
            cached: vec![wsl::CacheEntry {
                sha256: download.sha256_normalized(),
                path: Path::new("/cache/rocky.wsl").to_path_buf(),
                size: 42,
                modified: Some(UNIX_EPOCH),
                meta: Some(wsl::CacheMeta {
                    url: download.url.clone(),
                    name: entry.name.clone(),
                }),
            }],
            images: Vec::new(),
            machines: Vec::new(),
            distros: BTreeMap::from([(
                "rocky".to_string(),
                ops::DistroSummary {
                    id: "rocky".into(),
                    status: "stopped".into(),
                    source: Some("catalog:RLCPlus-10".into()),
                    ..ops::DistroSummary::default()
                },
            )]),
        };
        let info = info_for_entry(&cat, entry, &probe);
        assert_eq!(info.local.distros[0].id, "rocky");
        assert_eq!(info.local.state, "cached");
        assert_eq!(info.local.cache.as_ref().unwrap().path, "/cache/rocky.wsl");
        assert_eq!(info.local.cache.as_ref().unwrap().size_bytes, 42);

        let json = serde_json::to_value(&info).unwrap();
        assert!(json["source"]["sha256Normalized"].is_string());
        assert!(json["source"]["sha256_normalized"].is_null());

        let SourceInfo::Wsl { url, .. } = info.source.unwrap() else {
            panic!("expected .wsl source info");
        };
        assert_eq!(url, download.url);
    }

    #[test]
    fn render_info_includes_catalog_sources_and_local_rows() {
        let cat = builtin();
        let entry = catalog::lookup(&cat.entries, "RLCPlus-10").unwrap();
        let probe = StatusProbe {
            cached: Vec::new(),
            images: Vec::new(),
            machines: Vec::new(),
            distros: BTreeMap::new(),
        };
        let out = render_info(&info_for_entry(&cat, entry, &probe));
        assert!(out.contains("NAME"));
        assert!(out.contains("RLCPlus-10"));
        assert!(out.contains("ARM64 URL"));
        assert!(out.contains("https://"));
        assert!(out.contains("LOCAL"));
    }
}
