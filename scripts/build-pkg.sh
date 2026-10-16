#!/bin/sh
# Build the macOS installer .pkg, the cargo-binstall tarball, and — when
# cargo-cyclonedx is installed — the CycloneDX SBOM that the release
# workflow attaches to GitHub Releases (#14, #28). When cargo-auditable
# is installed the binaries also embed their dep tree for
# `cargo audit bin`. Both plugins are recommended but optional here —
# build-pkg-signed.sh and release.yml (the official release paths)
# require them, so shipped artifacts always carry the metadata.
# macOS only — needs cargo, jq, pkgbuild, and productbuild. Output
# lands in scratch/pkg/ (gitignored). `--skip-build` reuses the
# existing target/release build.
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
    # `cargo auditable` is a drop-in cargo wrapper that embeds the
    # resolved dep tree (zlib JSON, `.dep-v0` section) into each
    # binary — `cargo audit bin` can then scan an installed copy with
    # no bookkeeping. Interim until native cargo SBOM embedding lands.
    if command -v cargo-auditable >/dev/null; then
        step "cargo auditable build --release"
        cargo auditable build --release
    else
        step "cargo build --release"
        cargo build --release
        echo "  (no cargo auditable — binaries won't embed their dep tree; recommended for releases)"
    fi
fi

# cargo metadata reports the real target dir — honoring
# CARGO_TARGET_DIR and .cargo/config.toml overrides.
META=$(cargo metadata --no-deps --format-version 1)
version=$(echo "$META" | jq -r '.packages[0].version')
R="$(echo "$META" | jq -r '.target_directory')/release"
[ -x "$R/container-distro" ] || {
    echo "release binary missing — run without --skip-build" >&2
    exit 1
}
# Report whether the binaries carry the embedded dep tree — absent on
# a --skip-build reuse of a plain `cargo build`, or when
# cargo-auditable wasn't installed above.
otool -l "$R/container-distro" | grep -q dep-v0 || \
    echo "  note: container-distro has no .dep-v0 dep tree (not built via cargo auditable)"

step "CycloneDX SBOM"
SBOM="container-distro-$version-sbom.cdx"
mkdir -p "$OUT"
rm -f "$OUT/$SBOM.json"   # don't leave a stale artifact from a prior run
if command -v cargo-cyclonedx >/dev/null; then
    # cargo cyclonedx has no output-dir flag and --override-filename
    # rejects path separators, so it writes next to Cargo.toml —
    # generate there and move the file into the output dir. Dev-deps
    # are always omitted and build-only deps land marked
    # scope=excluded; --target pins the dep tree to the shipped
    # platform whatever the build host.
    cargo cyclonedx --format json --spec-version 1.5 \
        --target "$TARGET" --override-filename "$SBOM"
    mv "$SBOM.json" "$OUT/$SBOM.json"
else
    echo "  (no cargo cyclonedx — skipping the SBOM; recommended for releases)"
fi

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
    step "codesign binary ($APP_IDENTITY)"
    codesign --force --sign "$APP_IDENTITY" \
        --options runtime --timestamp "$R/container-distro"
fi

step "stage payload"
# Payload is rooted at the install-location — /usr/local, same layout
# Apple's container pkg uses — so the receipt's file/dir paths and its
# `location:` field both describe what we actually own.
PAYLOAD="$OUT/payload"
rm -rf "$PAYLOAD"
mkdir -p "$PAYLOAD/bin" "$PAYLOAD/share/doc/container-distro"
cp "$R/container-distro" packaging/uninstall-container-distro.sh \
    packaging/update-container-distro.sh "$PAYLOAD/bin/"
cp LICENSE THIRD-PARTY-NOTICES.yaml "$PAYLOAD/share/doc/container-distro/"
# The vendored Microsoft manifest's license — not a crate, so
# THIRD-PARTY-NOTICES.yaml doesn't cover it (#31).
cp vendor/microsoft/LICENSE "$PAYLOAD/share/doc/container-distro/LICENSE-microsoft-wsl"
# Everything else in the payload — man pages, completions, the `cm`
# alias script, the plugin copy — is produced by `install-all`, the
# same output a manual install writes. `--stage` writes the payload
# while the alias still runs the installed /usr/local binary.
"$PAYLOAD/bin/container-distro" install-all --prefix /usr/local \
    --stage "$PWD/$PAYLOAD" \
    --plugin-dir "$PWD/$PAYLOAD/libexec/container-plugins"

step "pkgbuild"
# The component pkg is an intermediate, not a release asset — keep it
# in a subdir so CI's scratch/pkg/container-distro-*.pkg glob can't
# pick it up.
COMPONENT_DIR=$OUT/component
mkdir -p "$COMPONENT_DIR"
pkgbuild --root "$PAYLOAD" \
    --identifier "$IDENTIFIER" \
    --version "$version" \
    --install-location /usr/local \
    --ownership recommended \
    "$COMPONENT_DIR/$COMPONENT"

step "productbuild"
sed -e "s/@VERSION@/$version/g" -e "s/@COMPONENT@/$COMPONENT/" \
    packaging/Distribution.xml > "$OUT/Distribution.xml"
# productbuild --resources takes a dir — assemble it fresh so the
# generated license screen can live next to the committed pages.
RESOURCES=$OUT/resources
rm -rf "$RESOURCES"
mkdir -p "$RESOURCES"
cp packaging/welcome.html packaging/conclusion.html "$RESOURCES/"
# The installer's license step names every vendored crate's license
# expression; the full copyright + license texts stay in
# THIRD-PARTY-NOTICES.yaml (payload + release asset).
{
    cat LICENSE
    echo
    echo "======================================================================"
    echo "Third-party components"
    echo
    echo "container-distro is built with these Rust crates. The complete"
    echo "copyright and license texts are reproduced in"
    echo "THIRD-PARTY-NOTICES.yaml, installed alongside the binaries and"
    echo "attached to each GitHub Release."
    echo
    awk '
        /^- package_name:/    { name=$NF }
        /^  package_version:/ { ver=$NF }
        /^  license: /        { print "  " name " " ver " — " substr($0, index($0, ":") + 2) }
    ' THIRD-PARTY-NOTICES.yaml
    echo
    echo "The built-in distribution catalog incorporates Microsoft's"
    echo "DistributionInfo.json (github.com/microsoft/WSL), MIT-licensed"
    echo "(c) Microsoft Corporation; the license text is installed as"
    echo "LICENSE-microsoft-wsl alongside the binaries."
} > "$RESOURCES/licenses.txt"
# A signed pkg keeps the clean name; unsigned builds (CI without a
# keychain) get Apple's own -unsigned convention — the maintainer adds
# the signed build at draft-review time (docs/design/packaging.md).
PKGNAME="container-distro-$version"
[ -n "$PKG_IDENTITY" ] || PKGNAME="$PKGNAME-unsigned"
if [ -n "$PKG_IDENTITY" ]; then
    productbuild --distribution "$OUT/Distribution.xml" \
        --resources "$RESOURCES" --package-path "$COMPONENT_DIR" \
        --sign "$PKG_IDENTITY" \
        "$OUT/$PKGNAME.pkg"
else
    productbuild --distribution "$OUT/Distribution.xml" \
        --resources "$RESOURCES" --package-path "$COMPONENT_DIR" \
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
cp "$R/container-distro" LICENSE THIRD-PARTY-NOTICES.yaml "$OUT/$TDIR/"
cp vendor/microsoft/LICENSE "$OUT/$TDIR/LICENSE-microsoft-wsl"
tar -czf "$OUT/$TDIR.tar.gz" -C "$OUT" "$TDIR"
rm -rf "$OUT/$TDIR"

step "summary"
pkgutil --payload-files "$COMPONENT_DIR/$COMPONENT" | sed 's/^/  /'
SUMFILES="$OUT/$PKGNAME.pkg $OUT/$TDIR.tar.gz"
[ -f "$OUT/$SBOM.json" ] && SUMFILES="$SUMFILES $OUT/$SBOM.json"
shasum -a 256 $SUMFILES | sed 's/^/  /'
