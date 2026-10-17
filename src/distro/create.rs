/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `create`: options, spec building, and first boot.

use super::adopt::{ensure_journal, ensure_state};
use super::assets::{journal_assets_dir, refresh_mounted_assets};
use super::converge::{converge, dismantle, materialize};
use super::default::{DefaultType, no_effective_default, write_default};
use super::host::{default_resources, host_home, host_user};
use super::journal::{
    DistroLock, distro_exists, journal_dir, read_journal, refresh_conf_snapshots, write_journal,
};
use super::names::{bail_if_staged, bail_machine_shadow};
use super::{find, iso_now, run_container, run_container_quiet};
use crate::container::{self, ContainerInfo, default_machine_name, ensure_started, validate_name};
use crate::distro::spec::{
    Arch, Automount, ConfList, DistroOrigin, DistroSpec, HomeMount, HostUser, INIT_DIR, Intent,
    MountSpec, OriginSource, Preset, PublishSpec, ResolveCtx, ResolvedSpec, SpecChanges,
    automounts, mode_from_flags,
};
use crate::distro::terminal;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::{env, fs};

/// Options shared by `create` and `import`.
#[derive(Debug, Clone, Default, PartialEq, clap::Args)]
pub struct CreateOptions {
    /// Share a host directory: SRC:DST[:opt,...] where opts include
    /// ro, rw, and guest mount flags (nosuid, nodev, noexec, noatime,
    /// relatime, strictatime, sync, dirsync — repeatable)
    #[arg(short = 'v', long = "volume", value_name = "SRC:DST[:opt,...]")]
    pub volumes: Vec<MountSpec>,

    /// Share every /Volumes/<name> at /mnt/<name> (like WSL's
    /// /mnt/<drive>); `--automount ro` mounts read-only. Hidden,
    /// Apple-private (com.apple.*), and unreadable volumes are skipped.
    /// Re-resolved on `set` and when a stopped distro starts, so
    /// attaching or ejecting a volume needs no manual fix-up. With no
    /// --home-mount, your home directory gets the same access (warned)
    #[arg(long, value_enum, num_args = 0..=1, require_equals = true,
          default_missing_value = "rw", value_name = "rw|ro|none")]
    pub automount: Option<Automount>,

    /// Publish a port: [HOST_IP:]HOST_PORT[:GUEST_PORT][/PROTO]; HOST_IP
    /// defaults to 127.0.0.1 (repeatable)
    #[arg(short = 'p', long = "publish", value_name = "SPEC")]
    pub publish: Vec<PublishSpec>,

    /// Number of virtual CPUs (default: half the host's)
    #[arg(long)]
    pub cpus: Option<u64>,

    /// Memory, e.g. 8G (default: half the host's)
    #[arg(long)]
    pub memory: Option<String>,

    /// CPU architecture the distro's userspace is built for — `amd64`
    /// (x86-64) runs under Rosetta translation on Apple silicon. Pinned
    /// at create: the filesystem is already one arch, so `set` keeps it
    /// and can't change it
    #[arg(long, value_enum, value_name = "ARCH")]
    pub arch: Option<Arch>,

    /// How to share your macOS home directory (default: none —
    /// read-write only for distros installed through a catalog entry,
    /// or when --automount implies the same access)
    #[arg(long, value_enum)]
    pub home_mount: Option<HomeMount>,

    /// Attach to a container network by name ("none" for no network)
    #[arg(long, value_name = "NAME")]
    pub network: Option<String>,

    /// Forward the host SSH agent socket into the distro (default: only
    /// when the effective home-mount is rw — the same trust tier)
    #[arg(long, conflicts_with = "no_ssh")]
    pub ssh: bool,

    /// Do not forward the host SSH agent socket
    #[arg(long)]
    pub no_ssh: bool,

    /// Grant the provisioned user passwordless sudo/doas
    #[arg(long, conflicts_with = "no_sudo")]
    pub sudo: bool,

    /// Do not grant the provisioned user sudo/doas
    #[arg(long)]
    pub no_sudo: bool,

    /// The defaults tier the distro runs at — `default`,
    /// `restricted`, or `trusted`. Recorded in the journal so `set
    /// --preset` can re-derive every option you haven't pinned
    #[arg(long, value_enum, conflicts_with_all = ["restricted", "trusted"])]
    pub preset: Option<Preset>,

    /// Restricted defaults for semi-trusted images: home mount none, no
    /// network, no SSH agent, no sudo/doas, no host file-access relay.
    /// Explicit flags still apply.
    /// A convenience preset, not a sandbox. (`--preset restricted`)
    #[arg(long, visible_alias = "untrusted")]
    pub restricted: bool,

    /// Trusted defaults for images you fully trust: home mount rw,
    /// /Volumes automounted rw, SSH agent forwarded. Explicit flags
    /// still apply — the mirror of --restricted. (`--preset trusted`)
    #[arg(long, conflicts_with = "restricted")]
    pub trusted: bool,

    /// Honor the guest's /etc/wsl.conf — optionally a comma list of
    /// features: hostname, fstab, init, command, automount, or
    /// `all`/`none` (bare `--use-wsl-conf` is `all`). A bare list pins
    /// exactly those features (`none` honors nothing); `command` runs
    /// the file's [boot] command as root once per boot; `automount`
    /// lowers automount/home-mount for an archive whose config says
    /// enabled=false. Default: `all` for --trusted and catalog
    /// installs, `none` otherwise
    #[arg(long, value_name = "FEATURE,...|all|none", num_args = 0..=1,
          require_equals = true, default_missing_value = "all")]
    pub use_wsl_conf: Option<ConfList>,

    /// Stop honoring /etc/wsl.conf entirely (the same as
    /// `--use-wsl-conf=none`)
    #[arg(long, conflicts_with_all = ["use_wsl_conf", "ignore_wsl_conf"])]
    pub no_wsl_conf: bool,

    /// Deny parts of /etc/wsl.conf — subtracts from what the preset and
    /// source would grant (`all` ignores the file entirely, the same
    /// as --no-wsl-conf)
    #[arg(long, value_name = "FEATURE,...|all|none")]
    pub ignore_wsl_conf: Option<ConfList>,

    /// Create without booting
    #[arg(long)]
    pub no_boot: bool,

    /// Make this the default distro (automatic when nothing is the default)
    #[arg(long)]
    pub set_default: bool,

    /// Create even when the name is taken in the other namespace — a
    /// same-named `container machine`, or a same-named distro when
    /// creating a machine. `cm` resolves a shared name to the distro,
    /// shadowing the machine, so the collision is refused without this
    #[arg(long)]
    pub force: bool,

    /// Generate macOS terminal profiles (the `.terminal` launcher +
    /// iTerm2 dynamic profile) for the new distro — the default, unless
    /// the package opts out (`windowsterminal.enabled = false`). On
    /// `set`, refreshes the launcher; open it to re-import
    #[arg(long, conflicts_with = "no_terminal_profile")]
    pub terminal_profile: bool,

    /// Don't generate macOS terminal profiles for the new distro
    #[arg(long)]
    pub no_terminal_profile: bool,

    /// Record host file-access intent for the planned `container
    /// distro fs` — the default, except under --restricted
    #[arg(long, conflicts_with = "no_files")]
    pub files: bool,

    /// Record intent against host file access
    #[arg(long)]
    pub no_files: bool,

    /// The caller opens the profile in Terminal.app right after
    /// create (`cm --install --terminal`): that open *is* the import,
    /// and its window must run the real launcher — not the
    /// self-replacing no-op a silent import arms.
    #[arg(skip)]
    pub will_open_terminal: bool,
}

impl CreateOptions {
    /// The defaults preset the flags select. `--preset`,
    /// `--restricted`, and `--trusted` conflict at parse, so at most
    /// one is set.
    pub fn preset(&self) -> Preset {
        self.preset.unwrap_or({
            if self.restricted {
                Preset::Restricted
            } else if self.trusted {
                Preset::Trusted
            } else {
                Preset::Default
            }
        })
    }
}

/// The mounts `--automount <mode>` would add right now: every readable
/// `/Volumes` entry at its canonical `/mnt` target, skipping targets
/// already in `taken` (e.g. user mounts).
fn scan_automounts(
    mode: Automount,
    taken: &std::collections::HashSet<String>,
) -> Result<Vec<MountSpec>> {
    let entries: Vec<(String, bool)> = fs::read_dir("/Volumes")
        .context("failed to read /Volumes")?
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_link = e.file_type().is_ok_and(|t| t.is_symlink());
            (e.file_name().to_string_lossy().into_owned(), is_link)
        })
        .collect();
    let mut taken = taken.clone();
    let mut mounts = Vec::new();
    for m in automounts(&entries, mode.read_only()) {
        // VZ rejects shares the virtiofs helper can't enumerate
        // (TCC/SIP-protected volumes like Time Machine destinations)
        // with "directory sharing device configuration is invalid";
        // probe readability so one bad volume can't sink the boot.
        // `.next()` is needed — readdir errors surface lazily.
        if fs::read_dir(&m.source)
            .and_then(|mut d| d.next().transpose())
            .is_err()
        {
            eprintln!(
                "container-distro: skipping automount {}: not readable on the host",
                m.source
            );
            continue;
        }
        if !taken.insert(m.target.clone()) {
            eprintln!(
                "container-distro: skipping automount {}: {} is already a mount target",
                m.source, m.target
            );
            continue;
        }
        mounts.push(m);
    }
    Ok(mounts)
}

/// The `/Volumes` shares to add at materialize time for `spec` — the
/// automount scan when the mode is enabled, skipping targets the
/// user's own mounts already claim. Derived, never journaled.
pub(super) fn auto_shares(r: &ResolvedSpec) -> Result<Vec<MountSpec>> {
    if !r.automount.enabled() {
        return Ok(Vec::new());
    }
    let taken = r.mounts.iter().map(|m| m.target.clone()).collect();
    scan_automounts(r.automount, &taken)
}

/// The host-dependent inputs `DistroSpec::resolve` can't see.
fn resolve_ctx() -> ResolveCtx {
    let (cpus, memory) = default_resources();
    ResolveCtx { cpus, memory }
}

/// A clap flag pair (`--ssh`/`--no-ssh`) as an `Option<bool>` pin —
/// they conflict at parse, so at most one is set.
fn flag_pair(on: bool, off: bool) -> Option<bool> {
    if on {
        Some(true)
    } else if off {
        Some(false)
    } else {
        None
    }
}

/// The `Intent` a `CreateOptions`' flags pin — the user side of the
/// spec; the source side comes from the catalog entry or machine.
pub(super) fn create_intent(opts: &CreateOptions) -> Intent {
    // `--no-wsl-conf` is `--use-wsl-conf=none` by another name; the
    // parse-level conflict keeps the two from arriving together.
    let use_conf = opts
        .use_wsl_conf
        .clone()
        .or_else(|| opts.no_wsl_conf.then(ConfList::none));
    Intent {
        home_mount: opts.home_mount,
        automount: opts.automount,
        ssh: flag_pair(opts.ssh, opts.no_ssh),
        admin: flag_pair(opts.sudo, opts.no_sudo),
        network: opts.network.clone(),
        cpus: opts.cpus,
        memory: opts.memory.clone(),
        wsl_conf: mode_from_flags(use_conf.as_ref(), opts.ignore_wsl_conf.as_ref(), None),
        terminal_profile: flag_pair(opts.terminal_profile, opts.no_terminal_profile),
        files: flag_pair(opts.files, opts.no_files),
    }
}

/// The options half of a spec: `opts`' flags as `explicit` intent
/// resolved against `source`, before identity (name/image/user)
/// lands — [`build_spec`] finishes those. Shared with the
/// machine-tier checks (`--install --machine`, `migrate --out`) so
/// what they probe is exactly the spec a distro would run.
fn options_spec(opts: &CreateOptions, source: &Intent) -> DistroSpec {
    DistroSpec {
        // `resolve` never reads identity — `build_spec` fills it in.
        name: String::new(),
        image: String::new(),
        arch: opts.arch,
        mounts: opts.volumes.clone(),
        publish: opts.publish.clone(),
        user: HostUser::default(),
        preset: opts.preset(),
        explicit: create_intent(opts),
        source: source.clone(),
        admin_grant: false,
        origin: None,
    }
}

impl CreateOptions {
    /// The intent-side spec these flags describe for `source` —
    /// identity (name/image/user) is left for the caller; `create`'s
    /// path fills it via `build_spec`, and `create_machine` layers
    /// `spec_changes` on a scratch copy.
    pub fn spec(&self, source: &Intent) -> DistroSpec {
        options_spec(self, source)
    }

    /// The spec these flags resolve to for `source` — computed by
    /// `DistroSpec::resolve` itself, so paths that need a resolved
    /// field without a journal (the machine-tier checks) derive
    /// exactly what a distro would.
    pub fn resolved_spec(&self, source: &Intent) -> ResolvedSpec {
        options_spec(self, source).resolve().0
    }

    /// `resolved_spec(source).home_mount` — for the `--home-mount`
    /// arg the machine checks still pass through.
    pub fn resolve_home_mount(&self, source: &Intent) -> HomeMount {
        self.resolved_spec(source).home_mount
    }

    /// The flags as `SpecChanges` — set-style overrides layered onto
    /// an existing spec (the `migrate --out` scratch spec). Only the
    /// options a machine can honor reach it; `reject_machine_opts`
    /// refuses the rest first.
    pub fn spec_changes(&self) -> SpecChanges {
        SpecChanges {
            preset: self.preset.or(if self.restricted {
                Some(Preset::Restricted)
            } else if self.trusted {
                Some(Preset::Trusted)
            } else {
                None
            }),
            cpus: self.cpus,
            memory: self.memory.clone(),
            home_mount: self.home_mount,
            automount: self.automount,
            network: self.network.clone(),
            ssh: flag_pair(self.ssh, self.no_ssh),
            admin: flag_pair(self.sudo, self.no_sudo),
            add_mounts: self.volumes.clone(),
            add_publish: self.publish.clone(),
            wsl_conf: self
                .use_wsl_conf
                .clone()
                .or_else(|| self.no_wsl_conf.then(ConfList::none)),
            wsl_conf_ignore: self.ignore_wsl_conf.clone(),
            terminal_profile: flag_pair(self.terminal_profile, self.no_terminal_profile),
            files: flag_pair(self.files, self.no_files),
            ..SpecChanges::default()
        }
    }
}

pub(super) fn build_spec(
    name: String,
    image: String,
    opts: &CreateOptions,
    source: &Intent,
) -> Result<DistroSpec> {
    validate_name(&name)?;
    let mut spec = options_spec(opts, source);
    spec.name = name;
    spec.image = image;
    spec.user = host_user()?;
    let (resolved, warnings) = spec.resolve();
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    // The grant env is armed at create — a guest that removes its own
    // privilege files stays unprivileged until an explicit `set
    // --sudo` re-arms it.
    spec.admin_grant = resolved.admin;
    if resolved.network.as_deref() == Some("none") && !spec.publish.is_empty() {
        eprintln!("container-distro: published ports have no effect without a network");
    }
    warn_public_publish(&spec.publish);
    Ok(spec)
}

/// Warn once per publish spec that binds a non-loopback address — the
/// flag that changes exposure for *other* machines on the LAN.
pub(super) fn warn_public_publish(specs: &[PublishSpec]) {
    for p in specs.iter().filter(|p| !p.is_loopback()) {
        eprintln!(
            "container-distro: warning: publishing on {}:{} — reachable beyond localhost (LAN); use 127.0.0.1 to bind locally",
            p.host_ip, p.host_port
        );
    }
}

pub(super) fn check_sources(spec: &DistroSpec) -> Result<()> {
    for m in &spec.mounts {
        if !Path::new(&m.source).is_dir() {
            bail!("mount source `{}` is not a directory", m.source);
        }
    }
    Ok(())
}

/// Create `spec`'s container under the ephemeral runtime `id`.
/// Automounts are resolved from the live `/Volumes` right here — the
/// journal stays declarative, so an attached or ejected volume is
/// picked up at every materialize, no rewrite needed.
pub(super) fn create_container(spec: &DistroSpec, id: &str) -> Result<()> {
    let assets = journal_assets_dir(&spec.name)?;
    let (resolved, _) = spec.resolve();
    let auto = auto_shares(&resolved)?;
    let args = resolved.create_args(
        &resolve_ctx(),
        id,
        &assets.to_string_lossy(),
        &host_home()?,
        &auto,
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_container_quiet(&args)
}

/// Boot distro `name`'s container `id` and (idempotently) provision
/// the user account.
pub(super) fn boot(name: &str, id: &str, lock: &DistroLock) -> Result<()> {
    let (r, _) = lock.spec()?.resolve();
    refresh_mounted_assets(name, r.admin)?;
    run_container_quiet(&["start", id])?;
    let init = format!("{INIT_DIR}/init");
    run_container(&["exec", "--user", "0:0", id, &init, "-u"])
        .with_context(|| format!("user setup failed in `{name}`"))?;
    // Snapshot the guest config while the container is up — the only
    // window `container cp` can read it — for host-side consumers now
    // and later.
    if let Ok(spec) = lock.spec() {
        refresh_conf_snapshots(name, id, &spec);
    }
    report_ports(name);
    Ok(())
}

/// Report the ports `name` publishes, now that it is listening —
/// loopback binds as info, anything wider as a LAN-exposure warning.
/// Read from the journal, not the container: the recorded spec is the
/// truth.
fn report_ports(name: &str) {
    let Ok(spec) = read_journal(name) else { return };
    for p in &spec.publish {
        if p.is_loopback() {
            println!("`{name}` listening on {}:{}", p.host_ip, p.host_port);
        } else {
            eprintln!(
                "container-distro: warning: `{name}` listening on {}:{} — reachable beyond localhost (LAN)",
                p.host_ip, p.host_port
            );
        }
    }
}

/// Start `name` under its lock if it isn't running — converge any
/// leftover container, materialize fresh from the journal (which is
/// where the automount scan happens), boot — and return the running
/// container.
pub(super) fn ensure_running_locked(name: &str, lock: &mut DistroLock) -> Result<ContainerInfo> {
    if let Some(info) = converge(name, lock)? {
        return Ok(info);
    }
    let spec = lock.spec()?;
    let id = materialize(name, &spec, lock)?;
    boot(name, &id, lock)?;
    find(name)
}

pub(super) fn ensure_running(name: &str) -> Result<ContainerInfo> {
    container::warn_unverified_version();
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    ensure_running_locked(name, &mut lock)
}

pub(super) fn create_from_spec(spec: &DistroSpec, opts: &CreateOptions) -> Result<String> {
    check_sources(spec)?;
    container::warn_unverified_version();
    ensure_state()?;
    // The name must be fresh in every form a distro can take — journal,
    // materialized container — and a same-named plain container would
    // collide at `create` anyway.
    if journal_dir(&spec.name)?.exists() || distro_exists(&spec.name)? {
        bail!("distro `{}` already exists", spec.name);
    }
    if container::list_containers()?
        .iter()
        .any(|c| c.id() == spec.name)
    {
        bail!(
            "a non-distro container named `{}` already exists",
            spec.name
        );
    }
    bail_if_staged(&spec.name)?;
    bail_machine_shadow(&spec.name, opts.force)?;
    write_journal(&spec.name, spec)?;
    let lock = DistroLock::acquire(&spec.name)?;
    let r = (|| -> Result<()> {
        // The create is validated now — `--no-boot` lands the same
        // resting state as a stopped distro: journal only.
        let id = materialize(&spec.name, spec, &lock)?;
        if opts.no_boot {
            dismantle(&spec.name, &id, &lock)?;
        } else {
            boot(&spec.name, &id, &lock)?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        // Leave the journal in place — a stopped distro. `start`
        // retries the materialization, `rm` discards it.
        if let Ok(info) = find(&spec.name)
            && !info.is_running()
        {
            let _ = dismantle(&spec.name, info.id(), &lock);
        }
        return Err(e);
    }
    // On request, or (like `container machine create`) when nothing
    // else can be the effective default — converge keeps the record
    // pointed at something real, so `no_effective_default` is only
    // true for a genuinely empty world.
    let set_default = opts.set_default || no_effective_default(&spec.name)?;
    if set_default {
        write_default(&DefaultType::Distro(spec.name.clone()))?;
    }
    let profiles = spec.resolve().0.terminal_profile;
    terminal::sync(&spec.name, profiles);
    if profiles && !opts.will_open_terminal {
        let _ = terminal::ensure_profile(&spec.name);
    }
    Ok(spec.name.clone())
}

/// Create (and unless `no_boot`, boot) a distro; returns its name.
/// `origin` is the provenance stamped into the journal; `requested`
/// is the source side of the spec's intent — what the catalog entry
/// or installer asked for (a catalog's `home-mount` request, for
/// one). It loses to every explicit flag and to the preset.
pub fn create(
    name: Option<String>,
    opts: &CreateOptions,
    image: &str,
    origin: OriginSource,
    requested: &Intent,
) -> Result<String> {
    ensure_started()?;
    let name = name.unwrap_or_else(|| default_machine_name(image));
    let mut spec = build_spec(name, image.to_string(), opts, requested)?;
    spec.origin = Some(stamped_origin(origin));
    create_from_spec(&spec, opts)
}

/// Journal provenance stamped at creation: when, what build, and from
/// what. `set` rewrites the spec but clones this field through — the
/// recorded version stays the *creating* build's.
pub(super) fn stamped_origin(source: OriginSource) -> DistroOrigin {
    DistroOrigin {
        created: iso_now(),
        created_with: env!("CARGO_PKG_VERSION").to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::distro::spec::{Automount, HomeMount, HostUser, Intent};

    use super::super::machine::{create_machine, spec_from_machine};
    use super::super::test_support::{detail_json, spec};

    #[test]
    fn home_mount_default_resolution() {
        // An explicit flag wins over everything — even --restricted.
        let opts = CreateOptions {
            home_mount: Some(HomeMount::Rw),
            restricted: true,
            automount: Some(Automount::Ro),
            ..CreateOptions::default()
        };
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::Rw);
        // --restricted pins none.
        let opts = CreateOptions {
            restricted: true,
            automount: Some(Automount::Rw),
            ..CreateOptions::default()
        };
        assert_eq!(
            opts.resolve_home_mount(&Intent::only_home_mount(HomeMount::Rw)),
            HomeMount::None
        );
        // An explicit --automount is followed (and warns).
        for (am, want) in [
            (Automount::Rw, HomeMount::Rw),
            (Automount::Ro, HomeMount::Ro),
            (Automount::None, HomeMount::None),
        ] {
            let opts = CreateOptions {
                automount: Some(am),
                ..CreateOptions::default()
            };
            assert_eq!(opts.resolve_home_mount(&Intent::default()), want, "{am:?}");
        }
        // --trusted presets rw regardless of source, and beats the
        // automount-follows default (ro volumes, still rw home).
        let opts = CreateOptions {
            trusted: true,
            ..CreateOptions::default()
        };
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::Rw);
        let opts = CreateOptions {
            trusted: true,
            automount: Some(Automount::Ro),
            ..CreateOptions::default()
        };
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::Rw);
        // A source request caps what the preset raises — ro stays ro
        // even under --trusted.
        assert_eq!(
            opts.resolve_home_mount(&Intent::only_home_mount(HomeMount::Ro)),
            HomeMount::Ro
        );
        // Otherwise a source request fills the unset slot — the
        // catalog's `rw` survives the `none` built-in default.
        let opts = CreateOptions::default();
        assert_eq!(
            opts.resolve_home_mount(&Intent::only_home_mount(HomeMount::Rw)),
            HomeMount::Rw
        );
        assert_eq!(opts.resolve_home_mount(&Intent::default()), HomeMount::None);
    }

    #[test]
    fn ssh_default_follows_home_tier() {
        // Catalog-tier rw → agent on; ad-hoc none → off.
        let opts = CreateOptions::default();
        for (default, want) in [(HomeMount::Rw, true), (HomeMount::None, false)] {
            let spec = build_spec(
                "d".into(),
                "img".into(),
                &opts,
                &Intent::only_home_mount(default),
            )
            .unwrap();
            assert_eq!(spec.resolve().0.ssh, want, "{default:?}");
        }
        // --restricted beats an explicit rw home: every hole needs its
        // own flag there.
        let opts = CreateOptions {
            restricted: true,
            home_mount: Some(HomeMount::Rw),
            ..CreateOptions::default()
        };
        assert!(
            !build_spec("d".into(), "i".into(), &opts, &Intent::default())
                .unwrap()
                .resolve()
                .0
                .ssh
        );
        // --trusted presets home rw, automount rw, and the agent.
        let opts = CreateOptions {
            trusted: true,
            ..CreateOptions::default()
        };
        let spec = build_spec("d".into(), "i".into(), &opts, &Intent::default()).unwrap();
        let (r, _) = spec.resolve();
        assert!(r.ssh);
        assert_eq!(r.home_mount, HomeMount::Rw);
        assert_eq!(r.automount, Automount::Rw);
    }

    #[test]
    fn migrate_out_refuses_a_pinned_agent_optout() {
        // A machine forwards SSH_AUTH_SOCK at every boot — `--machine`
        // is the opt-in, so an unpinned ssh-off distro only warns. The
        // one refusal is a distro with `--no-ssh` explicitly recorded.
        let mut spec = spec();
        spec.explicit.home_mount = Some(HomeMount::None);
        spec.explicit.ssh = Some(false);
        let err = create_machine("m", "img", &spec, &CreateOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("pinned off"), "{err}");
        // Without the pin the same spec proceeds past the check to
        // spawn `container` — not unit-testable here.
    }

    #[test]
    fn spec_from_machine_carries_settings() {
        let spec = spec_from_machine("d1", &detail_json(), &CreateOptions::default()).unwrap();
        assert_eq!(spec.name, "d1");
        assert_eq!(spec.image, "docker.io/library/alpine:latest");
        let (r, _) = spec.resolve();
        assert_eq!(r.cpus, Some(4));
        assert_eq!(r.memory.as_deref(), Some("4096M"));
        assert_eq!(r.home_mount, HomeMount::Ro);
        assert_eq!(
            spec.user,
            HostUser {
                name: "dp".into(),
                uid: 501,
                gid: 20
            }
        );
        // homeMount is ro → the agent default follows the home tier: off.
        assert!(!r.ssh && r.admin && spec.admin_grant);
        assert!(!r.is_restricted());
    }

    #[test]
    fn spec_from_machine_honors_overrides() {
        let opts = CreateOptions {
            cpus: Some(2),
            memory: Some("1G".into()),
            home_mount: Some(HomeMount::None),
            restricted: true,
            ..CreateOptions::default()
        };
        let spec = spec_from_machine("d1", &detail_json(), &opts).unwrap();
        let (r, _) = spec.resolve();
        assert_eq!(r.cpus, Some(2));
        assert_eq!(r.memory.as_deref(), Some("1G"));
        assert_eq!(r.home_mount, HomeMount::None);
        // --restricted still defaults network/ssh/admin.
        assert_eq!(r.network.as_deref(), Some("none"));
        assert!(!r.ssh && !r.admin);
    }
}
