/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The per-distro journal directory: paths, spec I/O, conf snapshots, and the distro lock.

use super::adopt::migrate_once;
use super::lock::{is_locked, open_locked_rw, set_locked, unlock, write_nofollow};
use super::{distro_name, distros, find};
use crate::catalog::wsl;
use crate::container::{self, container_cmd};
use crate::distro::spec::{Automount, DistroSpec, HomeMount, JournalSpec};
use crate::naming::state_dir;
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

/// The container's data directory under `container`'s appRoot.
pub(super) fn container_dir(id: &str) -> Result<PathBuf> {
    container::app_root()
        .map(|r| r.join("containers").join(id))
        .context("`container system status` did not report an appRoot")
}

/// The journal root — `state_dir()/distros`, inside our own Application
/// Support directory. Upstream keeps plugin state under
/// `appRoot/plugin-state/`, but that path lives inside *another app's*
/// data folder: an unentitled client writing there trips macOS's
/// app-data consent prompt, and resolving `appRoot` costs a
/// `container system status` spawn per journal read. The uchg flags —
/// not the location — are what keep guests from planting trusted
/// state. Every distro's durable self lives here; its container is
/// ephemeral.
fn journal_root() -> Result<PathBuf> {
    Ok(state_dir()?.join("distros"))
}

pub(super) fn journal_dir(name: &str) -> Result<PathBuf> {
    journal_root().map(|r| r.join(name))
}

fn journal_spec_path(name: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join("spec.json"))
}

pub(super) fn journal_fs_path(name: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join("rootfs.ext4"))
}

/// Files staged by an in-flight `set` on pre-journal builds, inside our
/// own state directory: `preserved/<name>.ext4` plus a
/// `preserved/<name>.json` journal. Kept only to fold leftovers into
/// the journal — nothing new is staged here.
pub(super) fn preserved_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("preserved"))
}

pub(super) fn preserved_rootfs(name: &str) -> Result<PathBuf> {
    Ok(preserved_dir()?.join(format!("{name}.ext4")))
}

pub(super) fn preserved_journal(name: &str) -> Result<PathBuf> {
    Ok(preserved_dir()?.join(format!("{name}.json")))
}

/// Keep `preserved/` and `distros/` present and `uchg`-locked: through
/// a rw-shared home a guest could otherwise plant journal entries a
/// later read would trust — a confused-deputy path to mounts nobody
/// asked for. Mutations unlock a scope briefly around their file ops
/// instead.
pub(super) fn ensure_state_locked() -> Result<()> {
    for dir in [preserved_dir()?, journal_root()?] {
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        set_locked(&dir, true)?;
    }
    // The pre-journal per-distro lockfiles are retired — `spec.json`
    // is the lock target now.
    let _ = fs::remove_dir_all(state_dir()?.join("locks"));
    Ok(())
}

/// Run `f` with `preserved/` unlocked, re-locking it afterwards — even
/// on error, where the operation's own failure is the one reported.
pub(super) fn with_preserved_unlocked<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = preserved_dir()?;
    unlock(&dir);
    let r = f();
    let relock = set_locked(&dir, true);
    match r {
        Err(e) => Err(e),
        Ok(v) => relock.map(|_| v),
    }
}

/// Run `f` with the journal root unlocked, re-locking it afterwards —
/// even on error, where `f`'s failure is the one reported.
pub(super) fn with_journal_root_unlocked<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = journal_root()?;
    unlock(&dir);
    let r = f();
    let relock = set_locked(&dir, true);
    match r {
        Err(e) => Err(e),
        Ok(v) => relock.map(|_| v),
    }
}

/// Run `f` with `name`'s journal dir unlocked — adding or removing an
/// entry needs it — re-locking afterwards.
pub(super) fn with_journal_dir_unlocked<T>(name: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = journal_dir(name)?;
    unlock(&dir);
    let r = f();
    let relock = set_locked(&dir, true);
    match r {
        Err(e) => Err(e),
        Ok(v) => relock.map(|_| v),
    }
}

/// Write `spec` as `name`'s journal — both the create path and the
/// adoption of a pre-journal distro land here. The directory and
/// `spec.json` come out `uchg`-locked.
pub(super) fn write_journal(name: &str, spec: &DistroSpec) -> Result<()> {
    ensure_state_locked()?;
    let dir = journal_dir(name)?;
    let spec_path = dir.join("spec.json");
    with_journal_root_unlocked(|| {
        if !dir.exists() {
            fs::create_dir(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        unlock(&spec_path);
        write_nofollow(
            &spec_path,
            &serde_json::to_vec_pretty(&spec.to_journal())?,
            0o644,
        )?;
        set_locked(&spec_path, true)?;
        set_locked(&dir, true)
    })
}

/// The journaled spec — the distro's trusted desired configuration.
/// The journal records intent plus the effective values at write
/// time; the intent is authoritative. Pre-preset journals get tier
/// and intent back-computed, derived automount entries a historic
/// journal recorded are dropped (automounts are computed, not
/// stored), and a recorded value that no longer matches the
/// resolution — a hand-edit, or a change in the rules — is noted
/// rather than silently adopted (the next write heals the record).
pub(super) fn read_journal(name: &str) -> Result<DistroSpec> {
    let journal: JournalSpec = serde_json::from_slice(
        &fs::read(journal_spec_path(name)?).context("failed to read the distro journal")?,
    )
    .context("invalid distro journal")?;
    if journal.name != name {
        bail!(
            "journal for `{name}` names `{}` — refusing to trust it",
            journal.name
        );
    }
    let (spec, notes) = journal.into_spec();
    for n in notes {
        eprintln!("container-distro: {n}");
    }
    Ok(spec)
}

/// The guest config files snapshotted into a journal:
/// `(guest path, journal path)`.
pub(super) const CONF_FILES: [(&str, &str); 2] = [
    ("/etc/wsl.conf", "wsl.conf"),
    ("/etc/wsl-distribution.conf", "wsl-distribution.conf"),
];

/// The journaled snapshot of a guest config file.
pub(super) fn journal_conf_path(name: &str, which: &str) -> Result<PathBuf> {
    Ok(journal_dir(name)?.join(which))
}

/// Copy `guest` out of the running container `id` with `container cp`
/// and return it, capped at 64 KiB. `Ok(None)` means the file doesn't
/// exist in the guest; other failures are errors for the caller to
/// warn about. `cp` is runtime-mediated — the file is read through the
/// container's rootfs without running any guest binary, so an image
/// can't control how its config is extracted (or feed us a `cat` it
/// ships).
fn cp_out(id: &str, guest: &str) -> Result<Option<Vec<u8>>> {
    let tmp = tempfile::tempdir().context("failed to create a snapshot scratch dir")?;
    let dst = tmp.path().join("conf");
    let out = container_cmd()
        .arg("cp")
        .arg(format!("{id}:{guest}"))
        .arg(&dst)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // vminitd reports ENOENT for a missing guest path — treat it
        // as "absent" but leave other failures (container stopped
        // between our check and the copy, a directory at the path)
        // for the caller.
        if stderr.contains("not found") || stderr.contains("No such file") {
            return Ok(None);
        }
        bail!("container cp {guest}: {}", stderr.trim());
    }
    let mut text = fs::read(&dst)?;
    text.truncate(64 * 1024);
    Ok(Some(text))
}

/// Refresh `name`'s guest-config snapshots from its running container
/// `id`. Called at boot (after the container starts) and before every
/// host-initiated stop — the only windows a `cp` can run, since it
/// needs a running container. A distro stopped by bare
/// `container stop` keeps its last snapshot — WSL's restart-to-apply
/// model makes that its effective config anyway. Skipped entirely
/// when the distro doesn't read its guest config. Missing guest files
/// clear any stale snapshot; an ambiguous `cp` failure keeps the old
/// one and warns rather than silently swapping in nothing.
pub(super) fn refresh_conf_snapshots(name: &str, id: &str, spec: &DistroSpec) {
    if spec.resolve().0.wsl_conf.is_empty() {
        return;
    }
    for (guest, which) in CONF_FILES {
        match cp_out(id, guest) {
            Ok(Some(text)) => {
                let p = journal_conf_path(name, which);
                let r = p.and_then(|p| {
                    with_journal_dir_unlocked(name, || {
                        unlock(&p);
                        write_nofollow(&p, &text, 0o644)?;
                        set_locked(&p, true)
                    })
                });
                if let Err(e) = r {
                    eprintln!(
                        "container-distro: warning: can't snapshot {guest} for `{name}`: {e:#}"
                    );
                }
            }
            Ok(None) => {
                if let Ok(p) = journal_conf_path(name, which)
                    && p.exists()
                {
                    let r = with_journal_dir_unlocked(name, || {
                        unlock(&p);
                        fs::remove_file(&p)
                            .with_context(|| format!("failed to remove {}", p.display()))
                    });
                    if let Err(e) = r {
                        eprintln!(
                            "container-distro: warning: can't clear `{name}`'s stale {which} snapshot: {e:#}"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("container-distro: warning: can't copy {guest} out of `{name}`: {e:#}")
            }
        }
    }
}

/// Write `name`'s config snapshots from text already extracted —
/// archive imports at create time, where the distro may never boot
/// (`--no-boot`). Entries that are `None` clear a stale file instead.
pub(super) fn write_conf_snapshots(
    name: &str,
    wsl_conf: Option<&str>,
    distribution_conf: Option<&str>,
) -> Result<()> {
    let texts = [wsl_conf, distribution_conf];
    for (text, (_, which)) in texts.iter().zip(CONF_FILES.iter()) {
        let p = journal_conf_path(name, which)?;
        with_journal_dir_unlocked(name, || {
            unlock(&p);
            match text {
                Some(t) => {
                    write_nofollow(&p, t.as_bytes(), 0o644)?;
                    set_locked(&p, true)
                }
                None => match fs::remove_file(&p) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    r => r.with_context(|| format!("failed to remove {}", p.display())),
                },
            }
        })?;
    }
    Ok(())
}

/// True when `conf` (the text of a `wsl.conf`, if any) declares
/// `[automount] enabled=false` — the only `wsl.conf` key the host
/// reads, and only ever to *reduce* sharing. Kept pure for tests.
pub(super) fn wsl_automount_disabled(conf: Option<&str>) -> bool {
    conf.and_then(|c| wsl::ini_value(c, "automount", "enabled"))
        .is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

/// Record the package's `automount.enabled=false` as *source* intent —
/// it caps `automount`/`home_mount` at `none` exactly the way the spec
/// model resolves everything else: explicit `--automount`/`--home-mount`
/// always win (they outrank source), preset-derived defaults like
/// `--trusted`'s rw home get lowered (they're defaults, not intent).
pub(super) fn lower_sharing(spec: &mut DistroSpec) {
    spec.source.automount = Some(Automount::None);
    spec.source.home_mount = Some(HomeMount::None);
}

/// Whether `name` has a verified journal: directory and spec exist and
/// carry `uchg`. A guest can't set the flag through a shared mount, so
/// an unsigned directory was planted, not written by us — it must not
/// count as a distro.
pub(super) fn journal_present(name: &str) -> bool {
    let Ok(dir) = journal_dir(name) else {
        return false;
    };
    is_locked(&dir) && is_locked(&dir.join("spec.json"))
}

/// Names of every journaled distro — stopped distros have no container,
/// so this is the only place they exist. Unsigned dirs are skipped: see
/// [`journal_present`]. Reads run the one-time migration first — it's
/// how pre-journal distros surface in `list`/`cm` on upgrade.
pub(super) fn journal_names() -> Result<Vec<String>> {
    migrate_once()?;
    let Ok(entries) = fs::read_dir(journal_root()?) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| journal_present(n))
        .collect())
}

/// Whether `name` is a distro — running, stopped-with-container, or
/// journaled.
pub(super) fn distro_exists(name: &str) -> Result<bool> {
    Ok(journal_present(name) || find(name).is_ok())
}

/// All distro names — journaled and container-backed — sorted. What
/// `cm` resolves names against.
pub fn distro_names() -> Result<Vec<String>> {
    let mut names: Vec<String> = journal_names()?;
    names.extend(
        distros()?
            .iter()
            .filter_map(|c| distro_name(c).map(String::from)),
    );
    names.sort();
    names.dedup();
    Ok(names)
}

/// Proof the per-distro `flock` is held. Only obtainable via
/// [`DistroLock::acquire`], so functions that must run under the lock
/// take `&DistroLock` — the compiler enforces it. The lock releases
/// when the wrapped descriptor closes (drop or process death), so an
/// interrupted operation can never leave a stale lock.
///
/// The lockfile *is* the journal's `spec.json`: the fd is opened
/// `O_RDWR|O_NOFOLLOW` while the `uchg` flag is cleared and the file is
/// re-locked immediately — the flag gates open/unlink/rename, not
/// writes on a live fd, so the same descriptor serializes operations
/// and rewrites the journal while it stays locked and unlinkable. A
/// guest could unlink a freestanding `locks/` file mid-operation and
/// silently void exclusion; it can't unlink this one.
pub(super) struct DistroLock {
    file: fs::File,
    name: String,
}

impl DistroLock {
    /// Serialize operations on `name`. The journal must already exist —
    /// [`ensure_journal`] or [`write_journal`] ran first.
    pub(super) fn acquire(name: &str) -> Result<Self> {
        let file = open_locked_rw(&journal_spec_path(name)?)
            .with_context(|| format!("no distro named `{name}`"))?;
        file.lock().context("failed to lock the distro journal")?;
        Ok(Self {
            file,
            name: name.to_string(),
        })
    }

    /// The journaled spec — the trusted desired configuration.
    pub(super) fn spec(&self) -> Result<DistroSpec> {
        read_journal(&self.name)
    }

    /// Rewrite the spec in place through the lock fd: the journal file
    /// stays `uchg`-locked — and the same inode, so the `flock` isn't
    /// disturbed — the whole time.
    pub(super) fn write_spec(&mut self, spec: &DistroSpec) -> Result<()> {
        let data = serde_json::to_vec_pretty(&spec.to_journal())?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&data)?;
        self.file.set_len(data.len() as u64)?;
        self.file.sync_all()?;
        Ok(())
    }
}

/// Whether another process holds `name`'s journal lock — a mutation is
/// in flight. Probe-only; `O_RDONLY` opens a locked journal fine.
pub(super) fn distro_busy(name: &str) -> bool {
    let Ok(path) = journal_spec_path(name) else {
        return false;
    };
    let Ok(f) = fs::File::open(path) else {
        return false;
    };
    matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::distro::spec::JournalSpec;

    use super::super::lock::{is_locked, open_locked_rw, set_locked, unlock};
    use super::super::test_support::spec;

    /// The journal lock contract: `spec.json` is `uchg`-locked at rest
    /// yet rewritable through the fd the flock is held on — verified
    /// behavior on macOS (the flag gates open/unlink/rename, not writes
    /// on a live descriptor).
    #[test]
    fn journal_lock_writes_through_uchg() {
        let dir = tempfile::tempdir().unwrap();
        let spec_p = dir.path().join("spec.json");
        fs::write(&spec_p, "{}").unwrap();
        set_locked(&spec_p, true).unwrap();
        assert!(is_locked(&spec_p));
        // The boundary a guest sees: no write, no unlink.
        assert!(fs::write(&spec_p, "tampered").is_err());
        assert!(fs::remove_file(&spec_p).is_err());
        assert!(fs::File::options().write(true).open(&spec_p).is_err());

        let spec = spec();
        let mut lock = DistroLock {
            file: open_locked_rw(&spec_p).unwrap(),
            name: "d1".into(),
        };
        lock.file.lock().unwrap();
        // The file is re-locked under the open fd, and a second locker
        // blocks — exclusion survives the unlock window.
        assert!(is_locked(&spec_p));
        assert!(matches!(
            fs::File::open(&spec_p).unwrap().try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        // Rewrites through the held fd are honored under `uchg`.
        lock.write_spec(&spec).unwrap();
        assert_eq!(
            serde_json::from_slice::<JournalSpec>(&fs::read(&spec_p).unwrap())
                .unwrap()
                .into_spec()
                .0,
            spec
        );
        drop(lock);
        // The flock releases with the fd — but a close-time release
        // isn't sequenced with all observable effects of close(2) (the
        // kernel can lag), so poll a fresh opener instead of asserting
        // on the first probe.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let probe = loop {
            match fs::File::open(&spec_p).unwrap().try_lock() {
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                res => break res,
            }
        };
        assert!(probe.is_ok(), "relock probe failed: {probe:?}");
        unlock(&spec_p);
    }
}
