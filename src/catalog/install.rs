/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Resolving catalog names and installing distros from them.

use crate::catalog::{self, wsl};
use crate::container::{self};
use crate::distro::spec::{Arch, HomeMount, Intent, OriginSource, WslConfMode};
use crate::distro::{self, CreateOptions, ImportSource};
use anyhow::{Context, Result, bail};
use std::path::Path;

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
            distro::create(Some(distro_name), opts, image, origin, &requested)
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

/// The name an import derives when `requested_name` is absent — the
/// WSL order: `name_hint` first, because it is the catalog entry's
/// NAME and `wsl --install` registers the manifest name (`Ubuntu`, not
/// the package's `oobe.defaultName` like `Ubuntu-26.04`); then the
/// package manifest's `oobe.defaultName`, which is what `wsl --install
/// --from-file` registers; then the file stem (our convenience — WSL
/// refuses a package that names nothing).
fn import_default_name(hint: Option<&str>, pkg: &wsl::PackageConf, file: &Path) -> String {
    hint.map(str::to_string)
        .or_else(|| pkg.default_name.clone())
        .unwrap_or_else(|| file_default_name(file))
}

/// Import a rootfs tar/`.wsl` as a distro, deriving its name in WSL
/// order when `requested_name` is absent: `name_hint` (the catalog
/// entry's NAME — `wsl --install` registers the manifest name), then
/// the package manifest's `oobe.defaultName` (what `wsl --install
/// --from-file` uses), then the file stem.
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
        container::default_machine_name(&import_default_name(name_hint, &pkg, file))
    });
    container::validate_name(&name)?;
    // Fail a refused name before `import` stages an image (the `.wsl`
    // download is already cached, so retrying with another name skips
    // the fetch). `import` re-checks — and issues the warning — for
    // --force.
    if !opts.force {
        distro::bail_machine_shadow(&name, false)?;
    }
    distro::import(&name, file, opts, source, requested, Some(&pkg))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::container::{self};
    use crate::distro::spec::{Arch, HomeMount};

    use super::super::test_support::builtin;

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
    fn catalog_name_beats_package_default_name() {
        let pkg = wsl::PackageConf {
            default_name: Some("Ubuntu-26.04".into()),
            wsl_conf: None,
            distribution_conf: None,
        };
        let file = std::path::Path::new("ubuntu-26.04.1-wsl-amd64.wsl");
        // `wsl --install` registers the manifest name, not oobe's.
        assert_eq!(import_default_name(Some("Ubuntu"), &pkg, file), "Ubuntu");
        // `--from-file` has no catalog name — oobe.defaultName wins.
        let pkg_none = wsl::PackageConf::default();
        assert_eq!(import_default_name(None, &pkg, file), "Ubuntu-26.04");
        assert_eq!(
            import_default_name(None, &pkg_none, file),
            "ubuntu-26.04.1-wsl-amd64"
        );
    }
}
