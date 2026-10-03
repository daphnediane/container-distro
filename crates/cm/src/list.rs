/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! WSL-shaped `--list` output.
//!
//! - `cm -l` — a header plus one name per line, `(Default)` marked
//! - `cm -l -v` — the `NAME STATE VERSION` table, laid out exactly like
//!   `wsl.exe -l -v` (editor extensions regex-match it)
//! - `cm -l -v -v` — the same table with extra columns appended

use cm_core::container::{Machine, MachineDetail};
use cm_core::table::{columns, human_bytes, title_case};
use container_distro::ops::DistroSummary;

/// Header printed by `cm -l` (WSL: "Windows Subsystem for Linux Distributions:").
pub const LIST_HEADER: &str = "Container Machines:";

/// One row of list output, independent of the backend that produced it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: String,
    pub status: String,
    pub default: bool,
    pub kind: &'static str,
    pub ip: Option<String>,
    pub cpus: Option<u64>,
    pub memory: Option<u64>,
    pub disk: Option<u64>,
    pub platform: Option<String>,
    pub home: Option<String>,
    pub image: Option<String>,
}

impl Entry {
    pub fn from_machine(m: &Machine, detail: Option<&MachineDetail>) -> Self {
        Entry {
            name: m.id.clone(),
            status: m.status.clone(),
            default: m.default,
            kind: "machine",
            ip: m.ip_address.clone(),
            cpus: m.cpus,
            memory: m.memory,
            disk: m.disk_size,
            platform: detail
                .and_then(|d| d.platform.as_ref())
                .map(|p| p.to_string()),
            home: detail.and_then(|d| d.home_mount.clone()),
            image: detail
                .and_then(|d| d.image.as_ref())
                .map(|i| i.reference.clone()),
        }
    }

    pub fn from_distro(d: &DistroSummary) -> Self {
        Entry {
            name: d.id.clone(),
            status: d.status.clone(),
            default: d.default,
            kind: "distro",
            ip: d.ip_address.clone(),
            cpus: d.cpus,
            memory: d.memory,
            disk: d.disk_size,
            platform: d.platform.clone(),
            home: d.home_mount.clone(),
            image: d.image.clone(),
        }
    }
}

/// Merge machines and distros into one list. A distro shadows a machine
/// of the same name, and a default distro takes the default marker from
/// the default machine (matching [`crate::backend::resolve`]).
pub fn merge(mut machines: Vec<Entry>, distros: Vec<Entry>) -> Vec<Entry> {
    machines.retain(|m| !distros.iter().any(|d| d.name == m.name));
    if distros.iter().any(|d| d.default) {
        machines.iter_mut().for_each(|m| m.default = false);
    }
    machines.extend(distros);
    machines
}

/// `cm -l`: header, then names with the default marked.
pub fn format_simple(entries: &[Entry]) -> String {
    let mut out = format!("{LIST_HEADER}\n");
    for e in entries {
        out.push_str(&e.name);
        if e.default {
            out.push_str(" (Default)");
        }
        out.push('\n');
    }
    out
}

/// `cm -l -q`: bare names.
pub fn format_quiet(entries: &[Entry]) -> String {
    entries.iter().map(|e| format!("{}\n", e.name)).collect()
}

/// WSL reports 1 or 2; a container machine is a full VM, like WSL 2.
const VERSION: &str = "2";

/// `cm -l -v` (`verbosity == 1`) or `-l -v -v` (`verbosity >= 2`).
///
/// The leading `NAME STATE VERSION` columns replicate `wsl.exe`: NAME is
/// padded to the longest name + 4, STATE to 16.
pub fn format_verbose(entries: &[Entry], verbosity: u8) -> String {
    let name_w = entries
        .iter()
        .map(|e| e.name.chars().count())
        .chain([4])
        .max()
        .unwrap_or(4)
        + 4;
    let states: Vec<String> = entries.iter().map(|e| title_case(&e.status)).collect();
    let state_w = states
        .iter()
        .map(|s| s.len() + 1)
        .chain([16])
        .max()
        .unwrap_or(16);

    let mut prefixes = vec![format!(
        "  {:<name_w$}{:<state_w$}{}",
        "NAME", "STATE", "VERSION"
    )];
    for (e, state) in entries.iter().zip(&states) {
        let star = if e.default { '*' } else { ' ' };
        prefixes.push(format!(
            "{star} {:<name_w$}{state:<state_w$}{VERSION}",
            e.name
        ));
    }
    if verbosity < 2 {
        return prefixes.iter().map(|p| format!("{p}\n")).collect();
    }

    let dash = || "-".to_string();
    let mut rows = vec![
        [
            "KIND", "IP", "CPUS", "MEMORY", "DISK", "PLATFORM", "HOME", "IMAGE",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for e in entries {
        rows.push(vec![
            e.kind.to_string(),
            e.ip.clone().unwrap_or_else(dash),
            e.cpus.map_or_else(dash, |c| c.to_string()),
            e.memory.map_or_else(dash, human_bytes),
            e.disk.map_or_else(dash, human_bytes),
            e.platform.clone().unwrap_or_else(dash),
            e.home.clone().unwrap_or_else(dash),
            e.image.clone().unwrap_or_else(dash),
        ]);
    }
    let prefix_w = prefixes.iter().map(String::len).max().unwrap_or(0) + 2;
    prefixes
        .iter()
        .zip(columns(&rows, 2))
        .map(|(p, extra)| format!("{p:<prefix_w$}{extra}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, status: &str, default: bool) -> Entry {
        Entry {
            name: name.into(),
            status: status.into(),
            default,
            kind: "machine",
            ..Entry::default()
        }
    }

    #[test]
    fn simple_marks_default() {
        let out = format_simple(&[
            entry("alpine", "running", true),
            entry("dev", "stopped", false),
        ]);
        assert_eq!(out, "Container Machines:\nalpine (Default)\ndev\n");
    }

    #[test]
    fn verbose_matches_wsl_single() {
        let out = format_verbose(&[entry("alpine", "running", true)], 1);
        assert_eq!(
            out,
            "  NAME      STATE           VERSION\n\
             * alpine    Running         2\n"
        );
    }

    #[test]
    fn verbose_matches_wsl_multi() {
        let entries = [
            entry("alpine", "stopped", true),
            entry("docker-desktop", "stopped", false),
            entry("docker-desktop-data", "stopped", false),
        ];
        let out = format_verbose(&entries, 1);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "  NAME                   STATE           VERSION",
                "* alpine                 Stopped         2",
                "  docker-desktop         Stopped         2",
                "  docker-desktop-data    Stopped         2",
            ]
        );
    }

    #[test]
    fn very_verbose_appends_columns() {
        let mut e = entry("alpine", "running", true);
        e.ip = Some("192.168.64.6".into());
        e.cpus = Some(7);
        e.memory = Some(34359738368);
        let out = format_verbose(&[e], 2);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("  NAME      STATE           VERSION  KIND"));
        assert!(lines[1].starts_with("* alpine    Running         2        machine  192.168.64.6"));
        assert!(lines[1].contains("32G"));
    }

    #[test]
    fn merge_distro_shadows_and_takes_default() {
        let machines = vec![
            entry("alpine", "running", true),
            entry("x", "stopped", false),
        ];
        let mut d1 = entry("d1", "running", true);
        d1.kind = "distro";
        let mut x = entry("x", "running", false);
        x.kind = "distro";
        let merged = merge(machines, vec![d1, x]);
        let names: Vec<_> = merged
            .iter()
            .map(|e| (e.name.as_str(), e.kind, e.default))
            .collect();
        assert_eq!(
            names,
            [
                ("alpine", "machine", false),
                ("d1", "distro", true),
                ("x", "distro", false)
            ]
        );
    }

    #[test]
    fn quiet_is_names_only() {
        assert_eq!(format_quiet(&[entry("a", "running", true)]), "a\n");
    }
}
