/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The installable-distro catalog — the `wsl --list --online` analogue.
//!
//! A baked-in JSON table (`catalog.json`, compiled in with
//! `include_str!`) rather than a fetched manifest: there is no distro
//! store to query, and a baked-in list works offline and can't be
//! poisoned by a runtime fetch. Entries are restricted to Docker
//! Official Images (`library/*`) and Verified Publisher namespaces on
//! Docker Hub, plus a distro's own canonical registry when it
//! publishes elsewhere (quay.io for Rocky, registry.access.redhat.com
//! for UBI).
//!
//! `cm --catalog FILE` swaps in a user catalog — same schema — but it
//! is an explicit per-invocation flag, never auto-loaded from a search
//! path: anything under `$HOME` is guest-writable (security.md T6), so
//! a distro could rewrite a default catalog file to point familiar
//! names at hostile images.
//!
//! Entry `name`s deliberately contain no `/`, `:`, or `@`, so any
//! argument shaped like a real image reference can never collide —
//! passing a fully-qualified ref (or `--from-image`) to `--install`
//! bypasses the catalog.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::table;

/// The compiled-in catalog — the same schema `--catalog` accepts.
const BUILTIN: &str = include_str!("catalog.json");

/// One installable distribution.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    /// Install key, matching WSL's NAME column (`cm --install <name>`).
    /// Lookup is case-insensitive; no `/`, `:`, or `@`.
    pub name: String,
    /// Display name for the FRIENDLY NAME column.
    pub friendly_name: String,
    /// Fully-qualified OCI image reference to install.
    pub image: String,
}

#[derive(Deserialize)]
struct Catalog {
    distributions: Vec<Entry>,
}

fn parse(text: &str, source: &str) -> Result<Vec<Entry>> {
    let catalog: Catalog =
        serde_json::from_str(text).with_context(|| format!("invalid catalog {source}"))?;
    for e in &catalog.distributions {
        if e.name.is_empty() || e.name.contains(['/', ':', '@']) {
            bail!("catalog {source}: invalid NAME {:?}", e.name);
        }
        if e.image.is_empty() {
            bail!("catalog {source}: {:?} has no image", e.name);
        }
    }
    Ok(catalog.distributions)
}

/// The catalog to use: `path`'s JSON, or the built-in list. The first
/// entry is the default for a bare `cm --install`, matching WSL (which
/// installs Ubuntu).
pub fn load(path: Option<&Path>) -> Result<Vec<Entry>> {
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
/// scrape.
#[must_use]
pub fn render(entries: &[Entry]) -> String {
    let mut out = String::from(
        "The following is a list of valid distributions that can be installed.\n\
         Install using 'cm --install <Distro>'.\n\n",
    );
    let mut rows = vec![vec!["NAME".to_string(), "FRIENDLY NAME".to_string()]];
    for e in entries {
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
        let entries = load(None).unwrap();
        assert_eq!(entries.first().unwrap().name, "Ubuntu");
        assert!(entries.len() > 10);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let entries = load(None).unwrap();
        assert_eq!(
            lookup(&entries, "Ubuntu-24.04").unwrap().image,
            "docker.io/library/ubuntu:24.04"
        );
        assert_eq!(lookup(&entries, "ubuntu").unwrap().name, "Ubuntu");
        assert_eq!(lookup(&entries, "KALI-LINUX").unwrap().name, "kali-linux");
        assert_eq!(lookup(&entries, "alpine").unwrap().name, "Alpine");
        assert!(lookup(&entries, "nixos").is_none());
    }

    #[test]
    fn builtin_names_never_look_like_image_refs() {
        // load() enforces this, but the property is important enough to
        // check on the built-in data too.
        for e in load(None).unwrap() {
            assert!(
                !e.name.contains(['/', ':', '@']),
                "{} is indistinguishable from an image ref",
                e.name
            );
        }
    }

    #[test]
    fn custom_catalog_rejects_ref_shaped_name() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            f.path(),
            r#"{"distributions":[{"name":"evil/latest","friendly_name":"x","image":"docker.io/library/alpine:latest"}]}"#,
        )
        .unwrap();
        assert!(load(Some(f.path())).is_err());
    }

    #[test]
    fn render_shaped_like_wsl() {
        let out = render(&load(None).unwrap());
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
    }
}
