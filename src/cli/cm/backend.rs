/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! One WSL-style namespace over two backends: `container machine`s and
//! `container distro`s (via the `container_distro` library).
//!
//! A name that is a distro resolves to the distro; anything else is a
//! machine. With no name, a default distro (if one is set) wins over the
//! default machine. `--distro`/`--machine` override the ordering —
//! an explicit selector never falls through to the other backend.
//! `CM_BACKEND=machine` turns distros off entirely.

use std::env;

use crate::container::{self, Machine};
use crate::distro::{self, DistroSummary};
use anyhow::{Context, Result, bail};

use super::args::Backend;

/// What a `-d NAME` (or the default) refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A machine; `None` means `container`'s default machine.
    Machine(Option<String>),
    Distro(String),
}

/// Whether distro support is enabled (`CM_BACKEND=machine` disables it).
pub fn distros_enabled() -> bool {
    env::var("CM_BACKEND").map_or(true, |b| b != "machine")
}

/// Pure resolution rule; see the module docs. An explicit selector
/// pins the backend: `--machine` resolves to a machine even when the
/// name is a distro; `--distro` resolves a distro name or the default
/// distro — and a name that isn't a distro is an error, not a fall
/// through to a machine.
pub fn resolve_target(
    name: Option<&str>,
    distro_names: &[String],
    distro_default: Option<&str>,
    sel: Backend,
) -> Result<Target> {
    match sel {
        Backend::Machine => Ok(Target::Machine(name.map(str::to_string))),
        Backend::Distro => {
            let d = match name {
                Some(n) => n,
                None => distro_default.context("no default distro set — `cm -s NAME` picks one")?,
            };
            if distro_names.iter().any(|x| x == d) {
                Ok(Target::Distro(d.to_string()))
            } else {
                bail!("no distro named `{d}`");
            }
        }
        Backend::Auto => Ok(match name {
            Some(n) if distro_names.iter().any(|d| d == n) => Target::Distro(n.to_string()),
            Some(n) => Target::Machine(Some(n.to_string())),
            None => match distro_default {
                Some(d) if distro_names.iter().any(|x| x == d) => Target::Distro(d.to_string()),
                _ => Target::Machine(None),
            },
        }),
    }
}

/// Resolve `name` against the live distro list. Costs one `container
/// list` only when a name is given or a distro default is set.
pub fn resolve(name: Option<&str>, sel: Backend) -> Result<Target> {
    if !distros_enabled() {
        if sel == Backend::Distro {
            bail!("--distro needs distro support (unset CM_BACKEND=machine)");
        }
        return Ok(Target::Machine(name.map(str::to_string)));
    }
    let default = distro::default_name();
    if sel != Backend::Distro && name.is_none() && default.is_none() {
        return Ok(Target::Machine(None));
    }
    let names = distro::distro_names()?;
    let target = resolve_target(name, &names, default.as_deref(), sel)?;
    // A distro silently shadows a machine of the same name — warn at
    // resolution time, not only in `-l` output. (An explicit `--distro`
    // made the choice already; `--machine` picks the machine.)
    if sel == Backend::Auto
        && let Target::Distro(d) = &target
        && container::list_machines()
            .map(|ms| ms.iter().any(|m| &m.id == d))
            .unwrap_or(false)
    {
        eprintln!("cm: `{d}` is both a machine and a distro; using the distro");
    }
    Ok(target)
}

/// All distros, or none when distro support is off.
pub fn distro_summaries(running_only: bool) -> Result<Vec<DistroSummary>> {
    if distros_enabled() {
        distro::summaries(running_only)
    } else {
        Ok(Vec::new())
    }
}

/// Names that exist as both a machine and a distro (the distro wins).
pub fn clashes(machines: &[Machine], distros: &[DistroSummary]) -> Vec<String> {
    distros
        .iter()
        .filter(|d| machines.iter().any(|m| m.id == d.id))
        .map(|d| d.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn resolve(
        name: Option<&str>,
        distro_names: &[String],
        distro_default: Option<&str>,
    ) -> Result<Target> {
        resolve_target(name, distro_names, distro_default, Backend::Auto)
    }

    #[test]
    fn named_distro_wins() {
        let d = names(&["d1"]);
        assert_eq!(
            resolve(Some("d1"), &d, None).unwrap(),
            Target::Distro("d1".into())
        );
        assert_eq!(
            resolve(Some("alpine"), &d, None).unwrap(),
            Target::Machine(Some("alpine".into()))
        );
    }

    #[test]
    fn default_distro_over_default_machine() {
        let d = names(&["d1"]);
        assert_eq!(
            resolve(None, &d, Some("d1")).unwrap(),
            Target::Distro("d1".into())
        );
        assert_eq!(resolve(None, &d, None).unwrap(), Target::Machine(None));
        // A stale default (distro deleted) falls back to the machine.
        assert_eq!(
            resolve(None, &d, Some("gone")).unwrap(),
            Target::Machine(None)
        );
    }

    #[test]
    fn explicit_selectors_pin_the_backend() {
        let d = names(&["d1"]);
        // --machine: even a distro name resolves to the machine.
        assert_eq!(
            resolve_target(Some("d1"), &d, Some("d1"), Backend::Machine).unwrap(),
            Target::Machine(Some("d1".into()))
        );
        assert_eq!(
            resolve_target(None, &d, Some("d1"), Backend::Machine).unwrap(),
            Target::Machine(None)
        );
        // --distro: a distro name, or the default distro bare.
        assert_eq!(
            resolve_target(Some("d1"), &d, None, Backend::Distro).unwrap(),
            Target::Distro("d1".into())
        );
        assert_eq!(
            resolve_target(None, &d, Some("d1"), Backend::Distro).unwrap(),
            Target::Distro("d1".into())
        );
        // --distro never falls through: unknown names and a missing or
        // stale default are errors, not a machine.
        assert!(resolve_target(Some("alpine"), &d, None, Backend::Distro).is_err());
        assert!(resolve_target(None, &d, None, Backend::Distro).is_err());
        assert!(resolve_target(None, &d, Some("gone"), Backend::Distro).is_err());
    }

    #[test]
    fn clash_detection() {
        let m: Machine = serde_json::from_str(r#"{"id":"x","status":"running"}"#).unwrap();
        let d = DistroSummary {
            id: "x".into(),
            ..DistroSummary::default()
        };
        assert_eq!(clashes(&[m], &[d]), ["x"]);
    }
}
