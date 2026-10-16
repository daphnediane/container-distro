/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Image refs we tag for snapshots and imports, and their cleanup.

use super::run_container_quiet;
use crate::container::{self, container_cmd};
use crate::distro::spec::{Arch, LABEL_DISTRO};
use crate::naming::label_lookup;
use crate::util::oci;
use anyhow::Result;
use std::path::Path;
use std::process::Stdio;

pub(super) fn snapshot_repo(name: &str) -> String {
    format!("local/distro-{name}")
}

/// Whether the image belongs to distro `name`: the `distro` label
/// pre-#32 `import`s set on their per-name snapshot images (C11).
/// Content-keyed shared images carry no name label — they're not any
/// one distro's to delete — so unlabeled images are left for
/// `container image prune`, as before.
fn is_our_image(image: &container::ImageListEntry, name: &str) -> bool {
    label_lookup(&image.labels(), LABEL_DISTRO).is_some_and(|v| v == name)
}

/// Best-effort removal of `name`'s imported-image tags, except `keep`.
/// Two claim shapes: the `local/distro-<name>:` tag prefix (current)
/// and the `distro` label pre-#32 imports set. `image delete` on a tag
/// untags only that reference — the shared `local/rootfs:*` tag and
/// other importers' tags survive; the unused-tag prune frees the
/// content once nothing references it.
pub(super) fn remove_snapshot_images(name: &str, keep: Option<&str>) {
    let Some(images) = container::list_images() else {
        return;
    };
    let prefix = format!("{}:", snapshot_repo(name));
    for image in &images {
        let reference = &image.configuration.name;
        let short = reference.strip_prefix("docker.io/").unwrap_or(reference);
        let claimed = is_our_image(image, name) || short.starts_with(&prefix);
        if claimed && Some(short) != keep {
            release_imported_image(reference);
        }
    }
}

/// A SHA-256 in cache-filename form: lowercase hex, `0x` stripped —
/// the form the shared image's reference and `rootfs-sha256` label
/// use. (Mirrors `catalog::normalize_sha`.)
pub(super) fn normalized_sha(s: &str) -> String {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
        .to_lowercase()
}

// Imported rootfs images carry two tags: the shared
// `local/rootfs:<sha256>` dedup tag and a per-importer
// `local/{distro,machine}-<name>:imported-<sha256>` tag the
// journal/machine records and `rm`/`--unregister` untags. 64 hex
// chars fit well under the 128-char tag limit.

/// The shared content-keyed tag: any later import of the same archive
/// — another distro, a machine, a retry after a failed create — reuses
/// it instead of re-running the `.wsl`→OCI wrap and `image load`,
/// which often outlasts the download itself.
fn imported_image_ref(sha256: &str) -> String {
    format!("local/rootfs:{sha256}")
}

/// The per-distro tag on an imported image (`snapshot_repo` keeps the
/// `local/distro-<name>` prefix `is_generated_image` and the rename
/// warning recognize).
pub(super) fn distro_import_ref(name: &str, sha256: &str) -> String {
    format!("{}:imported-{sha256}", snapshot_repo(name))
}

/// The per-machine tag on an imported image.
pub(super) fn machine_import_ref(name: &str, sha256: &str) -> String {
    format!("local/machine-{name}:imported-{sha256}")
}

/// The `<sha>` of an `*:imported-<sha>` tag when the suffix parses —
/// `imported-<ts>` refs from pre-#32 imports don't qualify (their
/// content tag didn't exist).
pub(super) fn imported_ref_sha(reference: &str) -> Option<&str> {
    let sha = reference.rsplit_once(":imported-")?.1;
    (sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha)
}

/// `container image tag` — a new reference on an existing image.
pub(super) fn tag_image(source: &str, target: &str) -> Result<()> {
    run_container_quiet(&["image", "tag", source, target])
}

/// Untag `reference`: `image delete` removes just that state entry —
/// other tags of the same image, and its blobs while any tag remains,
/// are untouched. Best-effort.
pub(super) fn untag_image(reference: &str) {
    let _ = container_cmd()
        .args(["image", "delete", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Whether `reference` already names a local image. A listing failure
/// reads as a miss — the cost is a redundant `image load` of identical
/// content, nothing worse.
fn image_cached(reference: &str) -> bool {
    container::list_images().is_some_and(|imgs| {
        imgs.iter()
            .any(|i| container::same_image(&i.configuration.name, reference))
    })
}

/// Cap on shared imported-rootfs images no importer references, the
/// image-side analogue of the `.wsl` download cache's budget — oldest
/// unused tags are evicted once the set is over it.
const MAX_IMPORTED_IMAGE_BYTES: u64 = 8 << 30;

/// Load `rootfs` as an image under the shared content tag unless that
/// tag already exists; returns the shared reference. Shared images
/// carry `imported-from`/`rootfs-sha256` provenance but deliberately
/// no `distro`/`machine` name label: the label is per-image, not
/// per-tag, so a name on it would let one importer's cleanup claim
/// tags another importer still uses.
pub(super) fn ensure_rootfs_image(
    rootfs: &Path,
    sha256: &str,
    arch: Arch,
    labels: &[(String, String)],
) -> Result<String> {
    let reference = imported_image_ref(sha256);
    if image_cached(&reference) {
        eprintln!("  reusing imported image {reference}");
    } else {
        oci::load_rootfs(rootfs, &reference, arch.as_str(), labels)?;
        // A fresh load may itself push the unused set over budget.
        prune_imported_images();
    }
    Ok(reference)
}

/// Tag the shared `shared` image as `per_ref` (a
/// `distro_import_ref`/`machine_import_ref`) — the recorded, prunable
/// ref. Falls back to the shared ref when `image tag` isn't available.
pub(super) fn per_importer_ref(shared: &str, per_ref: &str) -> String {
    match tag_image(shared, per_ref) {
        Ok(()) => per_ref.to_string(),
        Err(e) => {
            eprintln!(
                "container-distro: warning: `container image tag` failed ({e:#}); \
                 recording the shared reference {shared}"
            );
            shared.to_string()
        }
    }
}

/// Drop `per_ref` (an `*:imported-<sha>` tag), then run the unused-tag
/// prune — its shared tag may have just become evictable.
fn release_imported_image(per_ref: &str) {
    untag_image(per_ref);
    prune_imported_images();
}

/// Untag `local/machine-<name>:*` image tags — the import bookkeeping
/// `machine rm` doesn't know about — after the machine is gone.
/// Best-effort; a stale tag just keeps its shared image listed as
/// in-use until pruned.
pub fn release_machine_images(name: &str) {
    let Some(images) = container::list_images() else {
        return;
    };
    let prefix = format!("local/machine-{name}:");
    for i in &images {
        if i.configuration.name.starts_with(&prefix) {
            release_imported_image(&i.configuration.name);
        }
    }
}

/// Evict `local/rootfs:*` tags no `*:imported-<sha>` ref still claims,
/// oldest first, until the unused set is under
/// `MAX_IMPORTED_IMAGE_BYTES` — the image-side analogue of the `.wsl`
/// cache's `prune_cache`. Best-effort.
fn prune_imported_images() {
    let Some(images) = container::list_images() else {
        return;
    };
    let mut unused: Vec<(&container::ImageListEntry, u64)> = images
        .iter()
        .filter_map(|i| {
            let sha = i.configuration.name.strip_prefix("local/rootfs:")?;
            let suffix = format!(":imported-{sha}");
            if images
                .iter()
                .any(|o| o.configuration.name.ends_with(&suffix))
            {
                return None;
            }
            let size: u64 = i.variants.iter().map(|v| v.size.max(0) as u64).sum();
            Some((i, size))
        })
        .collect();
    // Oldest tag first; a missing creationDate sorts as epoch.
    unused.sort_by(|a, b| {
        a.0.configuration
            .creation_date
            .cmp(&b.0.configuration.creation_date)
    });
    let mut total: u64 = unused.iter().map(|(_, s)| s).sum();
    for (entry, size) in unused {
        if total <= MAX_IMPORTED_IMAGE_BYTES {
            break;
        }
        untag_image(&entry.configuration.name);
        total = total.saturating_sub(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::container::{self};
    use crate::naming::label_key;

    use crate::distro::spec::LABEL_DISTRO;

    #[test]
    fn imported_ref_sha_parses_only_sha_tags() {
        let sha = "a".repeat(64);
        assert_eq!(
            imported_ref_sha(&distro_import_ref("d1", &sha)),
            Some(sha.as_str())
        );
        assert_eq!(
            imported_ref_sha(&machine_import_ref("m1", &sha)),
            Some(sha.as_str())
        );
        // Pre-#32 timestamp refs and other tags don't qualify.
        assert_eq!(
            imported_ref_sha("local/distro-d1:imported-1728000000"),
            None
        );
        assert_eq!(imported_ref_sha("local/rootfs:abc"), None);
        assert_eq!(imported_ref_sha("alpine:latest"), None);
    }

    #[test]
    fn imported_refs_are_tag_shaped() {
        let sha = "b".repeat(64);
        assert_eq!(imported_image_ref(&sha), format!("local/rootfs:{sha}"));
        assert_eq!(
            distro_import_ref("d1", &sha),
            format!("local/distro-d1:imported-{sha}")
        );
        assert_eq!(
            machine_import_ref("m1", &sha),
            format!("local/machine-m1:imported-{sha}")
        );
        // Every per-importer ref still reads as a generated
        // `local/*-*:imported-*` tag, and fits the 128-char tag limit.
        assert!(distro_import_ref("d1", &sha).len() < 128);
        assert!(machine_import_ref("m1", &sha).len() < 128);
    }

    #[test]
    fn normalized_sha_strips_and_lowercases() {
        let hex = "AbC".repeat(21) + "d";
        assert_eq!(normalized_sha(&format!("0x{hex}")), hex.to_lowercase());
        assert_eq!(normalized_sha(&format!("0X{hex}")), hex.to_lowercase());
        assert_eq!(normalized_sha(&hex), hex.to_lowercase());
    }

    #[test]
    fn our_image_by_label() {
        let img = |name: &str, labels: &[(&str, &str)]| container::ImageListEntry {
            configuration: container::ImageListConfig {
                name: name.into(),
                creation_date: None,
            },
            variants: vec![container::ImageVariant {
                config: Some(container::ImageVariantConfig {
                    config: Some(container::ImageUserConfig {
                        labels: labels
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    }),
                }),
                size: 0,
            }],
        };
        let distro_key = label_key(LABEL_DISTRO);

        // The label decides regardless of the image's name.
        let labeled = img("whatever/tag:x", &[(distro_key.as_str(), "d1")]);
        assert!(is_our_image(&labeled, "d1"));
        let other = img("local/distro-d2:imported-1", &[(distro_key.as_str(), "d2")]);
        assert!(!is_our_image(&other, "d1"));

        // Unlabeled images are never touched — even under our repo.
        let unlabeled = img("local/distro-d1:imported-1", &[]);
        assert!(!is_our_image(&unlabeled, "d1"));
    }
}
