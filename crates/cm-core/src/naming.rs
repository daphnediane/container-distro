/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The project's externally visible names, in one place.
//!
//! The project is `container-distro`. Everything that persists
//! outside this repository — container labels, the per-user state
//! directory — is derived from the constants here, so a rename is a
//! one-line change plus an entry in the `LEGACY_*` lists. See
//! `doc/naming.md` for every place the name appears and the migration
//! strategy.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Short project name: the state directory and plugin metadata.
pub const APP_NAME: &str = "container-distro";

/// Reverse-DNS prefix for container labels (a namespace we control via
/// GitHub, per the OCI/Docker label-key convention).
pub const LABEL_PREFIX: &str = "io.github.daphnediane.container-distro";

/// Label prefixes written by earlier releases, newest first. Labels are
/// read through [`label_lookup`], which falls back to these, so distros
/// created before a rename keep working until they are recreated.
pub const LEGACY_LABEL_PREFIXES: &[&str] = &[];

/// App names used by earlier releases, newest first. [`state_dir`] moves
/// the first legacy state directory it finds into place.
pub const LEGACY_APP_NAMES: &[&str] = &[];

/// The full label key for `suffix` (`distro` → `io.github…container-distro.distro`).
#[must_use]
pub fn label_key(suffix: &str) -> String {
    format!("{LABEL_PREFIX}.{suffix}")
}

/// Look up label `suffix` under the current prefix, then legacy prefixes.
pub fn label_lookup<'a>(labels: &'a BTreeMap<String, String>, suffix: &str) -> Option<&'a String> {
    std::iter::once(LABEL_PREFIX)
        .chain(LEGACY_LABEL_PREFIXES.iter().copied())
        .find_map(|p| labels.get(&format!("{p}.{suffix}")))
}

fn app_support() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support"))
}

/// Per-user state directory (`~/Library/Application Support/<APP_NAME>`).
///
/// If it doesn't exist but a legacy one does, the legacy directory is
/// moved into place and a symlink is left at the old path (best effort):
/// existing distros mount their init assets from the old path until
/// they are recreated.
pub fn state_dir() -> Result<PathBuf> {
    let base = app_support()?;
    let dir = base.join(APP_NAME);
    if !dir.exists()
        && let Some(old) = LEGACY_APP_NAMES
            .iter()
            .map(|n| base.join(n))
            .find(|p| p.is_dir())
        && fs::rename(&old, &dir).is_ok()
    {
        let _ = std::os::unix::fs::symlink(&dir, &old);
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_key_uses_prefix() {
        assert_eq!(
            label_key("distro"),
            "io.github.daphnediane.container-distro.distro"
        );
    }

    #[test]
    fn label_lookup_current_prefix() {
        let mut labels = BTreeMap::new();
        labels.insert(label_key("user"), "dp:501:20".to_string());
        assert_eq!(
            label_lookup(&labels, "user").map(String::as_str),
            Some("dp:501:20")
        );
        assert!(label_lookup(&labels, "distro").is_none());
    }
}
