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
//! The remaining modules are shared plumbing for the `cm` and
//! `container distro` binaries: wrappers around the `container` CLI, the
//! serde types for its JSON output, and small utilities (distro catalog,
//! OCI layout writer, port forwarder, table helpers). Distro support in
//! `cm` comes from here too, so it works without the plugin installed.

pub mod catalog;
pub mod completions;
pub mod container;
pub mod forward;
pub mod man;
pub mod naming;
pub mod oci;
pub mod ops;
pub mod plugin;
pub mod spec;
pub mod table;
pub mod wsl;
