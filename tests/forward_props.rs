/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Property tests for [`PortMapping`] (#17): `cm --forward` values are
//! user-supplied, so parsing must never panic and must obey
//! `HOST_PORT[:GUEST_PORT]`. Proptest writes any failing case into
//! `proptest-regressions/` — commit those files; they are the
//! regression corpus. `PROPTEST_CASES=<n>` raises the case count for
//! the deeper release-gate run.

use container_distro::forward::PortMapping;
use proptest::prelude::*;
use proptest::string::string_regex;
use std::str::FromStr;

/// `A[:B]`-shaped input: numeric-ish fields joined by `:`.
fn mappingish() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            (0u32..100_000).prop_map(|n| n.to_string()),
            Just(String::new()),
            string_regex("[A-Za-z0-9.+-]{0,8}").unwrap(),
        ],
        0..=3,
    )
    .prop_map(|fields| fields.join(":"))
}

proptest! {
    /// Arbitrary strings — including non-UTF8 lossy input — must never
    /// panic the parser.
    #[test]
    fn port_mapping_never_panics(s in any::<String>(), b in any::<Vec<u8>>()) {
        let _ = PortMapping::from_str(&s);
        let _ = PortMapping::from_str(&String::from_utf8_lossy(&b));
    }

    /// Near-valid mappings: on success both ports are nonzero.
    #[test]
    fn port_mapping_structured(s in mappingish()) {
        if let Ok(m) = PortMapping::from_str(&s) {
            prop_assert!(m.host > 0 && m.guest > 0);
        }
    }

    /// A bare port maps host and guest to the same value; `h:g` maps
    /// them separately.
    #[test]
    fn port_mapping_semantics(p in 1u16..=u16::MAX, h in 1u16..=u16::MAX, g in 1u16..=u16::MAX) {
        prop_assert_eq!(
            p.to_string().parse::<PortMapping>().unwrap(),
            PortMapping { host: p, guest: p }
        );
        prop_assert_eq!(
            format!("{h}:{g}").parse::<PortMapping>().unwrap(),
            PortMapping { host: h, guest: g }
        );
    }

    /// Port 0 is rejected in either position.
    #[test]
    fn port_mapping_rejects_zero(g in 1u16..=u16::MAX) {
        let host_zero = format!("0:{g}");
        let guest_zero = format!("{g}:0");
        prop_assert!("0".parse::<PortMapping>().is_err());
        prop_assert!(host_zero.parse::<PortMapping>().is_err());
        prop_assert!(guest_zero.parse::<PortMapping>().is_err());
    }
}
