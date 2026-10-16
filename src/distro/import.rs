/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `import`: a distro from a rootfs tar.

use super::create::{CreateOptions, build_spec, create_from_spec, stamped_origin};
use super::images::{
    distro_import_ref, ensure_rootfs_image, normalized_sha, per_importer_ref, untag_image,
};
use super::journal::{distro_exists, lower_sharing, write_conf_snapshots, wsl_automount_disabled};
use super::names::bail_if_staged;
use crate::container::{ensure_started, validate_name};
use crate::distro::spec::{
    Arch, Intent, LABEL_IMPORTED_FROM, LABEL_ROOTFS_SHA256, OriginSource, WslConf, WslConfMode,
};
use crate::distro::terminal;
use crate::naming::label_key;
use crate::{oci, wsl};
use anyhow::{Result, bail};
use std::path::Path;

/// Where an imported rootfs came from, recorded in the loaded image's
/// `imported-from` label and the journal's `origin` — a local path for
/// `Local`, the download itself for `Download`. `Download` also
/// supplies the verified rootfs SHA-256 the shared image is keyed and
/// labeled by; `Local` hashes the file.
#[derive(Debug, Clone, Copy)]
pub enum ImportSource<'a> {
    /// A local file path (or `-` for stdin).
    Local,
    /// A catalog `.wsl` download: the URL, the SHA-256 it was verified
    /// against, and the catalog entry's name.
    Download {
        url: &'a str,
        sha256: &'a str,
        name: &'a str,
    },
}

/// Create a distro from a rootfs tar; returns its name. `requested`
/// is the source side of the spec's intent, as in [`create`]. `pkg`
/// carries the archive's `wsl.conf`/`wsl-distribution.conf` when the
/// caller already scanned it (catalog imports name the distro from
/// it); otherwise the archive is scanned here — one pass either way.
pub fn import(
    name: &str,
    file: &Path,
    opts: &CreateOptions,
    source: ImportSource<'_>,
    requested: &Intent,
    pkg: Option<&wsl::PackageConf>,
) -> Result<String> {
    validate_name(name)?;
    ensure_started()?;
    if distro_exists(name)? {
        bail!("distro `{name}` already exists");
    }
    // Checked here too so the conversion can't start only to have the
    // create refused on leftover state; `create_from_spec` re-checks
    // under the distro lock.
    bail_if_staged(name)?;
    let stdin_copy;
    let rootfs = if file == Path::new("-") {
        stdin_copy = oci::stdin_to_tempfile()?;
        stdin_copy.path()
    } else {
        file
    };
    // Content key for the shared import image: a catalog download's
    // verified hash is already known; a local file's takes one extra
    // read pass — cheap next to the OCI wrap a hit can skip.
    let sha256 = match &source {
        ImportSource::Download { sha256, .. } => normalized_sha(sha256),
        ImportSource::Local => oci::hash_file(rootfs)?,
    };
    // Provenance labels, no per-distro name label — see
    // `ensure_rootfs_image`. `imported-from` records the first
    // importer's source; the journal's `origin` tracks each distro's.
    let imported_from = match source {
        ImportSource::Local if file == Path::new("-") => "-".to_string(),
        ImportSource::Local => file.to_string_lossy().into_owned(),
        ImportSource::Download { url, .. } => url.to_string(),
    };
    let labels = [
        (label_key(LABEL_ROOTFS_SHA256), sha256.clone()),
        (label_key(LABEL_IMPORTED_FROM), imported_from),
    ];
    // The OCI image carries the requested arch so `create` picks the
    // amd64 platform — and Rosetta — without a separate flag.
    let shared = ensure_rootfs_image(
        rootfs,
        &sha256,
        opts.arch.unwrap_or_else(Arch::host),
        &labels,
    )?;
    // The journal records the per-distro tag — what `rm` untags — on
    // top of the shared content tag the dedup keys off.
    let reference = per_importer_ref(&shared, &distro_import_ref(name, &sha256));
    // The archive's config files are guest input: extract them while
    // the tar is already open, for the automount default below and —
    // when the distro honors guest config — the journal snapshots. An
    // unreadable tar is `load_rootfs`'s error; a scan failure just
    // means no config.
    let scanned;
    let pkg = match pkg {
        Some(p) => Some(p.clone()),
        None => {
            scanned = wsl::package_conf(rootfs).ok();
            scanned
        }
    };
    // A catalog download is a vetted package — its config is trusted
    // like `--trusted`'s, expressed as source intent so an explicit
    // `--use-wsl-conf none` still wins.
    let mut requested = requested.clone();
    if matches!(source, ImportSource::Download { .. }) {
        requested.wsl_conf = Some(WslConfMode::all());
    }
    // `[windowsterminal] enabled` is the package's host-profile ask —
    // source intent, so an explicit --terminal-profile still wins.
    if let Some(v) = pkg
        .as_ref()
        .and_then(|p| p.distribution_conf.as_deref())
        .and_then(terminal::conf_terminal_profile)
    {
        requested.terminal_profile = Some(v);
    }
    let mut spec = build_spec(name.to_string(), reference, opts, &requested)?;
    // `[automount] enabled=false` lowers the sharing defaults — only
    // where the distro honors its packaged config at all, and only
    // down (source intent loses to every explicit flag). Image-based
    // `create` can't see the file before first boot; an archive can.
    if spec.resolve().0.wsl_conf.contains(WslConf::AUTOMOUNT)
        && wsl_automount_disabled(pkg.as_ref().and_then(|p| p.wsl_conf.as_deref()))
    {
        lower_sharing(&mut spec);
    }
    spec.origin = Some(stamped_origin(match source {
        ImportSource::Local => OriginSource::File {
            path: file.to_string_lossy().into_owned(),
        },
        ImportSource::Download { url, sha256, name } => OriginSource::Download {
            url: url.to_string(),
            sha256: sha256.to_string(),
            name: name.to_string(),
        },
    }));
    match create_from_spec(&spec, opts) {
        Ok(name) => {
            // Snapshot the archive's config into the journal — the
            // `--no-boot` import lands with them in place; a booted
            // distro refreshes them from the live guest file anyway.
            if !spec.resolve().0.wsl_conf.is_empty()
                && let Some(p) = &pkg
                && (p.wsl_conf.is_some() || p.distribution_conf.is_some())
                && let Err(e) = write_conf_snapshots(
                    &name,
                    p.wsl_conf.as_deref(),
                    p.distribution_conf.as_deref(),
                )
            {
                eprintln!(
                    "container-distro: warning: can't snapshot `{name}`'s packaged config: {e:#}"
                );
            }
            Ok(name)
        }
        Err(e) => {
            // The shared tag stays — a cache entry a retry reuses —
            // but the per-distro tag belongs to a distro that never
            // landed.
            if spec.image != shared {
                untag_image(&spec.image);
            }
            Err(e)
        }
    }
}
