/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro catalog …` — the installable-distro catalog.

use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgAction, Subcommand};

use super::args::Format;
use crate::catalog;
use crate::catalog_ops;
use crate::ops::CreateOptions;
use crate::spec::Arch;
use crate::wsl;

#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    /// List the installable-distro catalog
    #[command(visible_alias = "ls")]
    List {
        /// Show local cache/image/instance state; repeat for source refs
        #[arg(short, long, action = ArgAction::Count)]
        verbose: u8,
        /// Use FILE instead of the built-in catalog
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
        /// List entries installable on ARCH — amd64 selects x86-64
        /// sources (Rosetta-translated on Apple silicon)
        #[arg(long, value_enum, value_name = "ARCH")]
        arch: Option<Arch>,
    },

    /// Show one catalog entry and its local state
    Info {
        /// Catalog entry to inspect (`catalog list` shows the choices)
        #[arg(value_name = "DISTRO")]
        distro: String,
        /// Use FILE instead of the built-in catalog
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },

    /// Install a distro from the catalog (default: Ubuntu)
    Install {
        /// Catalog entry to install (`catalog list` shows the choices)
        #[arg(value_name = "DISTRO")]
        distro: Option<String>,
        /// Distro name (default: the catalog entry's NAME, or the .wsl
        /// package's oobe.defaultName)
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Use FILE instead of the built-in catalog
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
        #[command(flatten)]
        opts: Box<CreateOptions>,
    },

    /// List cached .wsl catalog downloads
    Cache {
        /// Use FILE instead of the built-in catalog for entry names
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,
    },

    /// Delete cached .wsl catalog downloads
    Purge,
}

pub(super) fn run(command: CatalogCommand) -> Result<()> {
    match command {
        CatalogCommand::List {
            verbose,
            catalog,
            arch,
        } => {
            print!(
                "{}",
                catalog_ops::render_online(
                    catalog.as_deref(),
                    verbose,
                    true,
                    true,
                    "container distro catalog install <DISTRO>",
                    arch.unwrap_or_else(Arch::host),
                )?
            );
        }
        CatalogCommand::Info {
            distro,
            catalog,
            format,
        } => {
            let cat = catalog::load(catalog.as_deref())?;
            let info = catalog_ops::info(&cat, &distro)?;
            if format == Format::Json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{}", catalog_ops::render_info(&info));
            }
        }
        CatalogCommand::Install {
            distro,
            name,
            catalog,
            opts,
        } => {
            let cat = catalog::load(catalog.as_deref())?;
            println!(
                "{}",
                catalog_ops::install(&cat, distro.as_deref(), name, &opts, false)?
            );
        }
        CatalogCommand::Cache { catalog } => {
            print!("{}", catalog_ops::render_cache(catalog.as_deref()));
        }
        CatalogCommand::Purge => {
            let (count, bytes) = wsl::purge_cache();
            println!(
                "Removed {count} cached .wsl file(s), freed {} MiB",
                bytes >> 20
            );
        }
    }
    Ok(())
}
