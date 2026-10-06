/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Shared plumbing for `cm` and `container-distro`: wrappers around the
//! `container` CLI, the serde types for its JSON output, and small
//! utilities (OCI layout writer, port forwarder, table helpers).

pub mod container;
pub mod forward;
pub mod man;
pub mod naming;
pub mod oci;
pub mod table;
