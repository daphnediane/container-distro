/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `sync-profiles` and opening a distro in Terminal.

use super::adopt::{ensure_journal, ensure_state};
use super::journal::{DistroLock, journal_names, read_journal};
use crate::distro::terminal;
use anyhow::Result;

/// `container distro sync-profiles`: converge every journaled distro's
/// terminal-profile artifacts to its recorded intent — the upgrade/
/// repair path for launchers never generated (created before the
/// feature existed) or lost — and sweep `.terminal` files and
/// imported Terminal profiles whose distro is gone.
pub fn sync_profiles() -> Result<()> {
    ensure_state()?;
    let mut names = std::collections::BTreeSet::new();
    for name in journal_names()? {
        names.insert(name.clone());
        match read_journal(&name) {
            Ok(spec) => {
                let enabled = spec.resolve().0.terminal_profile;
                terminal::sync(&name, enabled);
                if enabled {
                    let _ = terminal::ensure_profile(&name);
                }
            }
            Err(e) => {
                eprintln!("container-distro: couldn't read `{name}`'s journal: {e:#}");
            }
        }
    }
    terminal::reconcile(&names);
    Ok(())
}

/// `container distro terminal` / `cm --terminal`: open the distro in
/// a Terminal.app window via its `.terminal` launcher — `open`
/// imports the profile and launches it. Asking to open is an explicit
/// opt-in, so `terminal_profile` is pinned like `set
/// --terminal-profile` would: a later `sync-profiles` converges the
/// artifacts rather than sweeping a profile the user just opened.
pub fn open_terminal(name: &str) -> Result<()> {
    ensure_state()?;
    ensure_journal(name)?;
    let mut lock = DistroLock::acquire(name)?;
    let mut spec = lock.spec()?;
    if spec.explicit.terminal_profile != Some(true) {
        spec.explicit.terminal_profile = Some(true);
        lock.write_spec(&spec)?;
        terminal::sync(name, spec.resolve().0.terminal_profile);
    }
    drop(lock);
    terminal::open_terminal(name)
}
