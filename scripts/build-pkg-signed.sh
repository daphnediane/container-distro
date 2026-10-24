#!/bin/sh
# Maintainer signed+notarized build: sources the gitignored
# packaging/signing.local.conf (see signing.conf.example) for the cert
# identities + notary profile, then runs build-pkg.sh --sign. Setting
# GPG_KEY_ID there additionally signs the binstall tarball with gpg.
# Upload the result over the draft release with
# scripts/upload-release-artifacts.sh (docs/design/release.md step 6).
# Extra args pass through (--skip-build etc).
set -eu
cd "$(dirname "$0")/.."

CONF=packaging/signing.local.conf
if [ ! -f "$CONF" ]; then
    echo "missing $CONF — copy packaging/signing.conf.example and fill in your values" >&2
    exit 1
fi
. "$CONF"

missing=
for v in CODESIGN_IDENTITY PKG_SIGN_IDENTITY NOTARY_PROFILE; do
    eval "val=\${$v:-}"
    [ -n "$val" ] || missing="$missing $v"
done
if [ -n "$missing" ]; then
    echo "unset in $CONF:$missing — update it (see packaging/signing.conf.example)" >&2
    exit 1
fi
# GPG_KEY_ID is optional — when set, build-pkg.sh emits the tarball's
# .asc next to the code-signature .sig.
export CODESIGN_IDENTITY PKG_SIGN_IDENTITY NOTARY_PROFILE GPG_KEY_ID

# Official artifacts always carry the SBOM + embedded dep tree —
# build-pkg.sh treats the plugins as optional, the signed path doesn't.
missing=
for t in cargo-auditable cargo-cyclonedx; do
    command -v "$t" >/dev/null || missing="$missing $t"
done
if [ -n "$missing" ]; then
    echo "missing tools:$missing — cargo install --locked$missing" >&2
    exit 1
fi

exec scripts/build-pkg.sh --sign "$@"
