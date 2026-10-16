/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Name clashes between distros, staged state, and machines.

use super::journal::{distro_exists, journal_dir, preserved_rootfs};
use crate::container::{self};
use anyhow::{Result, bail};

/// Refuse to create `name` while an unsigned journal dir or a legacy
/// `preserved/` pair sits at that name — the user must choose to keep
/// or discard it first.
pub(super) fn bail_if_staged(name: &str) -> Result<()> {
    if journal_dir(name).is_ok_and(|d| d.exists()) {
        bail!(
            "a journal directory for `{0}` exists but isn't a usable distro — \
             `container distro rm {0}` discards it",
            name
        );
    }
    if let Ok(staged) = preserved_rootfs(name)
        && staged.exists()
    {
        bail!(
            "an interrupted `set`/`migrate` left `{0}`'s filesystem staged — \
             `container distro rm {0}` discards it",
            name
        );
    }
    Ok(())
}

/// Whether a `container machine` named `name` exists.
fn machine_taken(name: &str) -> Result<bool> {
    Ok(container::list_machines()?.iter().any(|m| m.id == name))
}

/// `cm` resolves a name that exists as both a machine and a distro to
/// the distro — creating a distro on an existing machine name silently
/// shadows it, so the collision is refused unless `force` opts in
/// (and still warns: the shadow is real). Public for `catalog_ops`,
/// which checks a resolved name before staging an import.
pub fn bail_machine_shadow(name: &str, force: bool) -> Result<()> {
    if !machine_taken(name)? {
        return Ok(());
    }
    if !force {
        bail!(
            "a machine named `{name}` already exists — a same-named distro would \
             shadow it in `cm` (pick another name, or use --force)"
        );
    }
    eprintln!("container-distro: `{name}` is also a machine; `cm` resolves the name to the distro");
    Ok(())
}

/// The other direction: a distro hides a same-named machine from every
/// `cm` name lookup, so creating the machine is refused unless `force`
/// opts in (and still warns). Public for `cm`'s argv-built
/// `machine create` path, which never passes through `import_machine`.
pub fn bail_distro_shadow(name: &str, force: bool) -> Result<()> {
    if !distro_exists(name)? {
        return Ok(());
    }
    if !force {
        bail!(
            "a distro named `{name}` already exists — `cm` resolves that name to \
             the distro (pick another name, or use --force)"
        );
    }
    eprintln!("container-distro: `{name}` is also a distro; `cm` resolves the name to the distro");
    Ok(())
}
