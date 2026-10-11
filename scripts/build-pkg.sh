#!/bin/sh
# Build the macOS installer .pkg and the cargo-binstall tarball that the
# release workflow attaches to GitHub Releases (#14). macOS only — needs
# cargo, jq, pkgbuild, and productbuild. Output lands in scratch/pkg/
# (gitignored). `--skip-build` reuses the existing target/release build.
#
# Signing is auto-detected (docs/design/packaging.md):
#   --sign           require signing — fail when the identities are absent
#   --no-sign        force the -unsigned artifact even with certs present
#   CODESIGN_IDENTITY  signs the binaries (default: a "Developer ID
#                      Application" keychain identity, if present)
#   PKG_SIGN_IDENTITY  signs the .pkg (default: "Developer ID Installer")
#   NOTARY_PROFILE     notarytool keychain profile — when set, the signed
#                      pkg is notarized and stapled
# None are required: without them the outputs are unsigned, and the
# docs' `sudo installer -pkg` / right-click-Open steps apply.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

SKIP_BUILD=
SIGN=auto
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=1 ;;
        --sign) SIGN=1 ;;
        --no-sign) SIGN=0 ;;
        *) echo "usage: $0 [--skip-build] [--sign|--no-sign]" >&2; exit 2 ;;
    esac
done

OUT=scratch/pkg
COMPONENT=container-distro-core.pkg
IDENTIFIER=io.github.daphnediane.container-distro
TARGET=aarch64-apple-darwin

if [ -z "$SKIP_BUILD" ]; then
    step "cargo build --release"
    cargo build --release
fi

# cargo metadata reports the real target dir — honoring
# CARGO_TARGET_DIR and .cargo/config.toml overrides.
META=$(cargo metadata --no-deps --format-version 1)
version=$(echo "$META" | jq -r '.packages[0].version')
R="$(echo "$META" | jq -r '.target_directory')/release"
[ -x "$R/cm" ] && [ -x "$R/container-distro" ] || {
    echo "release binaries missing — run without --skip-build" >&2
    exit 1
}

# Find the first keychain identity with the given cert name under the
# given search policy. Empty when absent (the unsigned path).
find_identity() {
    security find-identity -v -p "$1" 2>/dev/null |
        sed -n "s/.*\"\($2[^\"]*\)\".*/\1/p" | head -n1
}
if [ "$SIGN" = 0 ]; then
    APP_IDENTITY=
    PKG_IDENTITY=
else
    APP_IDENTITY=${CODESIGN_IDENTITY:-$(find_identity codesigning "Developer ID Application")}
    PKG_IDENTITY=${PKG_SIGN_IDENTITY:-$(find_identity basic "Developer ID Installer")}
fi
if [ "$SIGN" = 1 ] && { [ -z "$APP_IDENTITY" ] || [ -z "$PKG_IDENTITY" ]; }; then
    echo "--sign was given but Developer ID identities are missing" >&2
    echo "  binaries: ${APP_IDENTITY:-not found}" >&2
    echo "  pkg:      ${PKG_IDENTITY:-not found}" >&2
    exit 1
fi

if [ -n "$APP_IDENTITY" ]; then
    step "codesign binaries ($APP_IDENTITY)"
    for b in cm container-distro; do
        codesign --force --sign "$APP_IDENTITY" \
            --options runtime --timestamp "$R/$b"
    done
fi

step "stage payload"
# Payload is rooted at the install-location — /usr/local, same layout
# Apple's container pkg uses — so the receipt's file/dir paths and its
# `location:` field both describe what we actually own.
PAYLOAD="$OUT/payload"
rm -rf "$PAYLOAD"
mkdir -p "$PAYLOAD/bin" "$PAYLOAD/share/doc/container-distro"
cp "$R/cm" "$R/container-distro" packaging/uninstall-container-distro.sh \
    packaging/update-container-distro.sh "$PAYLOAD/bin/"
cp LICENSE THIRD-PARTY-NOTICES.yaml "$PAYLOAD/share/doc/container-distro/"
# Everything else in the payload is produced by the binaries' own
# install commands — the same output a manual install writes.
"$R/container-distro" install-plugin \
    --plugin-dir "$PWD/$PAYLOAD/libexec/container-plugins"
"$R/cm" --install-man="$PWD/$PAYLOAD/share/man/man1"
"$R/container-distro" install-man \
    --dir "$PWD/$PAYLOAD/share/man/man1"
"$R/cm" --install-completions="$PWD/$PAYLOAD/share"
"$R/container-distro" install-completions \
    --dir "$PWD/$PAYLOAD/share"

step "pkgbuild"
pkgbuild --root "$PAYLOAD" \
    --identifier "$IDENTIFIER" \
    --version "$version" \
    --install-location /usr/local \
    --ownership recommended \
    "$OUT/$COMPONENT"

step "productbuild"
sed -e "s/@VERSION@/$version/g" -e "s/@COMPONENT@/$COMPONENT/" \
    packaging/Distribution.xml > "$OUT/Distribution.xml"
# A signed pkg keeps the clean name; unsigned builds (CI without a
# keychain) get Apple's own -unsigned convention — the maintainer adds
# the signed build at draft-review time (docs/design/packaging.md).
PKGNAME="container-distro-$version"
[ -n "$PKG_IDENTITY" ] || PKGNAME="$PKGNAME-unsigned"
if [ -n "$PKG_IDENTITY" ]; then
    productbuild --distribution "$OUT/Distribution.xml" \
        --resources packaging --package-path "$OUT" \
        --sign "$PKG_IDENTITY" \
        "$OUT/$PKGNAME.pkg"
else
    productbuild --distribution "$OUT/Distribution.xml" \
        --resources packaging --package-path "$OUT" \
        "$OUT/$PKGNAME.pkg"
    echo "  (unsigned — see docs/design/packaging.md for the signing setup)"
fi

if [ -n "${NOTARY_PROFILE:-}" ] && [ -n "$PKG_IDENTITY" ]; then
    step "notarize + staple ($NOTARY_PROFILE)"
    xcrun notarytool submit "$OUT/$PKGNAME.pkg" \
        --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$OUT/$PKGNAME.pkg"
fi

step "cargo-binstall tarball"
# { name }-{ target }-v{ version }.tar.gz with a matching inner dir —
# the layout [package.metadata.binstall] in Cargo.toml advertises.
TDIR="container-distro-$TARGET-v$version"
rm -rf "$OUT/$TDIR" "$OUT/$TDIR.tar.gz"
mkdir -p "$OUT/$TDIR"
cp "$R/cm" "$R/container-distro" LICENSE THIRD-PARTY-NOTICES.yaml "$OUT/$TDIR/"
tar -czf "$OUT/$TDIR.tar.gz" -C "$OUT" "$TDIR"
rm -rf "$OUT/$TDIR"

step "summary"
pkgutil --payload-files "$OUT/$COMPONENT" | sed 's/^/  /'
shasum -a 256 \
    "$OUT/$PKGNAME.pkg" \
    "$OUT/$TDIR.tar.gz" | sed 's/^/  /'
