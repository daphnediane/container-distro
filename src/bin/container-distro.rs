/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro` — see [`container_distro::cli::distro`].

use container_distro::setup::layout::FrontEnd;

fn main() -> std::process::ExitCode {
    container_distro::cli::main(FrontEnd::Distro)
}
