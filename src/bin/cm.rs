/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `cm` — see [`container_distro::cli::cm`].

use container_distro::setup::layout::FrontEnd;

fn main() -> std::process::ExitCode {
    container_distro::cli::main(FrontEnd::Cm)
}
