/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `--install`, `--import`, and `--export`.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};

use super::args::{Backend, InstallOpts, InstallSource};
use super::backend::{self, Target};
use super::named_target;
use super::run::launch;
use crate::catalog;
use crate::container::{self, ArgvMode};
use crate::distro::spec::{Arch, Intent};
use crate::distro::{self, CreateOptions};

/// The positional `--install` argument is a catalog name when it
/// matches one (case-insensitive), otherwise an image reference —
/// catalog names contain no `/`/`:`/`@`, so a qualified ref always
/// bypasses the catalog, as does `--from-image` explicitly. A catalog
/// entry resolves to its `Image` or to the `.wsl` download for the
/// host arch; an entry with neither is un-installable here (like WSL
/// on ARM rejecting amd64-only distributions). An image matched
/// through the catalog keeps the entry's NAME as the default distro
/// name — a bare image reference derives one from the ref instead.
fn resolve<'a>(
    cat: &'a catalog::Catalog,
    source: &'a InstallSource,
    arch: Arch,
) -> Result<catalog::Resolved<'a>> {
    match source {
        InstallSource::Default => catalog::resolve(cat, None, arch, false),
        InstallSource::CatalogOrImage(a) => catalog::resolve(cat, Some(a), arch, true),
        InstallSource::Image(image) => Ok(catalog::Resolved::Image {
            image,
            name: None,
            requested: Intent::default(),
        }),
        InstallSource::File(_) => bail!("internal error: file source resolved as image"),
    }
}

/// WSL `--install`: create a distro (or, with `--machine`, a machine)
/// from an image, then open a shell in it.
pub(super) fn install(opts: &InstallOpts) -> Result<ExitCode> {
    container::ensure_started()?;
    let create = CreateOptions {
        volumes: opts.shares.clone(),
        publish: opts.publish.clone(),
        cpus: opts.cpus.map(u64::from),
        memory: opts.memory.clone(),
        arch: opts.arch,
        home_mount: opts.home_mount,
        preset: opts.preset,
        ssh: opts.ssh,
        no_ssh: opts.no_ssh,
        restricted: opts.restricted,
        trusted: opts.trusted,
        use_wsl_conf: opts.use_wsl_conf.clone(),
        no_wsl_conf: opts.no_wsl_conf,
        ignore_wsl_conf: opts.ignore_wsl_conf.clone(),
        force: opts.force,
        // `--terminal` opens the distro's profile — the same explicit
        // opt-in `--terminal-profile` pins. Its open is the import, so
        // create skips the silent no-op import (`will_open_terminal`)
        // and the window launches the distro itself.
        terminal_profile: opts.terminal_profile || opts.terminal,
        no_terminal_profile: opts.no_terminal_profile,
        will_open_terminal: opts.terminal,
        ..CreateOptions::default()
    };
    if let InstallSource::File(file) = &opts.source {
        return install_from_file(opts, &create, file, None, distro::ImportSource::Local);
    }
    let cat = catalog::load(opts.catalog.as_deref())?;
    let resolved = resolve(&cat, &opts.source, opts.arch.unwrap_or_else(Arch::host))?;
    let (image, hint, requested) = match &resolved {
        catalog::Resolved::Image {
            image,
            name,
            requested,
        } => (*image, *name, requested.clone()),
        catalog::Resolved::Wsl { name, .. } => {
            // Machines boot from images — a catalog entry that only
            // ships a rootfs download can't satisfy `--machine`.
            if opts.machine {
                bail!(
                    "`{name}` ships as a rootfs archive, which only distros boot (drop --machine)"
                );
            }
            if !backend::distros_enabled() {
                bail!("--install needs distro support (unset CM_BACKEND=machine)");
            }
            let name = catalog::install_resolved(resolved, opts.name.clone(), &create)?;
            if opts.no_launch {
                return Ok(ExitCode::SUCCESS);
            }
            return launch(&name, opts.terminal);
        }
    };
    if !opts.wants_machine() {
        if !backend::distros_enabled() {
            bail!(
                "--install defaults to a distro; that needs distro support (unset CM_BACKEND=machine)"
            );
        }
        let name = catalog::install_resolved(
            catalog::Resolved::Image {
                image,
                name: hint,
                requested: requested.clone(),
            },
            opts.name.clone(),
            &create,
        )?;
        if opts.no_launch {
            return Ok(ExitCode::SUCCESS);
        }
        return launch(&name, opts.terminal);
    }

    // --name is used verbatim; otherwise a catalog install takes the
    // entry's NAME (sanitized: `Ubuntu` → `ubuntu`, like `wsl --install`
    // registering `Ubuntu`) and a bare image install derives one from
    // the image ref (`alpine:latest` → `alpine-latest`).
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| container::default_machine_name(hint.unwrap_or(image)));
    container::validate_name(&name)?;
    // A same-named distro hides the machine from every `cm` name
    // lookup — refused unless --force opts in.
    distro::bail_distro_shadow(&name, opts.force)?;
    // `machine create` is shared with `migrate --out`: the options
    // resolve through the same intent chain, and the
    // always-forwarded-agent check warns or refuses a `--no-ssh` pin
    // (clap already conflicts `--machine --no-ssh`, so only a derived
    // ssh-off reaches the warning).
    let spec = create.spec(&requested);
    distro::create_machine(&name, image, &spec, &create)?;
    if opts.no_launch {
        return Ok(ExitCode::SUCCESS);
    }
    let mut cmd = container::run_command(
        Some(&name),
        None,
        None,
        &container::GuestEnv::default(),
        None,
        &[],
        ArgvMode::Shell,
    );
    Err(anyhow::Error::from(cmd.exec()).context("failed to exec `container machine run`"))
}

/// `--install --from-file`, or a catalog `.wsl` entry: import a rootfs
/// tar or `.wsl` package as a distro — machines can't boot a bare
/// rootfs, so this is always the distro path regardless of `--distro`.
/// `name_hint` is a catalog entry's NAME when the source was a catalog
/// download; the naming order is `--name`, `name_hint` (`wsl --install`
/// registers the manifest name), the `.wsl` manifest's
/// `oobe.defaultName` (what `--from-file` uses), then the file stem.
fn install_from_file(
    opts: &InstallOpts,
    create: &CreateOptions,
    file: &std::path::Path,
    name_hint: Option<&str>,
    source: distro::ImportSource<'_>,
) -> Result<ExitCode> {
    if !backend::distros_enabled() {
        bail!("--from-file needs distro support (unset CM_BACKEND=machine)");
    }
    // --name is used verbatim; a derived name (a catalog NAME, the .wsl
    // manifest's oobe.defaultName, else the file stem) is sanitized to a
    // valid one.
    // `create` already pins terminal_profile when `--terminal` asked
    // to open the profile — the explicit opt-in `--terminal-profile`
    // would write.
    let name = catalog::import_file(
        file,
        opts.name.clone(),
        name_hint,
        create,
        source,
        &Intent::default(),
    )?;
    if opts.no_launch {
        return Ok(ExitCode::SUCCESS);
    }
    launch(&name, opts.terminal)
}

/// WSL `--export`: distros go through `container export` directly;
/// machines through the scratch-container workaround (`container
/// export` can't snapshot a machine's plugin-state rootfs — see
/// docs/design/upstream-limitations.md). FILE `-` writes the tar to stdout.
pub(super) fn export(name: &str, file: &str, sel: Backend) -> Result<ExitCode> {
    let output = (file != "-").then_some(Path::new(file));
    match named_target(name, sel)? {
        Target::Distro(d) => distro::export(&d, output)?,
        Target::Machine(_) => distro::export_machine(name, output)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// WSL `--import`: import a rootfs tar as a distro — or, with
/// `--machine`, wrap it as an OCI image and create a machine.
pub(super) fn import(
    name: &str,
    install_location: Option<&str>,
    file: &str,
    sel: Backend,
    arch: Option<Arch>,
    force: bool,
    terminal: bool,
) -> Result<ExitCode> {
    container::validate_name(name)?;
    if let Some(loc) = install_location.filter(|l| !l.is_empty() && *l != "-") {
        eprintln!("cm: ignoring install location `{loc}` — storage is managed by `container`");
    }
    let file = Path::new(file);
    if sel == Backend::Machine {
        distro::import_machine(name, file, arch, force)?;
    } else {
        if !backend::distros_enabled() {
            bail!(
                "--import defaults to a distro; that needs distro support (unset CM_BACKEND=machine)"
            );
        }
        // A name shared with a machine shadows it for `cm` — refused
        // inside `ops::import` unless --force opted in.
        distro::import(
            name,
            file,
            &CreateOptions {
                arch,
                force,
                // `--terminal` opens the profile — the same explicit
                // opt-in `--terminal-profile` pins; its open is the
                // import, so skip the silent no-op import.
                terminal_profile: terminal,
                will_open_terminal: terminal,
                ..CreateOptions::default()
            },
            distro::ImportSource::Local,
            &Intent::default(),
            None,
        )?;
        if terminal {
            distro::open_terminal(name)?;
        }
    }
    Ok(ExitCode::SUCCESS)
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
        // `wsl --install` registers `Ubuntu` — a catalog install takes
        // the entry's NAME, not a file- or image-derived name. The
        // default is a `.wsl` install on this host.
        let catalog::Resolved::Wsl { name, .. } =
            resolve(&cat, &InstallSource::Default, Arch::host()).unwrap()
        else {
            panic!("the catalog default should resolve to a .wsl download");
        };
        assert_eq!(name, "Ubuntu");
        assert_eq!(container::default_machine_name(name), "ubuntu");
        for arg in ["ubuntu", "UBUNTU-24.04"] {
            let source = InstallSource::CatalogOrImage(arg.to_string());
            let catalog::Resolved::Wsl { name, .. } = resolve(&cat, &source, Arch::host()).unwrap()
            else {
                panic!("{arg} should resolve to a .wsl download");
            };
            assert!(!name.is_empty(), "{arg}");
        }
        // Overlay entries without a `.wsl` download resolve to images.
        let source = InstallSource::CatalogOrImage("alpine".to_string());
        let catalog::Resolved::Image { name, .. } = resolve(&cat, &source, Arch::host()).unwrap()
        else {
            panic!("alpine should resolve to an image");
        };
        assert_eq!(name, Some("Alpine"));
    }

    /// `--arch amd64` switches the catalog lookup to the entry's x86-64
    /// download — installed tagged `amd64` so `container` runs it under
    /// Rosetta.
    #[test]
    fn amd64_install_picks_the_amd64_download() {
        let cat = builtin();
        let source = InstallSource::CatalogOrImage("ubuntu".to_string());
        let catalog::Resolved::Wsl { download, .. } = resolve(&cat, &source, Arch::Amd64).unwrap()
        else {
            panic!("Ubuntu should resolve to a .wsl download on amd64");
        };
        let entry = catalog::lookup(&cat.entries, "Ubuntu").unwrap();
        assert_eq!(download, entry.amd64_url.as_ref().unwrap());
    }

    #[test]
    fn image_installs_have_no_catalog_name() {
        let cat = builtin();
        // A qualified ref bypasses the catalog; --from-image skips it.
        // Both derive the name from the image instead.
        for source in [
            InstallSource::CatalogOrImage("quay.io/x/y:1".to_string()),
            InstallSource::Image("alpine:3".to_string()),
        ] {
            let catalog::Resolved::Image { name, .. } =
                resolve(&cat, &source, Arch::host()).unwrap()
            else {
                panic!("{source:?} should resolve to an image");
            };
            assert_eq!(name, None, "{source:?}");
        }
    }

    #[test]
    fn wsl_catalog_entries_keep_their_name() {
        let cat = builtin();
        let source = InstallSource::CatalogOrImage("rlcplus-9".to_string());
        let catalog::Resolved::Wsl { name, .. } = resolve(&cat, &source, Arch::host()).unwrap()
        else {
            panic!("RLCPlus-9 should resolve to a .wsl download");
        };
        assert_eq!(name, "RLCPlus-9");
    }
}
