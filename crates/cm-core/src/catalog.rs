/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The installable-distro catalog — the `wsl --list --online` analogue.
//!
//! The schema is Microsoft's `DistributionInfo.json` (the manifest that
//! backs `wsl --list --online`, in the `microsoft/WSL` repo) extended
//! with an `Image` field for OCI image installs, so Microsoft's own
//! file works as a `--catalog` argument. `ModernDistributions` holds
//! the entries; the legacy `Distributions` array (Store appx packages)
//! is ignored.
//!
//! An entry is installable from its `Image` (an OCI ref — a machine
//! or distro, `container`'s choice) or, without one, from the `.wsl`
//! rootfs download matching the host arch — which can only become a
//! distro. Entries installable on neither count as not installable
//! here and are hidden from `-l -o`, matching WSL-on-ARM (which hides
//! entries without `Arm64Url`).
//!
//! The built-in catalog (`catalog.json`, compiled in with
//! `include_str!`) mixes Docker Official/Verified-Publisher images
//! with canonical non-docker.io registries and a few `.wsl` downloads
//! taken verbatim — URL and SHA-256 — from Microsoft's manifest.
//! `cm --catalog FILE` swaps in a user catalog but is an explicit
//! per-invocation flag, never auto-loaded from `$HOME` (security.md:
//! anything under `$HOME` is guest-writable).
//!
//! Entry `Name`s deliberately contain no `/`, `:`, or `@`, so any
//! argument shaped like a real image reference can never collide —
//! passing a qualified ref (or `--from-image`) to `--install` bypasses
//! the catalog.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::table;

/// The compiled-in catalog — the same schema `--catalog` accepts.
const BUILTIN: &str = include_str!("catalog.json");

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

/// One installable distribution, Microsoft's `ModernDistributions`
/// entry shape plus our `Image` extension.
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
    /// Our extension: a fully-qualified OCI image reference. Wins over
    /// the `.wsl` downloads — images can become machines or distros.
    #[serde(default)]
    pub image: Option<String>,
    /// `.wsl` rootfs downloads per arch, Microsoft's shape.
    #[serde(default)]
    pub amd64_url: Option<Download>,
    #[serde(default)]
    pub arm64_url: Option<Download>,
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
    /// The install source for `arch` (`"arm64"`/`"amd64"`), or `None`
    /// when the entry offers nothing this host can use.
    #[must_use]
    pub fn source(&self, arch: &str) -> Option<Source<'_>> {
        if let Some(i) = &self.image {
            return Some(Source::Image(i));
        }
        let dl = match arch {
            "arm64" | "aarch64" => &self.arm64_url,
            "amd64" | "x86_64" => &self.amd64_url,
            _ => return None,
        };
        dl.as_ref().map(Source::Wsl)
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
    // Microsoft's legacy `Distributions` array (Store appx packages)
    // is intentionally ignored.
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

/// The catalog to use: `path`'s JSON, or the built-in list.
pub fn load(path: Option<&Path>) -> Result<Catalog> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("cannot read catalog {}", p.display()))?;
            parse(&text, &p.display().to_string())
        }
        None => parse(BUILTIN, "built-in"),
    }
}

/// Find an entry by name, case-insensitive (`kali-Linux` matches
/// `kali-linux`, like WSL's `--install` NAME matching).
#[must_use]
pub fn lookup<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
}

/// `wsl --list --online`-shaped output: a two-line header then a
/// `NAME FRIENDLY NAME` table — the format remote-WSL editor extensions
/// scrape. Entries not installable on `arch` are hidden, like WSL
/// on ARM hiding amd64-only distributions.
#[must_use]
pub fn render(entries: &[Entry], arch: &str) -> String {
    let mut out = String::from(
        "The following is a list of valid distributions that can be installed.\n\
         Install using 'cm --install <Distro>'.\n\n",
    );
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
        assert_eq!(
            lookup(&cat.entries, "Ubuntu-24.04")
                .unwrap()
                .image
                .as_deref(),
            Some("docker.io/library/ubuntu:24.04")
        );
        assert_eq!(lookup(&cat.entries, "ubuntu").unwrap().name, "Ubuntu");
        assert_eq!(
            lookup(&cat.entries, "KALI-LINUX").unwrap().name,
            "kali-linux"
        );
        assert_eq!(lookup(&cat.entries, "alpine").unwrap().name, "Alpine");
        assert!(lookup(&cat.entries, "nixos").is_none());
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
    fn microsofts_manifest_shape_parses() {
        // A trimmed DistributionInfo.json: ModernDistributions grouped
        // by vendor, top-level Default, plus a legacy Distributions
        // array we ignore.
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
}
