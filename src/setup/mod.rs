/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Installing and updating ourselves: the install layout, the
//! `container` plugin registration, man pages, completions,
//! self-update, and the sudo re-exec those need.

pub mod completions;
pub mod layout;
pub mod man;
pub mod plugin;
pub(crate) mod privilege;
pub mod update;
