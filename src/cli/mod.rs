/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! The command-line front ends: `cm`'s WSL-style interface and the
//! `container distro` plugin's subcommands. Both binaries are thin
//! `main`s over these.

pub mod cm;
pub mod distro;
mod docs;
