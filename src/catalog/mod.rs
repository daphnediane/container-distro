/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

pub mod wsl;

mod info;
mod install;
#[cfg(test)]
mod test_support;

use crate::util::table;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

pub use info::{
    CacheInfo, CatalogEntryInfo, DownloadInfo, LocalImage, LocalInfo, LocalMachine, NameConflict,
    SourceInfo, StatusProbe, info, render_cache, render_info, render_online,
};
pub use install::{Resolved, import_file, install, install_resolved, resolve};

/// The compiled-in overlay — the same schema `--catalog` accepts.
/// Merged over [`UPSTREAM`] for the built-in catalog.
const OVERLAY: &str = include_str!("catalog.json");

/// The pinned, vendored copy of Microsoft's WSL distro catalog,
/// `DistributionInfo.json` from `microsoft/WSL` (`vendor/microsoft/`,
/// with its MIT `LICENSE` and the upstream commit in `UPSTREAM`).
/// Verbatim — edit via `scripts/refresh-wsl-catalog.sh`, not by hand.
const UPSTREAM: &str = include_str!("../../vendor/microsoft/DistributionInfo.json");

/// A `.wsl` rootfs download: a URL plus the SHA-256 the download is
/// verified against. The hash is mandatory — a verified download of a
/// remote rootfs is the trust equivalent of an image pull.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Download {
    pub url: String,
    pub sha256: String,
}

impl Download {
    /// The hash as lowercase hex with any `0x` prefix stripped —
    /// the form used for cache filenames.
    #[must_use]
    pub fn sha256_normalized(&self) -> String {
        self.sha256
            .strip_prefix("0x")
            .or_else(|| self.sha256.strip_prefix("0X"))
            .unwrap_or(&self.sha256)
            .to_lowercase()
    }
}

/// One installable distribution, the WSL catalog's
/// `ModernDistributions` entry shape plus our `Image` extension.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Entry {
    /// Install key, matching WSL's NAME column (`cm --install <name>`).
    /// Lookup is case-insensitive; no `/`, `:`, or `@`.
    pub name: String,
    /// Display name for the FRIENDLY NAME column.
    #[serde(default)]
    pub friendly_name: String,
    /// Preferred for a bare `--install` when the file's top-level
    /// `Default` doesn't name an entry.
    #[serde(default)]
    pub default: bool,
    /// Our extension: a fully-qualified OCI image reference, used
    /// when the entry has no `.wsl` download for the host arch —
    /// the verified-download path is preferred where both exist.
    #[serde(default)]
    pub image: Option<String>,
    /// `.wsl` rootfs downloads per arch, the WSL catalog's shape.
    #[serde(default)]
    pub amd64_url: Option<Download>,
    #[serde(default)]
    pub arm64_url: Option<Download>,
    /// Our extension: the `--home-mount` default when installing this
    /// entry (`rw`/`ro`/`none`; absent → `rw`, since installing from a
    /// catalog — built-in or `--catalog FILE` — is an act of trust in
    /// its curation). A catalog can pin a tighter default for entries
    /// it wants to mark semi-trusted.
    #[serde(default)]
    pub home_mount: Option<crate::distro::spec::HomeMount>,
}

/// What an entry installs from on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    /// An OCI image reference (`container` pulls it).
    Image(&'a str),
    /// A `.wsl` rootfs download (we fetch and verify it).
    Wsl(&'a Download),
}

impl Entry {
    /// The install source for `arch` (`"arm64"`/`"amd64"`): the `.wsl`
    /// download when one exists for the arch — a verified, cached
    /// fetch — else the `Image`, else `None` when the entry offers
    /// nothing this host can use.
    #[must_use]
    pub fn source(&self, arch: &str) -> Option<Source<'_>> {
        let dl = match arch {
            "arm64" | "aarch64" => &self.arm64_url,
            "amd64" | "x86_64" => &self.amd64_url,
            _ => &None,
        };
        dl.as_ref()
            .map(Source::Wsl)
            .or_else(|| self.image.as_deref().map(Source::Image))
    }

    /// Whether the entry can be installed on `arch`.
    #[must_use]
    pub fn installable(&self, arch: &str) -> bool {
        self.source(arch).is_some()
    }
}

/// A resolved catalog: the entries plus the bare-`--install` default.
#[derive(Debug)]
pub struct Catalog {
    /// Top-level `Default` from the file, when present.
    pub default_name: Option<String>,
    /// `ModernDistributions` entries, in file order (vendors first,
    /// then their entries).
    pub entries: Vec<Entry>,
}

impl Catalog {
    /// The entry a bare `cm --install` creates: the file's `Default`,
    /// else the first `Default: true` entry, else the first entry.
    #[must_use]
    pub fn default_entry(&self) -> Option<&Entry> {
        self.default_name
            .as_deref()
            .and_then(|n| lookup(&self.entries, n))
            .or_else(|| self.entries.iter().find(|e| e.default))
            .or_else(|| self.entries.first())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CatalogFile {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    modern_distributions: serde_json::Map<String, serde_json::Value>,
    // The WSL catalog's legacy `Distributions` array (Store appx
    // packages) is intentionally ignored.
}

fn parse(text: &str, source: &str) -> Result<Catalog> {
    let file: CatalogFile =
        serde_json::from_str(text).with_context(|| format!("invalid catalog {source}"))?;
    let mut entries = Vec::new();
    for (vendor, value) in &file.modern_distributions {
        let list: Vec<Entry> = serde_json::from_value(value.clone())
            .with_context(|| format!("invalid catalog {source}: vendor {vendor:?}"))?;
        entries.extend(list);
    }
    let mut seen = BTreeSet::new();
    for e in &entries {
        if e.name.is_empty() || e.name.contains(['/', ':', '@']) {
            bail!("catalog {source}: invalid NAME {:?}", e.name);
        }
        if !seen.insert(e.name.to_lowercase()) {
            bail!("catalog {source}: duplicate NAME {:?}", e.name);
        }
        if e.image.is_none() && e.amd64_url.is_none() && e.arm64_url.is_none() {
            bail!("catalog {source}: {:?} has no image or download", e.name);
        }
        if let Some(i) = &e.image
            && i.is_empty()
        {
            bail!("catalog {source}: {:?} has an empty image", e.name);
        }
    }
    Ok(Catalog {
        default_name: file.default,
        entries,
    })
}

/// Merge `overlay` over `base` for the built-in catalog. An overlay
/// entry matching a base name (case-insensitive) only fills fields
/// the base entry lacks — upstream's URL/SHA/friendly-name fields
/// always win, so a stale overlay can't silently downgrade a
/// download — plus its `Default` flag; a non-matching entry appends.
/// The overlay's top-level `Default` wins when it names an entry.
fn merge(base: Catalog, overlay: Catalog) -> Catalog {
    let mut entries = base.entries;
    for e in overlay.entries {
        if let Some(m) = entries
            .iter_mut()
            .find(|x| x.name.eq_ignore_ascii_case(&e.name))
        {
            if m.image.is_none() {
                m.image = e.image;
            }
            if m.amd64_url.is_none() {
                m.amd64_url = e.amd64_url;
            }
            if m.arm64_url.is_none() {
                m.arm64_url = e.arm64_url;
            }
            if m.friendly_name.is_empty() {
                m.friendly_name = e.friendly_name;
            }
            if m.home_mount.is_none() {
                m.home_mount = e.home_mount;
            }
            m.default |= e.default;
        } else {
            entries.push(e);
        }
    }
    Catalog {
        default_name: overlay.default_name.or(base.default_name),
        entries,
    }
}

/// The catalog to use: `path`'s JSON, or the built-in merge of the
/// vendored WSL catalog and our overlay.
pub fn load(path: Option<&Path>) -> Result<Catalog> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("cannot read catalog {}", p.display()))?;
            parse(&text, &p.display().to_string())
        }
        None => {
            let base = parse(UPSTREAM, "vendored WSL catalog")?;
            let overlay = parse(OVERLAY, "built-in overlay")?;
            Ok(merge(base, overlay))
        }
    }
}

/// Find an entry by name, case-insensitive (`kali-Linux` matches
/// `kali-linux`, like WSL's `--install` NAME matching).
#[must_use]
pub fn lookup<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
}

/// The header above both catalog tables.
fn header(install_command: &str) -> String {
    format!(
        "The following is a list of valid distributions that can be installed.\n\
         Install using '{install_command}'.\n\n"
    )
}

/// `wsl --list --online`-shaped output: a two-line header then a
/// `NAME FRIENDLY NAME` table — the format remote-WSL editor extensions
/// scrape. Entries not installable on `arch` are hidden, like WSL
/// on ARM hiding amd64-only distributions.
#[must_use]
pub fn render_with_command(entries: &[Entry], arch: &str, install_command: &str) -> String {
    let mut out = header(install_command);
    let mut rows = vec![vec!["NAME".to_string(), "FRIENDLY NAME".to_string()]];
    for e in entries.iter().filter(|e| e.installable(arch)) {
        rows.push(vec![e.name.clone(), e.friendly_name.clone()]);
    }
    for line in table::columns(&rows, 3) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// [`render_with_command`] with `cm`'s install syntax.
#[must_use]
pub fn render(entries: &[Entry], arch: &str) -> String {
    render_with_command(entries, arch, "cm --install <Distro>")
}

/// Whether an entry's install source is present locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Local {
    /// Not present, or undetermined (e.g. `container` services down).
    #[default]
    No,
    /// The image is pulled, or the `.wsl` sits verified in the cache.
    Yes,
    /// A `.wsl` fetch is in flight in another process.
    Downloading,
}

/// Local state for one catalog entry, for [`render_verbose`]. The
/// caller knows about the download cache, pulled images, and installed
/// machines/distros; the default renders as "nothing local".
#[derive(Debug, Clone, Default)]
pub struct EntryStatus {
    /// Whether the entry's install source is already on disk.
    pub local: Local,
    /// Machines/distros built from this entry, as `(name, running)`.
    pub instances: Vec<(String, bool)>,
}

/// The catalog table plus local state per entry —
/// `LOCAL` reports a `pulled` image or `cached` `.wsl` (`downloading`
/// while another process is fetching it), and `INSTANCES` lists the
/// machines/distros installed from the entry, marking running ones.
/// `status` is parallel to `entries`. `verbosity >= 2` adds `SOURCE`
/// (`image`/`.wsl`) and `REF` (the image reference or download URL).
#[must_use]
pub fn render_verbose_with_command(
    entries: &[Entry],
    arch: &str,
    status: &[EntryStatus],
    verbosity: u8,
    install_command: &str,
) -> String {
    debug_assert_eq!(entries.len(), status.len());
    let mut out = header(install_command);
    let dash = || "-".to_string();
    let mut header = vec!["NAME".to_string(), "FRIENDLY NAME".to_string()];
    if verbosity >= 2 {
        header.extend(["SOURCE".to_string(), "REF".to_string()]);
    }
    header.extend(["LOCAL".to_string(), "INSTANCES".to_string()]);
    let mut rows = vec![header];
    for (e, st) in entries
        .iter()
        .zip(status)
        .filter(|(e, _)| e.installable(arch))
    {
        let local = match st.local {
            Local::Yes => match e.source(arch) {
                Some(Source::Image(_)) => "pulled".to_string(),
                _ => "cached".to_string(),
            },
            Local::Downloading => "downloading".to_string(),
            Local::No => dash(),
        };
        let instances = if st.instances.is_empty() {
            dash()
        } else {
            st.instances
                .iter()
                .map(|(n, running)| {
                    if *running {
                        format!("{n} (running)")
                    } else {
                        n.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut row = vec![e.name.clone(), e.friendly_name.clone()];
        if verbosity >= 2 {
            let (kind, r) = match e.source(arch) {
                Some(Source::Image(i)) => ("image".to_string(), i.to_string()),
                Some(Source::Wsl(d)) => (".wsl".to_string(), d.url.clone()),
                None => (dash(), dash()),
            };
            row.extend([kind, r]);
        }
        row.extend([local, instances]);
        rows.push(row);
    }
    for line in table::columns(&rows, 3) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// [`render_verbose_with_command`] with `cm`'s install syntax.
#[must_use]
pub fn render_verbose(
    entries: &[Entry],
    arch: &str,
    status: &[EntryStatus],
    verbosity: u8,
) -> String {
    render_verbose_with_command(entries, arch, status, verbosity, "cm --install <Distro>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_and_defaults_to_ubuntu() {
        let cat = load(None).unwrap();
        assert_eq!(cat.default_entry().unwrap().name, "Ubuntu");
        assert!(cat.entries.len() > 10);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let cat = load(None).unwrap();
        let u = lookup(&cat.entries, "Ubuntu-24.04").unwrap();
        assert!(u.image.is_none());
        assert!(u.arm64_url.is_some());
        assert_eq!(lookup(&cat.entries, "ubuntu").unwrap().name, "Ubuntu");
        assert_eq!(
            lookup(&cat.entries, "KALI-LINUX").unwrap().name,
            "kali-linux"
        );
        assert_eq!(lookup(&cat.entries, "alpine").unwrap().name, "Alpine");
        assert!(lookup(&cat.entries, "nixos").is_none());
    }

    #[test]
    fn builtin_merges_upstream_and_overlay() {
        let cat = load(None).unwrap();
        // `archlinux` collides: upstream's amd64 `.wsl` is kept and
        // the overlay's `Image` fills the missing arm64 source.
        let arch = lookup(&cat.entries, "archlinux").unwrap();
        assert!(arch.amd64_url.is_some());
        assert_eq!(
            arch.image.as_deref(),
            Some("docker.io/library/archlinux:latest")
        );
        match arch.source("amd64").unwrap() {
            Source::Wsl(_) => {}
            Source::Image(_) => panic!("amd64 should prefer the .wsl download"),
        }
        match arch.source("arm64").unwrap() {
            Source::Image(i) => assert!(i.contains("archlinux")),
            Source::Wsl(_) => panic!("arm64 has no .wsl — the Image fills in"),
        }
        // Overlay-only entries append after the upstream list.
        let alpine = lookup(&cat.entries, "Alpine").unwrap();
        assert!(matches!(alpine.source("arm64"), Some(Source::Image(_))));
        assert!(cat.entries.len() > 20);
        // Upstream entries the overlay doesn't touch come through
        // verbatim — Ubuntu is a `.wsl` install on both arches.
        let ubuntu = lookup(&cat.entries, "Ubuntu").unwrap();
        assert!(matches!(ubuntu.source("arm64"), Some(Source::Wsl(_))));
        assert!(matches!(ubuntu.source("amd64"), Some(Source::Wsl(_))));
    }

    #[test]
    fn merge_keeps_base_fields_and_appends_extras() {
        let base = parse(
            r#"{"ModernDistributions":{"v":[
                {"Name":"a","FriendlyName":"base a",
                 "Amd64Url":{"Url":"u1","Sha256":"s1"}},
                {"Name":"b","Image":"base:1"}]},
              "Default":"b"}"#,
            "base",
        )
        .unwrap();
        let overlay = parse(
            r#"{"ModernDistributions":{"v":[
                {"Name":"a","FriendlyName":"overlay a",
                 "Image":"overlay:1",
                 "Amd64Url":{"Url":"u2","Sha256":"s2"},
                 "Arm64Url":{"Url":"u3","Sha256":"s3"},
                 "Default":true},
                {"Name":"c","Image":"overlay:2"}]},
              "Default":"c"}"#,
            "overlay",
        )
        .unwrap();
        let cat = merge(base, overlay);
        assert_eq!(cat.entries.len(), 3);
        let a = lookup(&cat.entries, "a").unwrap();
        assert_eq!(a.friendly_name, "base a");
        assert_eq!(a.image.as_deref(), Some("overlay:1"));
        assert_eq!(a.amd64_url.as_ref().unwrap().url, "u1");
        assert_eq!(a.arm64_url.as_ref().unwrap().url, "u3");
        assert!(a.default);
        assert_eq!(cat.entries[2].name, "c");
        assert_eq!(cat.default_entry().unwrap().name, "c");
    }

    #[test]
    fn source_prefers_the_wsl_download() {
        // An entry offering both resolves to the verified `.wsl` on
        // the arch it covers and to the `Image` elsewhere.
        let e = Entry {
            name: "x".into(),
            friendly_name: String::new(),
            default: false,
            image: Some("img:1".into()),
            amd64_url: Some(Download {
                url: "u".into(),
                sha256: "s".into(),
            }),
            arm64_url: None,
            home_mount: None,
        };
        assert!(matches!(e.source("amd64"), Some(Source::Wsl(_))));
        for arch in ["arm64", "riscv"] {
            // Archs the .wsl doesn't cover fall back to the image —
            // an OCI ref isn't arch-bound.
            assert_eq!(
                match e.source(arch) {
                    Some(Source::Image(i)) => i,
                    _ => panic!("{arch} should fall back to the image"),
                },
                "img:1"
            );
        }
        assert!(Entry { image: None, ..e }.source("arm64").is_none());
    }

    #[test]
    fn wsl_entries_resolve_per_arch() {
        let cat = load(None).unwrap();
        let rlc = lookup(&cat.entries, "RLCPlus-10").unwrap();
        assert!(rlc.image.is_none());
        match rlc.source("arm64").unwrap() {
            Source::Wsl(d) => {
                assert!(d.url.contains("aarch64"));
                assert_eq!(d.sha256.len(), 64);
            }
            Source::Image(_) => panic!("RLCPlus should be a .wsl download"),
        }
        assert!(rlc.installable("amd64"));
    }

    #[test]
    fn wsl_catalog_shape_parses() {
        // A trimmed DistributionInfo.json: ModernDistributions grouped
        // by vendor, top-level Default, plus a legacy Distributions
        // array we ignore.
        // cspell:ignore 9MSVKQC78PK6
        let text = r#"{
          "ModernDistributions": {
            "Ubuntu": [{"Name":"Ubuntu","FriendlyName":"Ubuntu","Default":true,
              "Amd64Url":{"Url":"https://x/ubuntu.wsl","Sha256":"0xabc"},
              "Arm64Url":{"Url":"https://x/ubuntu-arm.wsl","Sha256":"def"}}]
          },
          "Default": "Ubuntu",
          "Distributions": [{"Name":"Debian","StoreAppId":"9MSVKQC78PK6"}]
        }"#;
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), text).unwrap();
        let cat = load(Some(f.path())).unwrap();
        assert_eq!(cat.default_entry().unwrap().name, "Ubuntu");
        let e = &cat.entries[0];
        match e.source("amd64").unwrap() {
            Source::Wsl(d) => assert_eq!(d.sha256, "0xabc"),
            Source::Image(_) => panic!("expected wsl download"),
        }
        assert!(!e.installable("riscv"));
    }

    #[test]
    fn entry_home_mount_override_parses() {
        let text = r#"{"ModernDistributions":{"v":[
            {"Name":"a","Image":"x:1","HomeMount":"ro"},
            {"Name":"b","Image":"x:2","HomeMount":"none"},
            {"Name":"c","Image":"x:3"}
        ]}}"#;
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), text).unwrap();
        let cat = load(Some(f.path())).unwrap();
        use crate::distro::spec::HomeMount;
        assert_eq!(cat.entries[0].home_mount, Some(HomeMount::Ro));
        assert_eq!(cat.entries[1].home_mount, Some(HomeMount::None));
        assert_eq!(cat.entries[2].home_mount, None);
    }

    #[test]
    fn custom_catalog_rejects_bad_entries() {
        for bad in [
            r#"{"ModernDistributions":{"v":[{"Name":"evil/latest","FriendlyName":"x","Image":"a:b"}]}}"#,
            r#"{"ModernDistributions":{"v":[{"Name":"x","FriendlyName":"y"}]}}"#,
            r#"{"ModernDistributions":{"v":[{"Name":"x","Image":"a:b"},{"Name":"X","Image":"a:c"}]}}"#,
        ] {
            let f = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(f.path(), bad).unwrap();
            assert!(load(Some(f.path())).is_err(), "{bad}");
        }
    }

    #[test]
    fn render_filters_by_arch() {
        let cat = load(None).unwrap();
        let out = render(&cat.entries, "arm64");
        let mut lines = out.lines();
        assert!(lines.next().unwrap().starts_with("The following is a list"));
        assert!(lines.next().unwrap().contains("cm --install"));
        assert!(lines.next().unwrap().is_empty());
        assert_eq!(
            lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
            ["NAME", "FRIENDLY", "NAME"]
        );
        assert_eq!(
            lines.next().unwrap().split_whitespace().next().unwrap(),
            "Ubuntu"
        );
        assert!(out.contains("RLCPlus-10"));
        assert!(out.contains("Alpine"));
    }

    #[test]
    fn render_uses_the_calling_frontend_in_its_hint() {
        let cat = load(None).unwrap();
        let out = render_with_command(
            &cat.entries,
            "arm64",
            "container distro catalog install <DISTRO>",
        );
        assert!(
            out.lines()
                .nth(1)
                .unwrap()
                .contains("container distro catalog install")
        );
    }

    #[test]
    fn render_verbose_adds_status_columns() {
        let cat = load(None).unwrap();
        let mut status = vec![EntryStatus::default(); cat.entries.len()];
        let i = cat.entries.iter().position(|e| e.name == "Alpine").unwrap();
        status[i].local = Local::Yes;
        status[i].instances = vec![("dev".into(), true), ("old".into(), false)];
        let i = cat
            .entries
            .iter()
            .position(|e| e.name == "RLCPlus-10")
            .unwrap();
        status[i].local = Local::Downloading;

        let out = render_verbose(&cat.entries, "arm64", &status, 1);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[3].split_whitespace().collect::<Vec<_>>(),
            ["NAME", "FRIENDLY", "NAME", "LOCAL", "INSTANCES"]
        );
        let alpine = lines.iter().find(|l| l.starts_with("Alpine ")).unwrap();
        assert!(alpine.contains("pulled"));
        assert!(alpine.contains("dev (running), old"));
        let rlc = lines.iter().find(|l| l.starts_with("RLCPlus-10")).unwrap();
        assert!(rlc.contains("downloading"));
        let oracle = lines
            .iter()
            .find(|l| l.starts_with("OracleLinux-9"))
            .unwrap();
        assert!(oracle.trim_end().ends_with('-'));
    }

    #[test]
    fn render_very_verbose_adds_source_columns() {
        let cat = load(None).unwrap();
        let status = vec![EntryStatus::default(); cat.entries.len()];
        let out = render_verbose(&cat.entries, "arm64", &status, 2);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[3].split_whitespace().collect::<Vec<_>>(),
            [
                "NAME",
                "FRIENDLY",
                "NAME",
                "SOURCE",
                "REF",
                "LOCAL",
                "INSTANCES"
            ]
        );
        let alpine = lines.iter().find(|l| l.starts_with("Alpine ")).unwrap();
        assert!(alpine.contains("image"));
        assert!(alpine.contains("docker.io/library/alpine:latest"));
        let rlc_entry = cat.entries.iter().find(|e| e.name == "RLCPlus-10").unwrap();
        let rlc = lines.iter().find(|l| l.starts_with("RLCPlus-10")).unwrap();
        assert!(rlc.contains(".wsl"));
        let Source::Wsl(download) = rlc_entry.source("arm64").unwrap() else {
            panic!("RLCPlus-10 should be a .wsl download");
        };
        assert!(rlc.contains(&download.url));
    }
}
