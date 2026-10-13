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

use std::sync::atomic::{AtomicBool, Ordering};

/// `--dev`: process-wide opt-in for test seams that steer a trust
/// decision — the `CONTAINER_CLI` binary override and the
/// `CONTAINER_DISTRO_RELEASES_URL` fixture server. These must hang off
/// an explicit flag on the invocation, not an env var alone: a
/// variable planted in a shell profile would otherwise silently
/// redirect a privileged install or every spawned `container` call.
static DEV_MODE: AtomicBool = AtomicBool::new(false);

/// Turn on dev-mode overrides; the binaries call this when `--dev`
/// was parsed.
pub fn enable_dev() {
    DEV_MODE.store(true, Ordering::Relaxed);
}

/// Whether `--dev` was passed on this invocation.
pub fn dev_mode() -> bool {
    DEV_MODE.load(Ordering::Relaxed)
}

pub mod catalog;
pub mod catalog_ops;
pub mod completions;
pub mod container;
pub mod forward;
mod install;
pub mod man;
pub mod naming;
pub mod oci;
pub mod ops;
pub mod plugin;
pub mod spec;
pub mod table;
pub mod terminal;
pub mod update;
pub mod wsl;
