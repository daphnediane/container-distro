/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Fixtures shared by the tests of several submodules.

use crate::container::MachineDetail;
use crate::distro::spec::{DistroSpec, HomeMount, HostUser, Intent, Preset, WslConfMode};
use std::fs;

// ---- guest-side script tests: real `container run` invocations ----

/// libtest captures stdout/stderr and hides it for passing tests, so
/// a skip notice must bypass it — write straight to /dev/stderr.
pub(super) fn skip(test: &str, why: &str) {
    use std::io::Write;
    if let Ok(mut e) = fs::OpenOptions::new().write(true).open("/dev/stderr") {
        let _ = writeln!(e, "SKIP {test}: {why}");
    }
}

/// An intent-only spec whose pins resolve to the shape the old
/// tests' effective values had.
pub(super) fn spec() -> DistroSpec {
    DistroSpec {
        name: "d1".into(),
        image: "alpine:latest".into(),
        arch: None,
        mounts: vec!["/a:/b:ro".parse().unwrap()],
        publish: vec!["8080:80".parse().unwrap()],
        user: HostUser {
            name: "dp".into(),
            uid: 501,
            gid: 20,
        },
        preset: Preset::Default,
        explicit: Intent {
            cpus: Some(2),
            memory: Some("4G".into()),
            home_mount: Some(HomeMount::Rw),
            ssh: Some(true),
            admin: Some(true),
            wsl_conf: Some(WslConfMode::all()),
            ..Intent::default()
        },
        source: Intent::default(),
        admin_grant: false,
        origin: None,
    }
}

// ---- migrate ----

pub(super) fn detail_json() -> MachineDetail {
    serde_json::from_str(
        r#"{"id":"m","status":"stopped","cpus":4,"memory":4294967296,
            "homeMount":"ro","containerId":"m-1a2b3c",
            "userSetup":{"uid":501,"gid":20,"username":"dp"},
            "image":{"reference":"docker.io/library/alpine:latest"},
            "platform":{"os":"linux","architecture":"arm64"}}"#,
    )
    .unwrap()
}
