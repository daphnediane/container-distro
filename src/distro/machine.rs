/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container machine` interop: export/import/create machines and read their rootfs.

use super::create::{CreateOptions, create_intent, warn_public_publish};
use super::export::check_export_output;
use super::host::host_user;
use super::images::{ensure_rootfs_image, machine_import_ref, per_importer_ref, untag_image};
use super::journal::container_dir;
use super::names::bail_distro_shadow;
use super::{run_container, run_container_quiet, timestamp};
use crate::container::{
    self, ArgvMode, MachineDetail, container_cmd, ensure_started, validate_name,
};
use crate::distro::spec::{
    Arch, DistroSpec, HostUser, Intent, LABEL_EXPORT_SCRATCH, LABEL_IMPORTED_FROM,
    LABEL_ROOTFS_SHA256,
};
use crate::naming::label_key;
use crate::oci;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// A scratch container created for a machine export — deleted on drop
/// so a failed export can't leak it. (A SIGKILL still can; the
/// `export-scratch` label identifies leftovers in `container list`.)
pub(super) struct ScratchContainer(pub(super) String);

impl ScratchContainer {
    pub(super) fn create(name: &str, image: &str, machine: &str) -> Result<Self> {
        let label = format!("{}={machine}", label_key(LABEL_EXPORT_SCRATCH));
        // --entrypoint only satisfies `create`'s requirement that a
        // command be specified even for images with no Cmd — the
        // scratch container is never started.
        run_container_quiet(&[
            "create",
            "--name",
            name,
            "--label",
            &label,
            "--entrypoint",
            "/bin/sh",
            image,
        ])
        .context("failed to create the export scratch container")?;
        Ok(Self(name.to_string()))
    }
}

impl Drop for ScratchContainer {
    fn drop(&mut self) {
        let _ = container_cmd()
            .args(["delete", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The scratch container's name: the export target's (truncated to fit
/// the 63-char `container` limit with the `-export-<timestamp>` suffix)
/// so a leftover is recognizable.
pub(super) fn export_scratch_name(of: &str) -> String {
    let base: String = of.chars().take(44).collect();
    format!("{}-export-{}", base.trim_end_matches('-'), timestamp())
}

/// The image a scratch export container is created from: `prefer` — the
/// target's own image — when still local, else any local image. The
/// container is never booted, so the image only has to exist. Fails
/// when the daemon holds no images at all.
pub(super) fn scratch_image(prefer: &str) -> Result<String> {
    let images = container::list_images().unwrap_or_default();
    let is = |n: &str| {
        n == prefer
            || n.strip_prefix("docker.io/") == Some(prefer)
            || n.strip_prefix("docker.io/library/") == Some(prefer)
    };
    if let Some(i) = images.iter().find(|i| is(&i.configuration.name)) {
        return Ok(i.configuration.name.clone());
    }
    images
        .first()
        .map(|i| i.configuration.name.clone())
        .context("no local images to create the export scratch container from")
}

/// `cm --export` for a machine. `container export` snapshots
/// `containers/<id>/rootfs.ext4` literally — a machine's backing
/// container mounts its disk from plugin state instead, so export
/// can't see it. The workaround: clone the machine's ext4 into a
/// scratch container that exports normally, then delete it (the
/// machine → distro `migrate` path uses the same clone mechanism).
/// The machine is stopped while its disk is cloned — a running
/// machine's clone would only be crash-consistent — and restarted if
/// it was running.
pub fn export_machine(machine: &str, output: Option<&Path>) -> Result<()> {
    ensure_started()?;
    // Reads and writes the daemon's containers/ directory internals.
    container::warn_unverified_version();
    let detail = container::inspect_machine(machine)?;
    if let Some(o) = output {
        check_export_output(o)?;
    }
    let scratch = ScratchContainer::create(
        &export_scratch_name(machine),
        &scratch_image(
            detail
                .image
                .as_ref()
                .map(|i| i.reference.as_str())
                .unwrap_or_default(),
        )?,
        machine,
    )?;
    let was_running = detail.is_running();
    let cloned = (|| -> Result<()> {
        if was_running {
            run_container_quiet(&["machine", "stop", machine])?;
        }
        let src = machine_rootfs(&detail)?
            .with_context(|| format!("machine `{machine}` has no root filesystem to export"))?;
        // `fs::copy` is a clonefile on APFS — instant and copy-on-write.
        fs::copy(&src, container_dir(&scratch.0)?.join("rootfs.ext4"))
            .with_context(|| format!("failed to clone {}", src.display()))?;
        Ok(())
    })();
    if was_running && let Err(e) = boot_machine(machine) {
        eprintln!("container-distro: warning: failed to restart machine `{machine}`: {e:#}");
    }
    cloned?;
    let mut args = vec!["export".to_string(), scratch.0.clone()];
    if let Some(o) = output {
        args.push("--output".into());
        args.push(o.to_string_lossy().into_owned());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container(&args)
}

/// `cm --import` for a machine: wrap the rootfs tar as an OCI image
/// (`oci::build_layout`), `image load` it, and `machine create` from
/// it. A boot probe then retries the documented first-boot flake — the
/// first `machine run` after `machine create` can lose a race while the
/// VM restarts after provisioning, for stock and imported images alike
/// — and leaves the machine stopped, like `wsl --import`.
pub fn import_machine(name: &str, file: &Path, arch: Option<Arch>, force: bool) -> Result<()> {
    validate_name(name)?;
    ensure_started()?;
    if container::list_machines()?.iter().any(|m| m.id == name) {
        bail!("a machine named `{name}` already exists");
    }
    // A same-named distro would shadow the machine everywhere in `cm`.
    bail_distro_shadow(name, force)?;
    let stdin_copy;
    let rootfs = if file == Path::new("-") {
        stdin_copy = oci::stdin_to_tempfile()?;
        stdin_copy.path()
    } else {
        file
    };
    // Content-keyed like distro imports — a machine imported from the
    // same archive a distro used (or a retry after a failed create)
    // skips the wrap+load entirely.
    let arch = arch.unwrap_or_else(Arch::host);
    let sha256 = oci::hash_file(rootfs)?;
    let labels = [
        (label_key(LABEL_ROOTFS_SHA256), sha256.clone()),
        (
            label_key(LABEL_IMPORTED_FROM),
            if file == Path::new("-") {
                "-".to_string()
            } else {
                file.to_string_lossy().into_owned()
            },
        ),
    ];
    let shared = ensure_rootfs_image(rootfs, &sha256, arch, &labels)?;
    // The machine records its own `local/machine-<name>` tag on the
    // shared image — the tag `--unregister` untags.
    let reference = per_importer_ref(&shared, &machine_import_ref(name, &sha256));
    // `machine create` boots the machine; its internal boot absorbs the
    // first-boot provisioning reboot. `--no-boot` is avoided on purpose:
    // a `machine run` on a never-booted machine races the boot and fails
    // every time (upstream apple/container#2024-adjacent). A failed create keeps the
    // shared tag — a cache entry — but drops the per-machine one, which
    // belongs to a machine that never landed.
    if let Err(e) = run_container(&[
        "machine",
        "create",
        "--name",
        name,
        "--arch",
        arch.as_str(),
        &reference,
    ]) {
        if reference != shared {
            untag_image(&reference);
        }
        return Err(e);
    }
    // Boot probe: the first `machine run` after `machine create` can
    // still lose the race while the VM settles — retry with a pause.
    let booted = (0..5).any(|attempt| {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        container::run_command(
            Some(name),
            None,
            None,
            &[],
            None,
            &["true".to_string()],
            ArgvMode::Shell,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    });
    if booted {
        // `wsl --import` registers without starting; leave it stopped.
        let _ = run_container_quiet(&["machine", "stop", name]);
    } else {
        eprintln!(
            "warning: `{name}` was created but did not boot; see `container machine logs {name}`"
        );
    }
    Ok(())
}

/// A distro spec reproducing a machine: its image, resources, home
/// mount, and provisioned account, with `opts` overrides on top
/// (`--restricted` remains a defaults preset, like at `create`).
pub(super) fn spec_from_machine(
    name: &str,
    m: &MachineDetail,
    opts: &CreateOptions,
) -> Result<DistroSpec> {
    let user = match &m.user_setup {
        Some(u) => HostUser {
            name: u.username.clone(),
            uid: u.uid,
            gid: u.gid,
        },
        None => host_user()?,
    };
    let image = m
        .image
        .as_ref()
        .map(|i| i.reference.clone())
        .with_context(|| "machine has no image reference".to_string())?;
    // The machine's recorded settings are source intent: they lose to
    // the user's flags and the preset, and resurface on `--reset`.
    let source = Intent {
        home_mount: Some(m.home_mount.as_deref().unwrap_or("rw").parse()?),
        cpus: m.cpus,
        memory: m.memory.map(|b| format!("{}M", b / (1024 * 1024))),
        ..Intent::default()
    };
    let mut spec = DistroSpec {
        name: name.to_string(),
        image,
        // An explicit --arch wins over the machine's recorded platform —
        // the same explicit-beats-source precedence as the options.
        arch: opts.arch.or_else(|| {
            m.platform
                .as_ref()
                .and_then(|p| p.architecture.parse::<Arch>().ok())
        }),
        mounts: opts.volumes.clone(),
        publish: opts.publish.clone(),
        user,
        preset: opts.preset(),
        explicit: create_intent(opts),
        source,
        admin_grant: false,
        origin: None,
    };
    let (resolved, warnings) = spec.resolve();
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    spec.admin_grant = resolved.admin;
    warn_public_publish(&spec.publish);
    Ok(spec)
}

/// The machine plugin's per-machine state directory.
fn machine_state_dir(name: &str) -> Result<PathBuf> {
    Ok(container::app_root()
        .context("`container system status` did not report an appRoot")?
        .join("plugin-state/machine-apiserver/machines")
        .join(name))
}

/// The `source` of a `rootFsOverride` record in a container's
/// runtime-configuration.json — where the daemon mounts its disk from.
pub(super) fn rootfs_override_source(json: &str) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .pointer("/options/rootFsOverride/source")?
        .as_str()
        .map(PathBuf::from)
}

/// The `source` of a `rootfs.json`-shaped mount record.
fn rootfs_source(json: &str) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("source")?
        .as_str()
        .map(PathBuf::from)
}

/// A machine's ext4 rootfs: what its backing container mounts when
/// there is one (a machine gets a backing container only once booted),
/// else the path the machine plugin state records. `None` means no
/// materialized rootfs — unusual, and treated like a never-booted
/// distro's missing rootfs: the target starts from the image.
pub(super) fn machine_rootfs(m: &MachineDetail) -> Result<Option<PathBuf>> {
    let app =
        container::app_root().context("`container system status` did not report an appRoot")?;
    if let Some(cid) = &m.container_id
        && let Some(src) = fs::read_to_string(
            app.join("containers")
                .join(cid)
                .join("runtime-configuration.json"),
        )
        .ok()
        .and_then(|s| rootfs_override_source(&s))
        && src.is_file()
    {
        return Ok(Some(src));
    }
    let dir = machine_state_dir(&m.id)?;
    if let Some(src) = fs::read_to_string(dir.join("rootfs.json"))
        .ok()
        .and_then(|s| rootfs_source(&s))
        && src.is_file()
    {
        return Ok(Some(src));
    }
    let ext4 = dir.join("rootfs.ext4");
    Ok(ext4.is_file().then_some(ext4))
}

/// Boot a machine (`machine run` is the boot primitive; there is no
/// `machine start`).
pub(super) fn boot_machine(name: &str) -> Result<()> {
    run_container_quiet(&["machine", "run", "-i", "-n", name, "--", "true"])
        .with_context(|| format!("failed to boot machine `{name}`"))
}

/// The `CreateOptions` `machine create` can't honor — `--out` rejects
/// them rather than silently dropping user intent.
/// `migrate --out` accepts set-style changes the machine can honor
/// (`--cpus`, `--memory`, `--home-mount`, `--ssh`, `--preset`/
/// `--restricted`/`--trusted` — they shift what the resolution
/// produces for the machine's own options) and refuses the ones it
/// can't, so a flag never silently no-ops.
pub(super) fn reject_machine_opts(opts: &CreateOptions) -> Result<()> {
    let mut bad: Vec<&str> = Vec::new();
    if !opts.volumes.is_empty() {
        bad.push("--volume");
    }
    if opts.automount.is_some() {
        bad.push("--automount");
    }
    if !opts.publish.is_empty() {
        bad.push("--publish");
    }
    if opts.network.is_some() {
        bad.push("--network");
    }
    // `--ssh` is a redundant opt-in the machine honors; `--no-ssh`
    // pins something it can never do.
    if opts.no_ssh {
        bad.push("--no-ssh");
    }
    if opts.sudo || opts.no_sudo {
        bad.push("--sudo/--no-sudo");
    }
    if opts.terminal_profile || opts.no_terminal_profile {
        bad.push("--terminal-profile/--no-terminal-profile");
    }
    if opts.files || opts.no_files {
        bad.push("--files/--no-files");
    }
    // `--ignore-wsl-conf` is fine — a machine never honors the file
    // anyway, so ignoring it asks for nothing the machine doesn't do.
    if opts.use_wsl_conf.is_some() {
        bad.push("--use-wsl-conf");
    }
    if !bad.is_empty() {
        bail!(
            "{} cannot be applied to a machine (supported overrides: \
             --cpus, --memory, --home-mount, --ssh, --preset/--restricted/--trusted)",
            bad.join(", ")
        );
    }
    Ok(())
}

/// The image to `machine create` from when migrating a distro out: the
/// distro's own when it is still local. When it is gone and a
/// filesystem is being carried, any local image does — the rootfs is
/// replaced right after the first boot. Without a filesystem to carry
/// the image is the machine's whole content, so the distro's reference
/// is passed through for `machine create` to pull.
pub(super) fn machine_create_image(spec_image: &str, carrying: bool) -> Result<String> {
    let images = container::list_images().unwrap_or_default();
    let is = |n: &str| {
        n == spec_image
            || n.strip_prefix("docker.io/") == Some(spec_image)
            || n.strip_prefix("docker.io/library/") == Some(spec_image)
    };
    if let Some(i) = images.iter().find(|i| is(&i.configuration.name)) {
        return Ok(i.configuration.name.clone());
    }
    if !carrying {
        return Ok(spec_image.to_string());
    }
    images
        .first()
        .map(|i| {
            eprintln!(
                "container-distro: image `{spec_image}` is no longer local; creating the machine from {}",
                i.configuration.name
            );
            i.configuration.name.clone()
        })
        .context("no local images to create the machine from")
}

/// `container machine create` for a distro migrating out: its
/// cpus/memory/home-mount with `opts` overrides on top. The machine
/// boots once — its plugin-state rootfs only materializes then — and is
/// stopped by the caller for the filesystem swap. A machine forwards
/// SSH_AUTH_SOCK at every boot with no opt-out, so only a distro that
/// resolves to agent-forwarding may migrate out — matching the `cm
/// --install --machine` refusal.
pub fn create_machine(
    name: &str,
    image: &str,
    spec: &DistroSpec,
    opts: &CreateOptions,
) -> Result<()> {
    // `opts` applies as set-style changes to a scratch copy of the
    // spec — the journal is never written, so `--keep` can't leak a
    // machine-shaped tweak back into the distro. A machine forwards
    // SSH_AUTH_SOCK at every boot with no opt-out, so migrating out is
    // itself the opt-in: warn when `ssh` lands off, and refuse only an
    // explicit `--no-ssh` pin (`reject_machine_opts` refuses an
    // `opts` one; a pin recorded on the distro still blocks).
    reject_machine_opts(opts)?;
    let changes = opts.spec_changes();
    let (result, warnings) = changes.apply(spec)?;
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    let (resolved, _) = result.resolve();
    if !resolved.ssh {
        if result.explicit.ssh == Some(false) {
            bail!(
                "`container machine` always forwards your SSH agent — {name} has \
                 agent forwarding pinned off; `distro set --ssh` it first (or \
                 drop the pin with `set --reset ssh`)"
            );
        }
        eprintln!("container-distro: `container machine` forwards your SSH agent at every boot");
    }
    let mut a: Vec<String> = ["machine", "create", "--name", name]
        .map(String::from)
        .to_vec();
    if let Some(c) = resolved.cpus {
        a.extend(["--cpus".into(), c.to_string()]);
    }
    if let Some(m) = &resolved.memory {
        a.extend(["--memory".into(), m.clone()]);
    }
    a.extend(["--arch".into(), resolved.arch.as_str().into()]);
    a.extend(["--home-mount".into(), resolved.home_mount.as_str().into()]);
    a.push(image.to_string());
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    run_container(&a)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use super::super::test_support::detail_json;

    #[test]
    fn machine_detail_parses_resources() {
        let d = detail_json();
        assert_eq!(d.cpus, Some(4));
        assert_eq!(d.memory, Some(4294967296));
    }

    #[test]
    fn rootfs_source_parses_mount_record() {
        let json = r#"{"options":[],"destination":"/","type":{"block":{"format":"ext4"}},
            "source":"/x/plugin-state/machine-apiserver/machines/m/rootfs.ext4"}"#;
        assert_eq!(
            rootfs_source(json),
            Some(PathBuf::from(
                "/x/plugin-state/machine-apiserver/machines/m/rootfs.ext4"
            ))
        );
        assert_eq!(rootfs_source("not json"), None);
        assert_eq!(rootfs_source("{}"), None);
    }
}
