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
#   GPG_KEY_ID         optional — an armored `gpg --detach-sign` .asc for
#                      the tarball, next to its detached code signature
# None are required: without them the outputs are unsigned, and the
# docs' `sudo installer -pkg` / right-click-Open steps apply. A signed
# build also signs the tarball — a detached code signature (.sig) under
# the same Developer ID Application identity as the binaries, verifiable
# with `codesign --verify --detached=`. Unsigned builds get a `-unsigned`
# tarball name like the pkg, so the canonical
# {name}-{target}-v{version}.tar.gz (fixed by the binstall metadata)
# only ever appears in its final signed form on a release. A build
# that will sign builds a `git worktree` of the `v<version>` tag under
# scratch/ so the source can't move mid-build (#50); `--allow-dirty`
# signs the working tree instead, for test builds.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

SKIP_BUILD=
SIGN=auto
ALLOW_DIRTY=
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=1 ;;
        --sign) SIGN=1 ;;
        --no-sign) SIGN=0 ;;
        --allow-dirty) ALLOW_DIRTY=1 ;;
        *) echo "usage: $0 [--skip-build] [--sign|--no-sign] [--allow-dirty]" >&2; exit 2 ;;
    esac
done

OUT=${CONTAINER_DISTRO_PKG_OUT:-scratch/pkg}
COMPONENT=container-distro-core.pkg
IDENTIFIER=io.github.daphnediane.container-distro
TARGET=aarch64-apple-darwin

# Find the first keychain identity with the given cert name under the
# given search policy. Empty when absent (the unsigned path). Resolved
# before the build: --sign fails fast instead of after a full cargo
# run, and the provenance check below knows whether this run signs.
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

# cargo metadata reports the real target dir — honoring
# CARGO_TARGET_DIR and .cargo/config.toml overrides. Needed before the
# build now: the provenance decision depends on the package version.
META=$(cargo metadata --no-deps --format-version 1)
version=$(echo "$META" | jq -r '.packages[0].version')
pv_tag=$(git describe --exact-match --tags HEAD 2>/dev/null || true)
pv_dirty=$(git status --porcelain)

# Provenance anchor (#50): signed artifacts attest to the tagged
# source, so a signing run doesn't build this checkout at all — it
# re-enters itself inside a `git worktree` of v<version> under
# scratch/, which is immune to uncommitted changes *and* to a checkout
# moved mid-build (a branch switch during the minutes-long cargo run
# has actually happened). --allow-dirty signs the working tree
# directly, for test builds.
if [ -n "$APP_IDENTITY$PKG_IDENTITY" ] && [ -z "$ALLOW_DIRTY" ] &&
    [ -z "${CONTAINER_DISTRO_PKG_FROM_WORKTREE:-}" ]; then
    git rev-parse -q --verify "refs/tags/v$version" >/dev/null || {
        echo "provenance: no tag v$version — sign from the tagged commit," >&2
        echo "  or pass --allow-dirty for a working-tree test build" >&2
        exit 1
    }
    [ "$pv_tag" = "v$version" ] ||
        echo "  note: HEAD is not at v$version — building the tag, not the checkout" >&2
    [ -z "$pv_dirty" ] ||
        echo "  note: uncommitted changes excluded — the build comes from the tag" >&2
    SRCDIR="scratch/pkg-src-v$version"
    git worktree remove --force "$SRCDIR" 2>/dev/null || {
        rm -rf "$SRCDIR"
        git worktree prune
    }
    git worktree add --detach "$SRCDIR" "v$version"
    step "worktree build of v$version ($SRCDIR)"
    REPO=$PWD
    pv_status=0
    ( cd "$REPO/$SRCDIR" &&
        CONTAINER_DISTRO_PKG_FROM_WORKTREE=1 \
        CONTAINER_DISTRO_PKG_OUT="$REPO/scratch/pkg" \
        scripts/build-pkg.sh "$@" ) || pv_status=$?
    git worktree remove --force "$SRCDIR"
    exit "$pv_status"
fi

# In-place build — unsigned runs (or --allow-dirty). The tree is only
# checked once, at start: `-unsigned` names are already non-canonical,
# and a dirty tree may be the point of a test build.
if [ "$pv_tag" != "v$version" ] || [ -n "$pv_dirty" ]; then
    if [ "$pv_tag" != "v$version" ]; then
        pv_note="HEAD is not at the v$version tag (found: ${pv_tag:-none})"
    else
        pv_note="working tree is dirty"
    fi
    if [ -n "$APP_IDENTITY$PKG_IDENTITY" ]; then
        echo "provenance: $pv_note — signing an unanchored test build" >&2
    else
        echo "  note: provenance: $pv_note — unsigned build continues" >&2
    fi
fi

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
# Microsoft WSL's license — not a crate, so
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
# the layout [package.metadata.binstall] in Cargo.toml advertises. The
# inner dir keeps the canonical name; only the archive name mangles —
# unsigned builds get `-unsigned` like the pkg, so the canonical name
# only ever appears in its final signed form and a signed upload adds
# assets instead of mutating already-attached ones.
TDIR="container-distro-$TARGET-v$version"
TARNAME="$TDIR.tar.gz"
[ -n "$APP_IDENTITY" ] || TARNAME="$TDIR-unsigned.tar.gz"
rm -rf "$OUT/$TDIR" "$OUT/$TDIR".tar.gz* "$OUT/$TDIR"-unsigned.tar.gz*
mkdir -p "$OUT/$TDIR"
cp "$R/container-distro" LICENSE THIRD-PARTY-NOTICES.yaml "$OUT/$TDIR/"
cp vendor/microsoft/LICENSE "$OUT/$TDIR/LICENSE-microsoft-wsl"
tar -czf "$OUT/$TARNAME" -C "$OUT" "$TDIR"
rm -rf "$OUT/$TDIR"
# Signed builds give the tarball a detached code signature too — same
# Developer ID Application identity as the binaries, timestamped like
# they are. GPG_KEY_ID adds an armored `gpg --detach-sign` .asc on top
# (a passphrase prompt via pinentry is expected on a cold agent).
if [ -n "$APP_IDENTITY" ]; then
    codesign --force --sign "$APP_IDENTITY" --timestamp \
        --detached "$OUT/$TARNAME.sig" "$OUT/$TARNAME"
fi
if [ -n "${GPG_KEY_ID:-}" ]; then
    command -v gpg >/dev/null || {
        echo "GPG_KEY_ID is set but gpg isn't installed" >&2
        exit 1
    }
    gpg --yes --armor --local-user "$GPG_KEY_ID" \
        --output "$OUT/$TARNAME.asc" --detach-sign "$OUT/$TARNAME"
fi

step "summary"
pkgutil --payload-files "$COMPONENT_DIR/$COMPONENT" | sed 's/^/  /'
SUMFILES="$OUT/$PKGNAME.pkg $OUT/$TARNAME"
[ -f "$OUT/$TARNAME.sig" ] && SUMFILES="$SUMFILES $OUT/$TARNAME.sig"
[ -f "$OUT/$TARNAME.asc" ] && SUMFILES="$SUMFILES $OUT/$TARNAME.asc"
[ -f "$OUT/$SBOM.json" ] && SUMFILES="$SUMFILES $OUT/$SBOM.json"
shasum -a 256 $SUMFILES | sed 's/^/  /'
