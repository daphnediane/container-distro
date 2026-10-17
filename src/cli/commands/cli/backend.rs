/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! One WSL-style namespace over two backends: `container machine`s and
//! `container distro`s (via the `container_distro` library).
//!
//! A name that is a distro resolves to the distro; anything else is a
//! machine. With no name, the effective default wins — the record
//! converges to a real target (see `distro::default`), so removing a
//! default repoints it rather than leaving none. `--distro`/`--machine`
//! override the ordering — an explicit selector never falls through to
//! the other backend. `CM_BACKEND=machine` turns distros off entirely.

use std::env;

use crate::container::{self, Machine};
use crate::distro::{self, DistroSummary, EffectiveDefault};
use anyhow::{Result, bail};

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
/// name is a distro; `--distro` resolves a distro name or an effective
/// default that is a distro — a name that isn't a distro, or a default
/// that isn't one, is an error, not a fall through to a machine.
pub fn resolve_target(
    name: Option<&str>,
    distro_names: &[String],
    default: Option<EffectiveDefault>,
    sel: Backend,
) -> Result<Target> {
    if let Some(n) = name {
        return match sel {
            Backend::Machine => Ok(Target::Machine(Some(n.to_string()))),
            Backend::Distro if distro_names.iter().any(|d| d == n) => {
                Ok(Target::Distro(n.to_string()))
            }
            Backend::Distro => bail!("no distro named `{n}`"),
            Backend::Auto if distro_names.iter().any(|d| d == n) => {
                Ok(Target::Distro(n.to_string()))
            }
            Backend::Auto => Ok(Target::Machine(Some(n.to_string()))),
        };
    }
    match (sel, default) {
        (Backend::Machine, Some(EffectiveDefault::Distro(d))) => {
            bail!("the default is distro `{d}` — `--machine` needs a machine name")
        }
        (Backend::Machine, Some(EffectiveDefault::Machine(m)))
        | (Backend::Auto, Some(EffectiveDefault::Machine(m))) => Ok(Target::Machine(m)),
        (Backend::Distro, Some(EffectiveDefault::Machine(m))) => bail!(
            "the default is {} — `--distro` needs a distro name",
            m.map(|n| format!("machine `{n}`"))
                .unwrap_or_else(|| "a machine".into())
        ),
        (_, Some(EffectiveDefault::Distro(d))) => Ok(Target::Distro(d)),
        (Backend::Distro, None) => bail!("no default distro set — `cm -s NAME` picks one"),
        (_, None) => Ok(Target::Machine(None)),
    }
}

/// Resolve `name` against the live world. With no name, the effective
/// default — converged from the recorded one — is the answer.
pub fn resolve(name: Option<&str>, sel: Backend) -> Result<Target> {
    if !distros_enabled() {
        if sel == Backend::Distro {
            bail!("--distro needs distro support (unset CM_BACKEND=machine)");
        }
        return Ok(Target::Machine(name.map(str::to_string)));
    }
    if sel == Backend::Machine && name.is_none() {
        // `--machine` bare can still use our recorded machine pick.
        return match distro::converge_default()? {
            Some(EffectiveDefault::Distro(d)) => {
                bail!("the default is distro `{d}` — `--machine` needs a machine name")
            }
            Some(EffectiveDefault::Machine(m)) => Ok(Target::Machine(m)),
            None => Ok(Target::Machine(None)),
        };
    }
    let names = if sel == Backend::Machine {
        Vec::new()
    } else {
        distro::distro_names()?
    };
    let eff = if name.is_none() {
        distro::converge_default()?
    } else {
        None
    };
    let target = resolve_target(name, &names, eff, sel)?;
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
        default: Option<EffectiveDefault>,
    ) -> Result<Target> {
        resolve_target(name, distro_names, default, Backend::Auto)
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
    fn effective_default_drives_bare_resolution() {
        let d = names(&["d1"]);
        assert_eq!(
            resolve(None, &d, Some(EffectiveDefault::Distro("d1".into()))).unwrap(),
            Target::Distro("d1".into())
        );
        // A recorded machine pick targets by name; `machine:` defers
        // to `container`'s default.
        assert_eq!(
            resolve(None, &d, Some(EffectiveDefault::Machine(Some("m".into())))).unwrap(),
            Target::Machine(Some("m".into()))
        );
        assert_eq!(
            resolve(None, &d, Some(EffectiveDefault::Machine(None))).unwrap(),
            Target::Machine(None)
        );
        assert_eq!(resolve(None, &d, None).unwrap(), Target::Machine(None));
    }

    #[test]
    fn explicit_selectors_pin_the_backend() {
        let d = names(&["d1"]);
        // --machine: even a distro name resolves to the machine.
        assert_eq!(
            resolve_target(Some("d1"), &d, None, Backend::Machine).unwrap(),
            Target::Machine(Some("d1".into()))
        );
        // --distro: a distro name, or an effective distro default.
        assert_eq!(
            resolve_target(
                None,
                &d,
                Some(EffectiveDefault::Distro("d1".into())),
                Backend::Distro
            )
            .unwrap(),
            Target::Distro("d1".into())
        );
        // Selectors never fall through: a `--machine` default that is
        // a distro — or a `--distro` default that is a machine — is an
        // error, not a silent other-backend answer.
        assert!(
            resolve_target(
                None,
                &d,
                Some(EffectiveDefault::Machine(None)),
                Backend::Machine
            )
            .is_ok()
        );
        assert!(
            resolve_target(
                None,
                &d,
                Some(EffectiveDefault::Distro("d1".into())),
                Backend::Machine
            )
            .is_err()
        );
        assert!(
            resolve_target(
                None,
                &d,
                Some(EffectiveDefault::Machine(Some("m".into()))),
                Backend::Distro
            )
            .is_err()
        );
        assert!(resolve_target(Some("alpine"), &d, None, Backend::Distro).is_err());
        assert!(resolve_target(None, &d, None, Backend::Distro).is_err());
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
