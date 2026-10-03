/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Machine-like Linux distros for Apple `container`.
//!
//! A distro is a regular container configured like a `container machine`
//! (our init as PID 1 helper, the host account provisioned inside, home
//! shared at the same path) plus what machines lack: mounts outside
//! `$HOME`, published ports, export/import, and changing settings after
//! creation. Everything is composed from the `container` CLI.
//!
//! Used by both the `container distro` CLI plugin and `cm`, so `cm` gets
//! distro support without the plugin being installed.

pub mod ops;
pub mod plugin;
pub mod spec;
