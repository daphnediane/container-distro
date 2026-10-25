#!/bin/sh
# Assemble the release artifacts entirely from --out DIR: signs the
# compiled binary in bin/, stages the payload from catalog/, builds
# the installer .pkg (productbuild --sign when $PKG_SIGN_IDENTITY),
# notarizes + staples when $NOTARY_PROFILE, and produces the
# cargo-binstall tarball with its detached signatures. No source
# checkout needed — a build worktree can already be gone.
#
#   bundle.sh --out DIR
#
# Requires $PKG_VERSION (and reads $PROV_SUFFIX) for artifact naming —
# export the pkg/name.sh values. Signing is env-driven like the rest
# of the pipeline; unsigned outputs are name-mangled `-unsigned`.
set -eu

step() { printf '\n==> %s\n' "$*"; }
HERE=$(cd "$(dirname "$0")" && pwd)

out=
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out=$2; shift 2 ;;
        *) echo "usage: $0 --out DIR" >&2; exit 2 ;;
    esac
done
[ -n "$out" ] || { echo "usage: $0 --out DIR" >&2; exit 2; }
OUT=$(cd "$out" && pwd)

: "${PKG_VERSION:?artifact naming needs PKG_VERSION — eval scripts/pkg/name.sh}"
version=$PKG_VERSION
prov_suffix=${PROV_SUFFIX:-}

COMPONENT=container-distro-core.pkg
IDENTIFIER=io.github.daphnediane.container-distro
TARGET=aarch64-apple-darwin
CATALOG="$OUT/collect"
SBOM="container-distro-$version$prov_suffix-sbom.cdx"

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    step "codesign binary ($CODESIGN_IDENTITY)"
    "$HERE/sign.sh" "$OUT/bin/container-distro"
fi

step "stage payload"
# Payload is rooted at the install-location — /usr/local, same layout
# Apple's container pkg uses — so the receipt's file/dir paths and its
# `location:` field both describe what we actually own.
PAYLOAD="$OUT/payload"
rm -rf "$PAYLOAD"
mkdir -p "$PAYLOAD/bin" "$PAYLOAD/share/doc/container-distro"
cp "$OUT/bin/container-distro" "$CATALOG/uninstall-container-distro.sh" \
    "$CATALOG/update-container-distro.sh" "$PAYLOAD/bin/"
cp "$CATALOG/LICENSE" "$CATALOG/THIRD-PARTY-NOTICES.yaml" \
    "$CATALOG/LICENSE-microsoft-wsl" \
    "$PAYLOAD/share/doc/container-distro/"
# Everything else in the payload — man pages, completions, the `cm`
# alias script, the plugin copy — is produced by `install-all`, the
# same output a manual install writes. `--stage` writes the payload
# while the alias still runs the installed /usr/local binary.
"$PAYLOAD/bin/container-distro" install-all --prefix /usr/local \
    --stage "$PAYLOAD" \
    --plugin-dir "$PAYLOAD/libexec/container-plugins"

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
    "$CATALOG/Distribution.xml" > "$OUT/Distribution.xml"
# productbuild --resources takes a dir — assemble it fresh so the
# generated license screen can live next to the committed pages.
RESOURCES=$OUT/resources
rm -rf "$RESOURCES"
mkdir -p "$RESOURCES"
cp "$CATALOG/welcome.html" "$CATALOG/conclusion.html" "$RESOURCES/"
# The installer's license step names every vendored crate's license
# expression; the full copyright + license texts stay in
# THIRD-PARTY-NOTICES.yaml (payload + release asset).
{
    cat "$CATALOG/LICENSE"
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
    ' "$CATALOG/THIRD-PARTY-NOTICES.yaml"
    echo
    echo "The built-in distribution catalog incorporates Microsoft's"
    echo "DistributionInfo.json (github.com/microsoft/WSL), MIT-licensed"
    echo "(c) Microsoft Corporation; the license text is installed as"
    echo "LICENSE-microsoft-wsl alongside the binaries."
} > "$RESOURCES/licenses.txt"
# A signed pkg keeps the clean name; unsigned builds (CI without a
# keychain) get Apple's own -unsigned convention — the maintainer adds
# the signed build at draft-review time (docs/design/packaging.md).
PKGNAME="container-distro-$version$prov_suffix"
[ -n "${PKG_SIGN_IDENTITY:-}" ] || PKGNAME="$PKGNAME-unsigned"
if [ -n "${PKG_SIGN_IDENTITY:-}" ]; then
    productbuild --distribution "$OUT/Distribution.xml" \
        --resources "$RESOURCES" --package-path "$COMPONENT_DIR" \
        --sign "$PKG_SIGN_IDENTITY" \
        "$OUT/$PKGNAME.pkg"
else
    productbuild --distribution "$OUT/Distribution.xml" \
        --resources "$RESOURCES" --package-path "$COMPONENT_DIR" \
        "$OUT/$PKGNAME.pkg"
    echo "  (unsigned — see docs/design/packaging.md for the signing setup)"
fi

if [ -n "${NOTARY_PROFILE:-}" ] && [ -n "${PKG_SIGN_IDENTITY:-}" ]; then
    step "notarize + staple ($NOTARY_PROFILE)"
    xcrun notarytool submit "$OUT/$PKGNAME.pkg" \
        --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$OUT/$PKGNAME.pkg"
fi

step "cargo-binstall tarball"
# { name }-{ target }-v{ version }.tar.gz with a matching inner dir —
# the layout [package.metadata.binstall] in Cargo.toml advertises. The
# inner dir keeps the canonical name; only the archive name mangles —
# unsigned builds get `-unsigned` like the pkg, so the canonical name
# only ever appears in its final signed form and a signed upload adds
# assets instead of mutating already-attached ones.
TDIR="container-distro-$TARGET-v$version"
TARNAME="$TDIR$prov_suffix.tar.gz"
[ -n "${CODESIGN_IDENTITY:-}" ] || TARNAME="$TDIR$prov_suffix-unsigned.tar.gz"
rm -rf "$OUT/$TDIR" "$OUT/$TDIR"*.tar.gz*
mkdir -p "$OUT/$TDIR"
cp "$OUT/bin/container-distro" "$CATALOG/LICENSE" \
    "$CATALOG/THIRD-PARTY-NOTICES.yaml" "$OUT/$TDIR/"
cp "$CATALOG/LICENSE-microsoft-wsl" "$OUT/$TDIR/"
tar -czf "$OUT/$TARNAME" -C "$OUT" "$TDIR"
rm -rf "$OUT/$TDIR"
# Signed builds give the tarball a detached code signature too — same
# Developer ID Application identity as the binaries, timestamped like
# they are. GPG_KEY_ID adds an armored `gpg --detach-sign` .asc on top.
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    "$HERE/sign.sh" --detached "$OUT/$TARNAME"
fi

step "summary"
pkgutil --payload-files "$COMPONENT_DIR/$COMPONENT" | sed 's/^/  /'
SUMFILES="$OUT/$PKGNAME.pkg $OUT/$TARNAME"
[ -f "$OUT/$TARNAME.sig" ] && SUMFILES="$SUMFILES $OUT/$TARNAME.sig"
[ -f "$OUT/$TARNAME.asc" ] && SUMFILES="$SUMFILES $OUT/$TARNAME.asc"
[ -f "$OUT/$SBOM.json" ] && SUMFILES="$SUMFILES $OUT/$SBOM.json"
shasum -a 256 $SUMFILES | sed 's/^/  /'
