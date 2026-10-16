/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Fixtures shared by the tests of several submodules.

use super::*;

pub(super) fn user() -> HostUser {
    HostUser {
        name: "dp".into(),
        uid: 501,
        gid: 20,
    }
}

/// An intent-only spec: the pins produce the resolved shape the
/// old tests' effective values had — home ro, agent on, sudo on,
/// 2 cpus/4G, `/Volumes/X` mounted, 8080 published.
pub(super) fn spec() -> DistroSpec {
    DistroSpec {
        name: "d1".into(),
        image: "alpine:latest".into(),
        arch: None,
        mounts: vec!["/Volumes/X:/mnt/x".parse().unwrap()],
        publish: vec!["8080:80".parse().unwrap()],
        user: user(),
        preset: Preset::Default,
        explicit: Intent {
            cpus: Some(2),
            memory: Some("4G".into()),
            home_mount: Some(HomeMount::Ro),
            ssh: Some(true),
            admin: Some(true),
            ..Intent::default()
        },
        source: Intent::default(),
        admin_grant: true,
        origin: None,
    }
}

/// `s.resolve()` — the resolved snapshot plus its warnings.
pub(super) fn resolved(s: &DistroSpec) -> (ResolvedSpec, Vec<String>) {
    s.resolve()
}

pub(super) fn ctx() -> ResolveCtx {
    ResolveCtx {
        cpus: 4,
        memory: "8G".into(),
    }
}

/// `spec()` already pins every value it resolves to — the alias
/// marks tests exercising pin-vs-derive semantics.
pub(super) fn pinned_spec() -> DistroSpec {
    spec()
}

/// A pre-preset journal body — the recorded effective values as a
/// [`JournalSpec`] with no `preset`/`explicit`/`source` — for the
/// back-computation tests.
pub(super) fn legacy_journal(mut j: JournalSpec) -> JournalSpec {
    j.preset = None;
    j.explicit = Intent::default();
    j.source = Intent::default();
    j
}
