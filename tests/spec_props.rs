/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Property tests for the `FromStr` parsers in [`container_distro::spec`]
//! (#17): these take user-supplied CLI strings, so they must never panic
//! and must stay inside the documented grammar. Proptest writes any
//! failing case into `proptest-regressions/` — commit those files; they
//! are the regression corpus. `PROPTEST_CASES=<n>` raises the case count
//! for the deeper release-gate run.

use std::str::FromStr;

use container_distro::spec::{Automount, HomeMount, MountSpec, PublishSpec};
use proptest::prelude::*;
use proptest::string::string_regex;

/// A path-ish segment: plausible filename characters, sometimes empty.
fn path_segment() -> impl Strategy<Value = String> {
    string_regex("[A-Za-z0-9._ -]{0,12}").unwrap()
}

/// An absolute-path-ish string (`/` + segments) or a relative one.
fn pathish() -> impl Strategy<Value = String> {
    (
        prop::bool::ANY,
        prop::collection::vec(path_segment(), 0..=4),
    )
        .prop_map(|(absolute, segs)| {
            let joined = segs.join("/");
            if absolute {
                format!("/{joined}")
            } else {
                joined
            }
        })
}

/// `SRC[:DST[:mode]]`-shaped input: near-valid mounts with weird parts.
fn mountish() -> impl Strategy<Value = String> {
    (
        pathish(),
        prop::option::of((
            pathish(),
            prop::option::of(prop::sample::select(&["ro", "rw", "RO", "junk", "", "ro "])),
        )),
    )
        .prop_map(|(src, rest)| match rest {
            None => src,
            Some((dst, None)) => format!("{src}:{dst}"),
            Some((dst, Some(mode))) => format!("{src}:{dst}:{mode}"),
        })
}

/// An IP-ish string: dotted quads, v6-ish, or plain junk.
fn ipish() -> impl Strategy<Value = String> {
    prop_oneof![
        (0u16..=511, 0u16..=511, 0u16..=511, 0u16..=511)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        Just("127.0.0.1".to_string()),
        Just("::1".to_string()),
        Just("::ffff:127.0.0.1".to_string()),
        Just("localhost".to_string()),
        string_regex("[A-Fa-f0-9:]{0,20}").unwrap(),
    ]
}

/// `[IP:]HOST:GUEST[/PROTO]`-shaped input: fields joined by `:` with an
/// optional `/proto` tail.
fn publishish() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(
            prop_oneof![
                ipish(),
                (0u32..100_000).prop_map(|n| n.to_string()),
                Just(String::new()),
            ],
            0..=4,
        ),
        prop::option::of(prop::sample::select(&["tcp", "udp", "TCP", "sctp", ""])),
    )
        .prop_map(|(fields, proto)| {
            let core = fields.join(":");
            match proto {
                Some(p) => format!("{core}/{p}"),
                None => core,
            }
        })
}

proptest! {
    /// Arbitrary strings — including non-UTF8 lossy input — must never
    /// panic the parsers.
    #[test]
    fn mount_spec_never_panics(s in any::<String>(), b in any::<Vec<u8>>()) {
        let _ = MountSpec::from_str(&s);
        let _ = MountSpec::from_str(&String::from_utf8_lossy(&b));
    }

    #[test]
    fn publish_spec_never_panics(s in any::<String>(), b in any::<Vec<u8>>()) {
        let _ = PublishSpec::from_str(&s);
        let _ = PublishSpec::from_str(&String::from_utf8_lossy(&b));
    }

    /// Near-valid mounts: on success both paths are absolute and the
    /// `Display` output re-parses to the same spec.
    #[test]
    fn mount_spec_structured(s in mountish()) {
        if let Ok(m) = MountSpec::from_str(&s) {
            prop_assert!(m.source.starts_with('/'));
            prop_assert!(m.target.starts_with('/'));
            prop_assert_eq!(m.to_string().parse::<MountSpec>().unwrap(), m);
        }
    }

    /// Near-valid publishes: on success the ports are nonzero, the
    /// protocol is tcp/udp, the IP parses, and `Display` round-trips.
    #[test]
    fn publish_spec_structured(s in publishish()) {
        if let Ok(p) = PublishSpec::from_str(&s) {
            prop_assert!(p.host_port > 0 && p.guest_port > 0);
            prop_assert!(p.proto == "tcp" || p.proto == "udp");
            prop_assert!(p.host_ip.parse::<std::net::IpAddr>().is_ok());
            prop_assert_eq!(p.to_string().parse::<PublishSpec>().unwrap(), p);
        }
    }

    /// The mode enums accept exactly `rw`/`ro`/`none`.
    #[test]
    fn home_mount_exact_set(s in any::<String>()) {
        prop_assert_eq!(
            HomeMount::from_str(&s).is_ok(),
            matches!(s.as_str(), "rw" | "ro" | "none")
        );
        prop_assert_eq!(
            Automount::from_str(&s).is_ok(),
            matches!(s.as_str(), "rw" | "ro" | "none")
        );
    }
}
