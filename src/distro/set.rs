/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `set`: applying spec changes.

use super::adopt::ensure_journal;
use super::converge::{dismantle, materialize};
use super::create::{boot, check_sources, warn_public_publish};
use super::host::host_home;
use super::images::remove_snapshot_images;
use super::journal::{DistroLock, refresh_conf_snapshots, write_conf_snapshots};
use super::{find, run_container_quiet};
use crate::container::{self, ContainerInfo, ensure_started};
use crate::distro::spec::{DistroSpec, HostUser, ResolvedSpec, SpecChanges, size_bytes};
use crate::distro::terminal;
use anyhow::{Result, bail};

/// Field-level differences between the journaled `want` and `actual`,
/// a spec reconstructed from a live container — one human-readable line
/// each, empty when they agree. List fields compare as sets (`inspect`
/// ordering isn't stable); memory compares as bytes so `4G` and `4096M`
/// don't read as drift.
fn spec_diff(want: &ResolvedSpec, actual: &ResolvedSpec) -> Vec<String> {
    let mut out = Vec::new();
    macro_rules! field {
        ($name:literal, $w:expr, $a:expr) => {
            if $w != $a {
                out.push(format!("{}: journal {}, running {}", $name, $w, $a))
            }
        };
    }
    fn opt(o: &Option<String>) -> &str {
        o.as_deref().unwrap_or("default")
    }
    let user = |u: &HostUser| format!("{}({}:{})", u.name, u.uid, u.gid);
    // `inspect` reports the expanded reference (`docker.io/local/...`)
    // while the journal keeps the shorthand — same_image applies
    // registry/tag defaults so the spellings compare equal.
    if !container::same_image(&want.image, &actual.image) {
        out.push(format!(
            "image: journal {}, running {}",
            want.image, actual.image
        ));
    }
    field!("arch", want.arch.as_str(), actual.arch.as_str());
    field!("preset", want.preset.as_str(), actual.preset.as_str());
    // Unpinned resources are "the host picks" — `create` materializes
    // them into the container config, and a reconstructed spec can't
    // tell a host-picked 7 from a pinned one, so any value is
    // consistent with a `None` journal. Only a pin can drift.
    if let Some(w) = want.cpus
        && actual.cpus != Some(w)
    {
        out.push(format!(
            "cpus: journal {w}, running {}",
            actual.cpus.map_or("default".to_string(), |c| c.to_string())
        ));
    }
    // Both sides of a memory compare go through `size_bytes` so unit
    // spelling doesn't matter; an unparseable side falls back to a
    // string compare.
    let mem = |o: &Option<String>| o.as_deref().and_then(size_bytes);
    if let Some(w) = &want.memory {
        let drifts = match (mem(&want.memory), mem(&actual.memory)) {
            (Some(w), Some(a)) => w != a,
            _ => Some(w.as_str()) != actual.memory.as_deref(),
        };
        if drifts {
            out.push(format!(
                "memory: journal {w}, running {}",
                opt(&actual.memory)
            ));
        }
    }
    field!(
        "home-mount",
        want.home_mount.as_str(),
        actual.home_mount.as_str()
    );
    field!(
        "automount",
        want.automount.as_str(),
        actual.automount.as_str()
    );
    field!("network", opt(&want.network), opt(&actual.network));
    field!("ssh", want.ssh, actual.ssh);
    field!("admin", want.admin, actual.admin);
    field!("admin-grant", want.admin_grant, actual.admin_grant);
    field!("wsl-conf", want.wsl_conf, actual.wsl_conf);
    field!("user", user(&want.user), user(&actual.user));
    let mut list = |name: &str, w: Vec<String>, a: Vec<String>| {
        for m in w.iter().filter(|m| !a.contains(m)) {
            out.push(format!("{name}: journal has `{m}`, running doesn't"));
        }
        for m in a.iter().filter(|m| !w.contains(m)) {
            out.push(format!("{name}: running has `{m}` the journal doesn't"));
        }
    };
    list(
        "mounts",
        want.mounts.iter().map(ToString::to_string).collect(),
        actual.mounts.iter().map(ToString::to_string).collect(),
    );
    list(
        "publish",
        want.publish.iter().map(ToString::to_string).collect(),
        actual.publish.iter().map(ToString::to_string).collect(),
    );
    out
}

/// A container already running when the lock was taken may predate the
/// journal's last write — an interrupted `set`, or tampering with the
/// recorded config. Reconstruct its spec and diff it against the
/// journal so drift is loud rather than silently adopted.
pub(super) fn warn_spec_drift(name: &str, info: &ContainerInfo, lock: &DistroLock) {
    let want = lock.spec();
    let actual = host_home().and_then(|home| DistroSpec::from_container(info, &home));
    match (want, actual) {
        (Ok(want), Ok(actual)) => {
            for d in spec_diff(&want.resolve().0, &actual.resolve().0) {
                eprintln!("container-distro: warning: `{name}` drifted from its journal: {d}");
            }
        }
        (Err(e), _) => {
            eprintln!("container-distro: warning: can't check `{name}` against its journal: {e:#}")
        }
        (_, Err(e)) => eprintln!(
            "container-distro: warning: `{name}`'s running container can't be \
             verified against its journal ({e:#}) — its config isn't trusted"
        ),
    }
}

/// `container distro set`: update `name`'s journaled spec. A running
/// distro is stopped and rebooted under the new settings; a stopped one
/// is just re-journaled — its container is fresh-created at the next
/// start either way. Conceptually stop → update → start, so an
/// interruption lands in the same resting state a normal stop does.
/// Like `machine boot`, a failed materialize leaves the distro stopped
/// with the new spec committed; there is no stale config to fall back
/// to.
///
/// Tier and resets resolve first (`--preset T` re-derives the
/// unpinned options; `--reset <opt>` un-pins one; `--reset <tier>`
/// clears every pin), then the per-option flags pin on top, so a
/// report of the resulting spec can say what changed and what the
/// user's own flags still pin.
pub fn set(name: &str, changes: &SpecChanges) -> Result<()> {
    ensure_started()?;
    container::warn_unverified_version();
    if changes.is_empty() {
        bail!("nothing to change (see `container distro set --help`)");
    }
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    let old = lock.spec()?;
    let (new, warnings) = changes.apply(&old)?;
    for w in warnings {
        eprintln!("container-distro: {w}");
    }
    for line in new.change_report(&old) {
        println!("{line}");
    }
    warn_public_publish(&changes.add_publish);
    check_sources(&new)?;
    let info = find(name).ok();
    let was_running = info.as_ref().is_some_and(|i| i.is_running());
    if let Some(info) = &info {
        if was_running {
            refresh_conf_snapshots(name, info.id(), &old);
            run_container_quiet(&["stop", info.id()])?;
        }
        dismantle(name, info.id(), &lock)?;
    }
    // Turning config honoring off also forgets what was captured —
    // a distro that ignores wsl.conf should hold no guest config.
    if new.resolve().0.wsl_conf.is_empty() && !old.resolve().0.wsl_conf.is_empty() {
        let _ = write_conf_snapshots(name, None, None);
    }
    lock.write_spec(&new)?;
    // Profile intent is journaled too — resync when the resolved value
    // moved (a flag, a `--reset`, a tier change unpinning it) or a flag
    // was passed at all.
    let enabled = new.resolve().0.terminal_profile;
    if enabled != old.resolve().0.terminal_profile || changes.terminal_profile.is_some() {
        terminal::sync(name, enabled);
        // An explicit opt-in (or a resolution flip to enabled) imports
        // the profile — silently where that's safe.
        if enabled {
            let _ = terminal::ensure_profile(name);
        }
    }
    if was_running {
        let id = materialize(name, &new, &lock)?;
        boot(name, &id, &lock)?;
    }
    remove_snapshot_images(name, Some(new.image.as_str()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::distro::spec::{DistroSpec, HomeMount};

    use super::super::test_support::spec;

    fn resolved(s: &DistroSpec) -> crate::distro::spec::ResolvedSpec {
        s.resolve().0
    }

    /// The start-time integrity check: identical specs produce no diff;
    /// drifted fields name themselves; `4G` vs `4096M` is not drift.
    #[test]
    fn spec_diff_names_drifted_fields() {
        let want = resolved(&spec());
        // `from_container` normalizes memory to `NM` — same bytes is
        // not drift.
        let mut actual = spec();
        actual.explicit.memory = Some("4096M".into());
        assert!(spec_diff(&want, &resolved(&actual)).is_empty());

        let mut actual = spec();
        actual.explicit.cpus = Some(8);
        actual.explicit.ssh = Some(false);
        actual.explicit.home_mount = Some(HomeMount::Ro);
        let diff = spec_diff(&want, &resolved(&actual));
        assert!(diff.iter().any(|d| d.starts_with("cpus:")), "{diff:?}");
        assert!(diff.iter().any(|d| d.starts_with("ssh:")), "{diff:?}");
        assert!(
            diff.iter().any(|d| d.starts_with("home-mount:")),
            "{diff:?}"
        );

        // List fields diff per-entry, unordered.
        let mut actual = spec();
        actual.mounts.pop();
        actual.mounts.push("/x:/y".parse().unwrap());
        let diff = spec_diff(&want, &resolved(&actual));
        assert!(
            diff.iter()
                .any(|d| d == "mounts: journal has `/a:/b:ro`, running doesn't"),
            "{diff:?}"
        );
        assert!(
            diff.iter()
                .any(|d| d == "mounts: running has `/x:/y` the journal doesn't"),
            "{diff:?}"
        );
    }

    /// An unpinned journal resource is "the host picks": whatever the
    /// host materialized into the container (a reconstructed spec
    /// can't distinguish it from a pin) is consistent — only a journal
    /// pin mismatching the running value is drift.
    #[test]
    fn spec_diff_unpinned_resources_dont_drift() {
        let mut want = spec();
        want.explicit.cpus = None;
        want.explicit.memory = None;
        let want = resolved(&want);

        // Host picked 7/32768M — the reconstruction pins the
        // materialized values; not drift.
        let mut actual = spec();
        actual.explicit.cpus = Some(7);
        actual.explicit.memory = Some("32768M".into());
        assert!(spec_diff(&want, &resolved(&actual)).is_empty());

        // A pinned journal value that doesn't match the run is drift.
        let mut want = spec();
        want.explicit.memory = None;
        let want = resolved(&want);
        let diff = spec_diff(&want, &resolved(&actual));
        assert!(diff.iter().any(|d| d.starts_with("cpus:")), "{diff:?}");
        assert!(!diff.iter().any(|d| d.starts_with("memory:")), "{diff:?}");
    }

    /// The journal stores the shorthand ref; `inspect` expands it
    /// (`docker.io/local/...`) — the same image either way, not drift.
    #[test]
    fn spec_diff_image_ref_spelling_isnt_drift() {
        let mut want = spec();
        want.image = "local/distro-d1:imported-abc123".into();
        let want = resolved(&want);

        let mut actual = spec();
        actual.image = "docker.io/local/distro-d1:imported-abc123".into();
        assert!(spec_diff(&want, &resolved(&actual)).is_empty());

        let mut actual = spec();
        actual.image = "docker.io/local/distro-d1:imported-def456".into();
        let diff = spec_diff(&want, &resolved(&actual));
        assert!(diff.iter().any(|d| d.starts_with("image:")), "{diff:?}");
    }
}
