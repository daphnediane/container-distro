/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro list`.

use anyhow::Result;
use clap::Args;

use super::args::Format;
use crate::distro::spec::Arch;
use crate::distro::{self, DistroSummary};
use crate::table::{columns, human_bytes, local_datetime};

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only names
    #[arg(short, long)]
    pub quiet: bool,
    /// Only running distros
    #[arg(long)]
    pub running: bool,
    /// Only distros of this architecture
    #[arg(long, value_enum, value_name = "ARCH")]
    pub arch: Option<Arch>,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

pub(super) fn run(args: ListArgs) -> Result<()> {
    let ListArgs {
        quiet,
        running,
        arch,
        format,
    } = args;
    crate::container::ensure_started()?;
    let mut rows = distro::summaries(running)?;
    if let Some(arch) = arch {
        rows.retain(|r| {
            r.platform
                .as_deref()
                .is_some_and(|p| p.rsplit('/').next() == Some(arch.as_str()))
        });
    }
    if quiet {
        rows.iter().for_each(|r| println!("{}", r.id));
    } else if format == Format::Json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        print_table(&rows);
    }
    Ok(())
}

fn print_table(rows: &[DistroSummary]) {
    let dash = || "-".to_string();
    // Unpinned resources render the host's pick bracketed — `[8]`
    // means "derived, not set".
    let (def_cpus, def_mem) = crate::distro::default_resources();
    let def_mem = crate::distro::spec::size_bytes(&def_mem).unwrap_or(0);
    let mut table = vec![
        [
            "NAME", "IMAGE", "CREATED", "STATE", "IP", "CPUS", "MEMORY", "DISK", "MOUNTS", "PORTS",
            "DEFAULT",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for r in rows {
        table.push(vec![
            r.id.clone(),
            r.source
                .clone()
                .or_else(|| r.image.clone())
                .unwrap_or_else(dash),
            r.created_date
                .as_deref()
                .and_then(local_datetime)
                .unwrap_or_else(dash),
            r.status.clone(),
            r.ip_address.clone().unwrap_or_else(dash),
            // Unpinned resources show the host's pick bracketed —
            // `[8]` means "derived, not set" — except rows with no
            // spec at all (an interrupted create), which get `-`.
            r.cpus.map_or_else(
                || {
                    if r.status == "interrupted" {
                        dash()
                    } else {
                        format!("[{def_cpus}]")
                    }
                },
                |c| c.to_string(),
            ),
            r.memory.map_or_else(
                || {
                    if r.status == "interrupted" {
                        dash()
                    } else {
                        format!("[{}]", human_bytes(def_mem))
                    }
                },
                human_bytes,
            ),
            r.disk_size.map_or_else(dash, human_bytes),
            {
                // User mounts, then compact markers for the derived
                // shares: `~`/`~(ro)` for home, `automounts`/
                // `automounts(ro)` for the /Volumes set.
                let mut m = r.mounts.clone();
                match r.home_mount.as_deref() {
                    Some("ro") => m.push("~(ro)".into()),
                    Some("rw") => m.push("~".into()),
                    _ => {}
                }
                match r.automount.as_deref() {
                    Some("ro") => m.push("automounts(ro)".into()),
                    Some("rw") => m.push("automounts".into()),
                    _ => {}
                }
                if m.is_empty() { dash() } else { m.join(",") }
            },
            if r.ports.is_empty() {
                dash()
            } else {
                r.ports.join(",")
            },
            if r.default { "*".into() } else { String::new() },
        ]);
    }
    for line in columns(&table, 2) {
        println!("{}", line.trim_end());
    }
}
