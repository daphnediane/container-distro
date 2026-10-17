/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The recorded default and its convergence into an effective target.
//!
//! `default-distro` holds a `DefaultType`: a bare distro name, a
//! `machine:<name>` we picked, or `machine:` — "whatever `container`
//! calls the default machine". The indirect `machine:` form is the
//! only lazy one; a live distro always keeps a concrete `Distro` or
//! `Machine` record (WSL repoints the default rather than leaving
//! none). Reads *converge*: a drifted record is repaired to point at
//! something real, so `--unregister`, deletes, and out-of-band
//! `machine set-default` changes self-heal.

use super::journal::{distro_exists, journal_names};
use super::lock::{open_locked_rw, set_locked, unlock, write_nofollow};
use crate::container::{Machine, ensure_started, list_machines};
use crate::naming::state_dir;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

fn default_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("default-distro"))
}

/// What's recorded in `default-distro`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultType {
    /// An explicit distro default (`cm -s`, `container distro
    /// set-default`, an auto-claimed create).
    Distro(String),
    /// A machine we picked as effective default without touching
    /// `container`'s own default — only `cm -s <machine>` sets the
    /// machine default itself.
    Machine(String),
    /// `machine:` — follow `container`'s default machine. `container`
    /// clears its default when the default machine is deleted rather
    /// than repointing, so this form doesn't imply one exists.
    MachineDefault,
}

/// The effective default a caller should act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveDefault {
    Distro(String),
    /// `Some(name)` — target that machine by name (it may not be
    /// `container`'s default); `None` — defer to `container`'s default
    /// machine.
    Machine(Option<String>),
}

/// A distro's fitness for the effective-default pick.
#[derive(Debug, Clone)]
pub struct DistroState {
    pub name: String,
    pub running: bool,
    /// `false` for staging leftovers an interrupted `create`/`import`
    /// left behind — they aren't real distros.
    pub selectable: bool,
}

impl From<&super::list::DistroSummary> for DistroState {
    fn from(s: &super::list::DistroSummary) -> Self {
        DistroState {
            name: s.id.clone(),
            running: s.is_running(),
            // "interrupted" rows are staging leftovers, not distros.
            selectable: s.status != "interrupted",
        }
    }
}

/// Every journaled distro's default-worthiness, journal being the
/// source of truth: running if it has a live container. Orphaned
/// containers aren't candidates.
pub(crate) fn distro_states() -> Result<Vec<DistroState>> {
    let running: HashSet<String> = super::distros()?
        .iter()
        .filter(|c| c.is_running())
        .filter_map(|c| super::distro_name(c).map(str::to_string))
        .collect();
    let mut states: Vec<DistroState> = journal_names()?
        .into_iter()
        .map(|name| DistroState {
            running: running.contains(&name),
            selectable: true,
            name,
        })
        .collect();
    states.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(states)
}

fn parse_default(s: &str) -> Option<DefaultType> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(match s.strip_prefix("machine:") {
        Some("") => DefaultType::MachineDefault,
        Some(name) => DefaultType::Machine(name.to_string()),
        None => DefaultType::Distro(s.to_string()),
    })
}

/// The recorded default, unrepaired — an unlocked probe for cheap
/// checks (`cm -l`, rename guards). `None` (absent or empty file)
/// means the same thing as `MachineDefault` — follow `container`.
pub fn recorded_default() -> Option<DefaultType> {
    let s = fs::read_to_string(default_file().ok()?).ok()?;
    parse_default(&s)
}

fn serialize_default(d: &DefaultType) -> String {
    match d {
        // A bare name is a distro (backwards compatible); the
        // `machine:` prefix can't collide — `:` is invalid in names.
        DefaultType::Distro(n) => n.clone(),
        DefaultType::Machine(n) => format!("machine:{n}"),
        DefaultType::MachineDefault => "machine:".to_string(),
    }
}

/// The flock'd marker descriptor — the lockfile *is* the marker, like
/// the journal's `DistroLock`: opened `O_RDWR|O_NOFOLLOW` while `uchg`
/// is cleared and re-locked, so the file can't be swapped or unlinked
/// by a guest mid-operation, and the fd both serializes the
/// read→converge→write sequence and carries the rewrite. A freestanding
/// `default.lock` wouldn't do: a guest could unlink it and void the
/// exclusion.
struct DefaultLock {
    file: fs::File,
}

fn acquire_default() -> Result<DefaultLock> {
    let path = default_file()?;
    fs::create_dir_all(state_dir()?)?;
    if fs::symlink_metadata(&path).is_err() {
        // First creation needs no flock — nobody can hold a lock on an
        // inode that didn't exist, and every creator writes the same
        // `machine:` (the indirect record an absent file already
        // meant), so racing creators can't tear.
        write_raw(&path, &DefaultType::MachineDefault)?;
    }
    let file = open_locked_rw(&path)?;
    file.lock().context("failed to lock the default marker")?;
    Ok(DefaultLock { file })
}

fn write_raw(path: &std::path::Path, d: &DefaultType) -> Result<()> {
    // Locked at rest like the init assets: this file picks which
    // distro a bare `cm` enters, so it shouldn't be guest-writable.
    unlock(path);
    write_nofollow(
        path,
        format!("{}\n", serialize_default(d)).as_bytes(),
        0o644,
    )?;
    set_locked(path, true)?;
    Ok(())
}

impl DefaultLock {
    fn recorded(&mut self) -> Result<Option<DefaultType>> {
        let mut s = String::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_string(&mut s)?;
        Ok(parse_default(&s))
    }

    /// Rewrite through the lock fd: the file stays `uchg`-locked — and
    /// the same inode, so the `flock` isn't disturbed — the whole time.
    fn write(&mut self, d: &DefaultType) -> Result<()> {
        let s = format!("{}\n", serialize_default(d));
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(s.as_bytes())?;
        self.file.set_len(s.len() as u64)?;
        self.file.sync_all()?;
        Ok(())
    }
}

pub(super) fn write_default(d: &DefaultType) -> Result<()> {
    acquire_default()?.write(d)
}

/// The best remaining distro for a repoint: running before stopped,
/// name order as the tiebreak (the sort `container distro list` shows).
fn best_distro(distros: &[DistroState]) -> Option<String> {
    distros
        .iter()
        .filter(|d| d.selectable)
        .min_by(|a, b| (!a.running, &a.name).cmp(&(!b.running, &b.name)))
        .map(|d| d.name.clone())
}

/// The pure converge rule: what the recorded default should become
/// (a repair write, `None` when it's already truthful) and what the
/// caller should act on. The `machine:` tier — the one place laziness
/// is allowed, and only while no distro forces a concrete pick —
/// follows `container`'s own default, else repoints to the best
/// distro, else lazily picks the best machine into `Machine(name)`
/// *without* `machine set-default`: nothing explicitly told `container`
/// to make it the default.
fn converge_record(
    recorded: Option<DefaultType>,
    distros: &[DistroState],
    machines: &[Machine],
) -> (Option<DefaultType>, Option<EffectiveDefault>) {
    match recorded {
        Some(DefaultType::Distro(n)) => {
            if distros.iter().any(|d| d.name == n && d.selectable) {
                return (None, Some(EffectiveDefault::Distro(n)));
            }
            // The recorded distro is gone: another distro takes over,
            // or the default becomes `machine:` when none are left.
            if let Some(next) = best_distro(distros) {
                return (
                    Some(DefaultType::Distro(next.clone())),
                    Some(EffectiveDefault::Distro(next)),
                );
            }
            converge_as_machine_default(distros, machines)
        }
        Some(DefaultType::Machine(n)) => {
            let exists = machines.iter().any(|m| m.id == n);
            // A `container` default machine pointing elsewhere means
            // the default was moved outside `cm` — our pick is stale
            // and `machine:` follows the real one.
            let overridden = machines.iter().any(|m| m.default && m.id != n);
            if exists && !overridden {
                return (None, Some(EffectiveDefault::Machine(Some(n))));
            }
            converge_as_machine_default(distros, machines)
        }
        Some(DefaultType::MachineDefault) | None => machine_tier(distros, machines),
    }
}

/// The `machine:` tier shared by an absent/`machine:` record and a
/// stale `Distro`/`Machine` one: a stale record converges to at least
/// `machine:`, while an already-indirect record writes only the
/// concrete pick it lands on.
fn converge_as_machine_default(
    distros: &[DistroState],
    machines: &[Machine],
) -> (Option<DefaultType>, Option<EffectiveDefault>) {
    let (w, e) = machine_tier(distros, machines);
    (w.or(Some(DefaultType::MachineDefault)), e)
}

/// `container`'s own default machine wins; a remaining distro is a
/// better default than an arbitrary machine; as a last resort record
/// `Machine(name)` for the best machine — *without* `machine
/// set-default`, since nothing explicitly told `container` to make it
/// the default.
fn machine_tier(
    distros: &[DistroState],
    machines: &[Machine],
) -> (Option<DefaultType>, Option<EffectiveDefault>) {
    if machines.iter().any(|m| m.default) {
        return (None, Some(EffectiveDefault::Machine(None)));
    }
    if let Some(d) = best_distro(distros) {
        return (
            Some(DefaultType::Distro(d.clone())),
            Some(EffectiveDefault::Distro(d)),
        );
    }
    let pick = machines
        .iter()
        .min_by(|a, b| (!a.is_running(), &a.id).cmp(&(!b.is_running(), &b.id)));
    match pick {
        Some(m) => (
            Some(DefaultType::Machine(m.id.clone())),
            Some(EffectiveDefault::Machine(Some(m.id.clone()))),
        ),
        None => (None, None),
    }
}

/// Reconcile the recorded default against what exists — repairing the
/// marker when it has drifted — and return the effective target. The
/// marker flock serializes the whole read→repair→write across `cm`
/// and `container distro` processes.
pub fn effective_default(
    distros: &[DistroState],
    machines: &[Machine],
) -> Result<Option<EffectiveDefault>> {
    let mut lock = acquire_default()?;
    let (write, eff) = converge_record(lock.recorded()?, distros, machines);
    if let Some(w) = write {
        lock.write(&w)?;
    }
    Ok(eff)
}

/// `effective_default` over the live world — gathers distro states and
/// machines itself. Mutates the marker (and nothing else) when the
/// record has drifted.
pub fn converge_default() -> Result<Option<EffectiveDefault>> {
    effective_default(&distro_states()?, &list_machines().unwrap_or_default())
}

/// Whether nothing but `excluded` can be the effective default —
/// `create`/`migrate` claim the default slot only then (first thing in
/// the world, like `wsl --install` making the first distro default).
pub(super) fn no_effective_default(excluded: &str) -> Result<bool> {
    let distros: Vec<DistroState> = distro_states()?
        .into_iter()
        .filter(|d| d.name != excluded)
        .collect();
    let machines: Vec<Machine> = list_machines()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.id != excluded)
        .collect();
    Ok(effective_default(&distros, &machines)?.is_none())
}

/// `cm -s <machine>` records `machine:<name>` — the pick sits beside
/// `container`'s own default (the caller just ran `machine
/// set-default`). A record failure doesn't fail the `-s` that worked.
pub fn record_machine_default(name: &str) {
    if let Err(e) = write_default(&DefaultType::Machine(name.to_string())) {
        eprintln!("container-distro: couldn't record the default: {e:#}");
    }
}

pub fn set_default(name: Option<&str>) -> Result<()> {
    match name {
        Some(n) => {
            ensure_started()?;
            if !distro_exists(n)? {
                bail!("no distro named `{n}`");
            }
            write_default(&DefaultType::Distro(n.to_string()))
        }
        // --clear / an unset goes indirect: follow `container`.
        None => write_default(&DefaultType::MachineDefault),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::distro::spec::{Automount, HomeMount, Preset};

    use super::super::create::CreateOptions;
    use super::super::machine::reject_machine_opts;

    fn distro(name: &str, running: bool) -> DistroState {
        DistroState {
            name: name.to_string(),
            running,
            selectable: true,
        }
    }

    fn machine(id: &str, default: bool) -> Machine {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","status":"running","default":{default}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn parses_all_record_forms() {
        assert_eq!(
            parse_default("alpine"),
            Some(DefaultType::Distro("alpine".into()))
        );
        assert_eq!(
            parse_default("machine:m1"),
            Some(DefaultType::Machine("m1".into()))
        );
        assert_eq!(parse_default("machine:"), Some(DefaultType::MachineDefault));
        assert_eq!(parse_default(""), None);
    }

    #[test]
    fn best_distro_prefers_running_then_name() {
        let distros = [distro("b", false), distro("a", false), distro("z", true)];
        assert_eq!(best_distro(&distros), Some("z".to_string()));
        let stopped = [distro("b", false), distro("a", false)];
        assert_eq!(best_distro(&stopped), Some("a".to_string()));
        let mut unselectable = distro("s", false);
        unselectable.selectable = false;
        assert_eq!(best_distro(&[unselectable]), None);
    }

    fn converge(
        recorded: Option<DefaultType>,
        distros: &[DistroState],
        machines: &[Machine],
    ) -> (Option<DefaultType>, Option<EffectiveDefault>) {
        converge_record(recorded, distros, machines)
    }

    #[test]
    fn live_records_stand() {
        let d = [distro("d1", false)];
        assert_eq!(
            converge(Some(DefaultType::Distro("d1".into())), &d, &[]),
            (None, Some(EffectiveDefault::Distro("d1".into())))
        );
        // A machine pick stands while the machine exists and no other
        // machine claimed `container`'s default.
        let m = [machine("m", false)];
        assert_eq!(
            converge(Some(DefaultType::Machine("m".into())), &d, &m),
            (None, Some(EffectiveDefault::Machine(Some("m".into()))))
        );
        // `machine:` follows `container`'s own default machine.
        let m = [machine("m", true)];
        assert_eq!(
            converge(Some(DefaultType::MachineDefault), &d, &m),
            (None, Some(EffectiveDefault::Machine(None)))
        );
    }

    #[test]
    fn removed_default_repoints() {
        // `gone` was deleted — the `*` moves to the best remaining
        // distro, like WSL.
        let d = [distro("next", false)];
        assert_eq!(
            converge(Some(DefaultType::Distro("gone".into())), &d, &[]),
            (
                Some(DefaultType::Distro("next".into())),
                Some(EffectiveDefault::Distro("next".into()))
            )
        );
        // With no distros left the record goes indirect — laziness is
        // only allowed when nothing concrete remains.
        assert_eq!(
            converge(
                Some(DefaultType::Distro("gone".into())),
                &[],
                &[machine("m", true)]
            ),
            (
                Some(DefaultType::MachineDefault),
                Some(EffectiveDefault::Machine(None))
            )
        );
        // A picked machine that no longer exists also converges —
        // following `container`'s default when it has one.
        assert_eq!(
            converge(
                Some(DefaultType::Machine("gone".into())),
                &[distro("d", false)],
                &[machine("m", true)]
            ),
            (
                Some(DefaultType::MachineDefault),
                Some(EffectiveDefault::Machine(None))
            )
        );
        // And when `container` has no default either, our pick moves
        // to the best machine — recorded, not `machine set-default`'d.
        assert_eq!(
            converge(
                Some(DefaultType::Machine("gone".into())),
                &[],
                &[machine("m2", false), machine("m1", false)]
            ),
            (
                Some(DefaultType::Machine("m1".into())),
                Some(EffectiveDefault::Machine(Some("m1".into())))
            )
        );
    }

    #[test]
    fn machine_default_moved_outside_cm_follows() {
        // `container machine set-default` outside `cm` overrode our
        // pick — `machine:` defers to it.
        let machines = [machine("picked", false), machine("other", true)];
        assert_eq!(
            converge(Some(DefaultType::Machine("picked".into())), &[], &machines),
            (
                Some(DefaultType::MachineDefault),
                Some(EffectiveDefault::Machine(None))
            )
        );
    }

    #[test]
    fn indirect_record_converges() {
        // Nothing recorded + no machine default + distros present →
        // the best distro becomes the recorded default.
        let d = [distro("d1", false)];
        assert_eq!(
            converge(None, &d, &[machine("m", false)]),
            (
                Some(DefaultType::Distro("d1".into())),
                Some(EffectiveDefault::Distro("d1".into()))
            )
        );
        // Empty world: no repair, no default.
        assert_eq!(converge(None, &[], &[]), (None, None));
        // Machines without a default and no distros: lazily pick one.
        assert_eq!(
            converge(
                Some(DefaultType::MachineDefault),
                &[],
                &[machine("m", false)]
            ),
            (
                Some(DefaultType::Machine("m".into())),
                Some(EffectiveDefault::Machine(Some("m".into())))
            )
        );
    }

    #[test]
    fn machine_opts_rejection() {
        // The set-style subset a machine can honor passes — resources,
        // home-mount, `--ssh`, and the tier flags.
        assert!(
            reject_machine_opts(&CreateOptions {
                cpus: Some(2),
                memory: Some("1G".into()),
                home_mount: Some(HomeMount::Ro),
                ssh: true,
                preset: Some(Preset::Restricted),
                no_boot: true,
                set_default: true,
                ..CreateOptions::default()
            })
            .is_ok()
        );
        // Options a machine can't honor are refused, named in the
        // error — including `--no-ssh`, a pin no machine can keep.
        let err = reject_machine_opts(&CreateOptions {
            volumes: vec!["/a:/b".parse().unwrap()],
            publish: vec!["8080".parse().unwrap()],
            network: Some("none".into()),
            no_ssh: true,
            sudo: true,
            ..CreateOptions::default()
        })
        .unwrap_err()
        .to_string();
        for f in [
            "--volume",
            "--publish",
            "--network",
            "--no-ssh",
            "--sudo/--no-sudo",
        ] {
            assert!(err.contains(f), "{err}");
        }
        // --automount (even bare) is refused.
        assert!(
            reject_machine_opts(&CreateOptions {
                automount: Some(Automount::Rw),
                ..CreateOptions::default()
            })
            .is_err()
        );
    }
}
