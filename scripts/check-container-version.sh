#!/bin/sh
# Container version currency gate: the newest apple/container release
# upstream must be a minor we have verified, and the install-offer pin
# must name the newest verified release — otherwise a release would
# ship warning users off (or offering to install) a version whose
# appRoot internals we haven't checked. Failing means: run the
# version-gating verification (docs/design/container-internals.md),
# then bump VERIFIED_CONTAINER_MINOR plus PINNED_CONTAINER_VERSION and
# PKG_SHA256. Needs curl, jq, and network access to GitHub.
set -eu
cd "$(dirname "$0")/.."

MOD=src/container/mod.rs

latest=$(curl -fsSL \
    "https://api.github.com/repos/apple/container/releases/latest" |
    jq -r '.tag_name' | sed 's/^v//')
[ -n "$latest" ] && [ "$latest" != "null" ] || {
    echo "could not resolve latest apple/container release" >&2
    exit 1
}
minor=${latest%.*}

# VERIFIED_CONTAINER_MINOR = ["1.5", "1.6"] -> "1.5 1.6"
verified=$(sed -n 's/^pub const VERIFIED_CONTAINER_MINOR.*=.*\[\(.*\)\].*/\1/p' "$MOD" | tr -d ' "' | tr ',' ' ')
pinned=$(sed -n 's/^pub const PINNED_CONTAINER_VERSION: &str = "\([^"]*\)".*/\1/p' "$MOD")

ok=1
case " $verified " in
    *" $minor "*) ;;
    *)
        echo "FAIL: container $latest is out; minor $minor is not in" >&2
        echo "      VERIFIED_CONTAINER_MINOR ($verified) — run the version-gating" >&2
        echo "      verification (docs/design/container-internals.md), then bump" >&2
        echo "      VERIFIED_CONTAINER_MINOR, PINNED_CONTAINER_VERSION, PKG_SHA256" >&2
        ok=0 ;;
esac

if [ "$pinned" != "$latest" ] && [ "$ok" = 1 ]; then
    echo "FAIL: PINNED_CONTAINER_VERSION is $pinned but latest upstream" >&2
    echo "      release is $latest — the install offer should pin the" >&2
    echo "      newest verified release" >&2
    ok=0
fi

[ "$ok" = 1 ] || exit 1
echo "ok: latest container $latest is verified (minor $minor); pin $pinned"
