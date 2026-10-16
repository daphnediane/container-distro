/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Fixtures shared by the tests of several submodules.

use crate::catalog::{self};

pub(super) fn builtin() -> catalog::Catalog {
    catalog::load(None).unwrap()
}
