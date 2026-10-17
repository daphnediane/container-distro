/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Lifecycle actions: `-s`, `-t`, `--unregister`, `--migrate`,
//! `--shutdown`, `--rename`.

use std::process::ExitCode;

use anyhow::{Result, bail};

use super::args::Backend;
use super::backend::{self, Target};
use super::{named_target, passthru, target};
use crate::container;
use crate::distro::{self, CreateOptions};

/// WSL `-s`: a distro becomes the recorded default; a machine becomes
/// `container`'s default machine *and* our recorded `Machine` — the
/// one pick that is allowed to touch `container`'s default.
pub(super) fn set_default(name: &str, sel: Backend) -> Result<ExitCode> {
    match named_target(name, sel)? {
        Target::Distro(d) => {
            distro::set_default(Some(&d))?;
            Ok(ExitCode::SUCCESS)
        }
        Target::Machine(_) => {
            let code = passthru(&["machine", "set-default", name])?;
            if code == ExitCode::SUCCESS && backend::distros_enabled() {
                distro::record_machine_default(name);
            }
            Ok(code)
        }
    }
}

/// WSL `-t`, plus our convenience: no name stops the effective default.
pub(super) fn terminate(name: Option<&str>, sel: Backend) -> Result<ExitCode> {
    match target(name, sel)? {
        Target::Distro(d) => {
            distro::stop(&[d])?;
            Ok(ExitCode::SUCCESS)
        }
        Target::Machine(m) => {
            let mut args = vec!["machine", "stop"];
            if let Some(m) = m.as_deref() {
                args.push(m);
            }
            passthru(&args)
        }
    }
}

pub(super) fn unregister(name: &str, sel: Backend) -> Result<ExitCode> {
    match named_target(name, sel)? {
        Target::Distro(d) => {
            // `delete` converges the default record itself.
            distro::delete(true, &[d])?;
            Ok(ExitCode::SUCCESS)
        }
        Target::Machine(_) => {
            let rc = passthru(&["machine", "rm", name])?;
            if rc == ExitCode::SUCCESS {
                // `machine rm` doesn't know about the `local/machine-*`
                // tags `import` added — release them.
                distro::release_machine_images(name);
                // A removed machine may have been the recorded or the
                // machine default — converge repoints it.
                if backend::distros_enabled()
                    && let Err(e) = distro::converge_default()
                {
                    eprintln!("cm: couldn't update the default: {e:#}");
                }
            }
            Ok(rc)
        }
    }
}

/// `cm --migrate NAME`: converge NAME to the selected backend — a
/// machine migrates in, a pre-journal distro is adopted, a journaled
/// distro is already home. With `--machine`, a distro migrates out.
pub(super) fn migrate(name: &str, sel: Backend) -> Result<ExitCode> {
    container::validate_name(name)?;
    if sel != Backend::Machine && !backend::distros_enabled() {
        bail!("distros are disabled (CM_BACKEND=machine) — nothing to migrate to");
    }
    let opts = CreateOptions::default();
    let msg = match sel {
        Backend::Machine => distro::migrate_to_machine(name, None, false, &opts)?,
        _ => distro::migrate(name, None, false, &opts)?,
    };
    println!("{msg}");
    Ok(ExitCode::SUCCESS)
}

/// WSL `--shutdown`: stop every running machine and distro. Services
/// stop too when `system` is set, or when our tooling started them and
/// the post-stop world is completely idle — "stop what we started"
/// (#11), since a bare `container system stop` also tears down
/// unrelated containers.
pub(super) fn shutdown(system: bool) -> Result<ExitCode> {
    if !container::system_running() {
        // Services are already down; any "we started them" marker
        // left behind is stale.
        container::clear_services_marker();
        return Ok(ExitCode::SUCCESS);
    }
    let mut code = ExitCode::SUCCESS;
    for m in container::list_machines()?
        .iter()
        .filter(|m| m.is_running())
    {
        let c = passthru(&["machine", "stop", &m.id])?;
        if c != ExitCode::SUCCESS {
            code = c;
        }
    }
    let running: Vec<String> = backend::distro_summaries(true)?
        .into_iter()
        .map(|d| d.id)
        .collect();
    if !running.is_empty()
        && let Err(e) = distro::stop(&running)
    {
        eprintln!("cm: {e:#}");
        code = ExitCode::FAILURE;
    }
    // `is_ok_and` keeps this conservative: when the running-state
    // probe fails we can't prove idleness, so the services stay up.
    let ours_and_idle = container::services_started_by_us()
        && container::list_machines().is_ok_and(|ms| !ms.iter().any(|m| m.is_running()))
        && container::list_containers().is_ok_and(|cs| !cs.iter().any(|c| c.is_running()));
    if ours_and_idle && !system {
        eprintln!("cm: stopping container services (started by cm; nothing left running)");
    }
    if system || ours_and_idle {
        let c = passthru(&["system", "stop"])?;
        if c != ExitCode::SUCCESS {
            code = c;
        } else {
            container::clear_services_marker();
        }
    }
    Ok(code)
}

/// `cm --rename OLD NEW`: a distro's journal — spec, disk, assets —
/// moves wholesale; a machine is recreated under the new name with its
/// filesystem carried across (`container machine` has no rename).
pub(super) fn rename(old: &str, new: &str, sel: Backend, force: bool) -> Result<ExitCode> {
    container::validate_name(old)?;
    match named_target(old, sel)? {
        Target::Distro(d) => distro::rename(&d, new, force)?,
        Target::Machine(_) => distro::rename_machine(old, new, force)?,
    }
    println!("`{old}` renamed to `{new}`");
    Ok(ExitCode::SUCCESS)
}
