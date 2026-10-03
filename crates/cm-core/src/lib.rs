/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Shared plumbing for `cm` and `container-distro`: wrappers around the
//! `container` CLI and the serde types for its JSON output.

pub mod container;
pub mod forward;
pub mod table;
