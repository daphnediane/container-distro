/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Fixtures shared by the tests of several submodules.

use super::*;

pub(super) const INSPECT_JSON: &str = r#"[{"containerId":"alpine-730439","cpus":7,"homeMount":"rw","id":"alpine",
    "image":{"descriptor":{"digest":"sha256:a2","mediaType":"application/vnd.oci.image.index.v1+json","size":9218},
             "reference":"docker.io/library/alpine:latest"},
    "ipAddress":"192.168.64.6","platform":{"architecture":"arm64","os":"linux"},"status":"running",
    "userSetup":{"gid":20,"uid":1027,"username":"daphnediane"}}]"#;

pub(super) fn detail() -> MachineDetail {
    let d: Vec<MachineDetail> = serde_json::from_str(INSPECT_JSON).unwrap();
    d.into_iter().next().unwrap()
}
