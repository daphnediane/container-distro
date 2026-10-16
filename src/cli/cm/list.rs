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

use crate::container::{Machine, MachineDetail};
use crate::distro::DistroSummary;
use crate::table::{columns, human_bytes, title_case};

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;

use super::backend;
use crate::catalog_ops;
use crate::container;
use crate::distro::spec::Arch;

/// Header printed by `cm -l` (WSL: "Windows Subsystem for Linux Distributions:").
pub const LIST_HEADER: &str = "Container Machines:";

/// One row of list output, independent of the backend that produced it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: String,
    pub status: String,
    pub default: bool,
    pub kind: &'static str,
    /// A machine and a distro share this name; displays annotate it.
    pub conflict: bool,
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
            conflict: false,
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
            conflict: false,
            ip: d.ip_address.clone(),
            cpus: d.cpus,
            memory: d.memory,
            disk: d.disk_size,
            platform: d.platform.clone(),
            home: d.home_mount.clone(),
            image: d.source.clone().or_else(|| d.image.clone()),
        }
    }
}

/// Merge machines and distros into one list. Same-named pairs are
/// conflicts: both rows stay and are marked so every format can
/// annotate them; shadowing is an operational-resolution concern, not a
/// display one. A default distro still takes the default marker from
/// the default machine (matching [`super::backend::resolve`]).
pub fn merge(mut machines: Vec<Entry>, distros: Vec<Entry>) -> Vec<Entry> {
    let distro_names: std::collections::BTreeSet<&str> =
        distros.iter().map(|d| d.name.as_str()).collect();
    let conflicts: std::collections::BTreeSet<String> = machines
        .iter()
        .filter(|m| distro_names.contains(m.name.as_str()))
        .map(|m| m.name.clone())
        .collect();
    if distros.iter().any(|d| d.default) {
        machines.iter_mut().for_each(|m| m.default = false);
    }
    machines.extend(distros);
    for e in &mut machines {
        e.conflict = conflicts.contains(&e.name);
    }
    machines
}

impl Entry {
    /// The name as printed: suffixed with the backend kind when a
    /// same-named machine and distro would otherwise be ambiguous.
    fn display_name(&self) -> String {
        if self.conflict {
            format!("{} ({})", self.name, self.kind)
        } else {
            self.name.clone()
        }
    }
}

/// `cm -l`: header, then names with the default marked.
pub fn format_simple(entries: &[Entry]) -> String {
    let mut out = format!("{LIST_HEADER}\n");
    for e in entries {
        out.push_str(&e.display_name());
        if e.default {
            out.push_str(" (Default)");
        }
        out.push('\n');
    }
    out
}

/// `cm -l -q`: bare names; conflicted names carry their kind suffix so
/// the two backends stay distinguishable.
pub fn format_quiet(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| format!("{}\n", e.display_name()))
        .collect()
}

/// WSL reports 1 or 2; a container machine is a full VM, like WSL 2.
const VERSION: &str = "2";

/// `cm -l -v` (`verbosity == 1`) or `-l -v -v` (`verbosity >= 2`).
///
/// The leading `NAME STATE VERSION` columns replicate `wsl.exe`: NAME is
/// padded to the longest name + 4, STATE to 16.
pub fn format_verbose(entries: &[Entry], verbosity: u8) -> String {
    let names: Vec<String> = entries.iter().map(Entry::display_name).collect();
    let name_w = names
        .iter()
        .map(|n| n.chars().count())
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
    for ((e, name), state) in entries.iter().zip(&names).zip(&states) {
        let star = if e.default { '*' } else { ' ' };
        prefixes.push(format!("{star} {name:<name_w$}{state:<state_w$}{VERSION}"));
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

pub(super) fn run(
    running_only: bool,
    quiet: bool,
    verbosity: u8,
    online: bool,
    cache: bool,
    catalog_file: Option<PathBuf>,
    arch: Option<Arch>,
) -> Result<ExitCode> {
    if online {
        print!(
            "{}",
            catalog_ops::render_online(
                catalog_file.as_deref(),
                verbosity,
                true,
                backend::distros_enabled(),
                "cm --install <Distro>",
                arch.unwrap_or_else(Arch::host),
            )?
        );
        return Ok(ExitCode::SUCCESS);
    }
    if cache {
        print!("{}", catalog_ops::render_cache(catalog_file.as_deref()));
        return Ok(ExitCode::SUCCESS);
    }
    container::ensure_started()?;
    let machines = container::list_machines()?;
    let distros = backend::distro_summaries(running_only)?;
    for name in backend::clashes(&machines, &distros) {
        eprintln!("cm: `{name}` is both a machine and a distro; `-d {name}` uses the distro");
    }
    let mut machine_entries = Vec::new();
    for m in machines {
        if running_only && !m.is_running() {
            continue;
        }
        // --arch filters on a machine's platform, which only inspect
        // reports.
        let detail = if verbosity >= 2 || arch.is_some() {
            container::inspect_machine(&m.id).ok()
        } else {
            None
        };
        machine_entries.push(Entry::from_machine(&m, detail.as_ref()));
    }
    let mut entries = merge(
        machine_entries,
        distros.iter().map(Entry::from_distro).collect(),
    );
    if let Some(arch) = arch {
        entries.retain(|e| {
            e.platform
                .as_deref()
                .is_some_and(|p| p.rsplit('/').next() == Some(arch.as_str()))
        });
    }
    if entries.is_empty() {
        if let Some(arch) = arch {
            eprintln!("No {arch} container machines or distros are installed.");
        } else if running_only {
            eprintln!("There are no running container machines or distros.");
        } else {
            eprintln!(
                "No container machines or distros are installed.\n\
                 Use `cm --install <image>` to create one."
            );
        }
        return Ok(ExitCode::FAILURE);
    }
    let out = if quiet {
        format_quiet(&entries)
    } else if verbosity > 0 {
        format_verbose(&entries, verbosity)
    } else {
        format_simple(&entries)
    };
    print!("{out}");
    Ok(ExitCode::SUCCESS)
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
    fn merge_keeps_conflicting_rows_and_annotates_them() {
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
            .map(|e| (e.name.as_str(), e.kind, e.default, e.conflict))
            .collect();
        assert_eq!(
            names,
            [
                ("alpine", "machine", false, false),
                ("x", "machine", false, true),
                ("d1", "distro", true, false),
                ("x", "distro", false, true)
            ]
        );
        let out = format_simple(&merged);
        assert!(out.contains("x (machine)\n"));
        assert!(out.contains("x (distro)\n"));
        assert!(out.contains("alpine\n"));
        assert!(out.contains("d1 (Default)\n"));
    }

    #[test]
    fn quiet_is_names_only() {
        assert_eq!(format_quiet(&[entry("a", "running", true)]), "a\n");
    }
}
