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
//! default machine. `CM_BACKEND=machine` turns distros off entirely.

use std::env;

use anyhow::Result;
use cm_core::container::{self, Machine};
use container_distro::ops::{self as distro, DistroSummary};

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

/// Pure resolution rule; see the module docs.
pub fn resolve_target(
    name: Option<&str>,
    distro_names: &[String],
    distro_default: Option<&str>,
) -> Target {
    match name {
        Some(n) if distro_names.iter().any(|d| d == n) => Target::Distro(n.to_string()),
        Some(n) => Target::Machine(Some(n.to_string())),
        None => match distro_default {
            Some(d) if distro_names.iter().any(|x| x == d) => Target::Distro(d.to_string()),
            _ => Target::Machine(None),
        },
    }
}

/// Resolve `name` against the live distro list. Costs one `container
/// list` only when a name is given or a distro default is set.
pub fn resolve(name: Option<&str>) -> Result<Target> {
    if !distros_enabled() {
        return Ok(Target::Machine(name.map(str::to_string)));
    }
    let default = distro::default_name();
    if name.is_none() && default.is_none() {
        return Ok(Target::Machine(None));
    }
    let names: Vec<String> = distro::distros()?
        .iter()
        .map(|c| c.id().to_string())
        .collect();
    let target = resolve_target(name, &names, default.as_deref());
    // A distro silently shadows a machine of the same name — warn at
    // resolution time, not only in `-l` output.
    if let Target::Distro(d) = &target
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

    #[test]
    fn named_distro_wins() {
        let d = names(&["d1"]);
        assert_eq!(
            resolve_target(Some("d1"), &d, None),
            Target::Distro("d1".into())
        );
        assert_eq!(
            resolve_target(Some("alpine"), &d, None),
            Target::Machine(Some("alpine".into()))
        );
    }

    #[test]
    fn default_distro_over_default_machine() {
        let d = names(&["d1"]);
        assert_eq!(
            resolve_target(None, &d, Some("d1")),
            Target::Distro("d1".into())
        );
        assert_eq!(resolve_target(None, &d, None), Target::Machine(None));
        // A stale default (distro deleted) falls back to the machine.
        assert_eq!(
            resolve_target(None, &d, Some("gone")),
            Target::Machine(None)
        );
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
