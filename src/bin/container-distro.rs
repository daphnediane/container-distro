/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container-distro`, the `container distro` plugin, and (through
//! alias scripts running `container-distro cli`) `cm` — see
//! [`container_distro::cli`].

fn main() -> std::process::ExitCode {
    container_distro::cli::main()
}
