#!/bin/sh
# Build the macOS installer .pkg, the cargo-binstall tarball, the
# CycloneDX SBOM, and their signatures (#14, #28). Orchestrator over
# the scripts/pkg/ helpers: name.sh decides artifact naming +
# provenance, worktree.sh materializes tag checkouts, build.sh
# compiles, collect.sh gathers binaries + inputs into the output dir,
# bundle.sh assembles and signs — source-free, so a worktree can go
# away once collecting is done. Output lands in scratch/pkg/
# (gitignored).
#
# Signing is auto-detected (docs/design/packaging.md):
#   --sign           require signing — fail when the identities are absent
#   --no-sign        force the -unsigned artifacts even with certs present
#   --skip-build     don't compile; collect.sh takes the existing
#                    target/release binary (in-place only)
#   --allow-dirty    build the working tree even when signing — the
#                    artifacts get -<hash>/-dirty name mangling
#   --no-worktree    build in place but keep the tag+clean requirement
#                    (re-checked before bundling signs anything)
#   CODESIGN_IDENTITY  signs the binaries (default: a "Developer ID
#                      Application" keychain identity, if present)
#   PKG_SIGN_IDENTITY  signs the .pkg (default: "Developer ID Installer")
#   NOTARY_PROFILE     notarytool keychain profile — when set, the signed
#                      pkg is notarized and stapled
#   GPG_KEY_ID         optional — an armored `gpg --detach-sign` .asc for
#                      the tarball, next to its detached code signature
#
# Provenance (#50): a build whose tag exists on a clean checkout
# compiles inside a `git worktree` of `v<version>` under scratch/ —
# the source can't move mid-build — and prefers the tag's own helpers
# so a release is built by its tag's tooling. Signing without
# --allow-dirty additionally *requires* an anchor. Unanchored builds
# mangle the name too: `-<hash>` off-tag, `-dirty` on a dirty tree,
# `-unsigned` without certs — a canonical name always means tag
# content. Every build re-checks HEAD/version/status before
# bundling, catching a checkout moved mid-build.
set -eu
cd "$(dirname "$0")/.."

PKG=scripts/pkg
SKIP_BUILD=
SIGN=auto
ALLOW_DIRTY=
NO_WORKTREE=
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=1 ;;
        --sign) SIGN=1 ;;
        --no-sign) SIGN=0 ;;
        --allow-dirty) ALLOW_DIRTY=1 ;;
        --no-worktree) NO_WORKTREE=1 ;;
        *) echo "usage: $0 [--skip-build] [--sign|--no-sign] [--allow-dirty|--no-worktree]" >&2; exit 2 ;;
    esac
done

# The output dir: artifacts + collected inputs land here; bundle.sh
# works on it alone.
OUT=${CONTAINER_DISTRO_PKG_OUT:-scratch/pkg}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

# Find the first keychain identity with the given cert name under the
# given search policy. Empty when absent (the unsigned path). Resolved
# before the build: --sign fails fast instead of after a full cargo
# run, and the provenance decision knows whether this run signs.
find_identity() {
    security find-identity -v -p "$1" 2>/dev/null |
        sed -n "s/.*\"\($2[^\"]*\)\".*/\1/p" | head -n1
}
if [ "$SIGN" = 0 ]; then
    CODESIGN_IDENTITY=
    PKG_SIGN_IDENTITY=
else
    CODESIGN_IDENTITY=${CODESIGN_IDENTITY:-$(find_identity codesigning "Developer ID Application")}
    PKG_SIGN_IDENTITY=${PKG_SIGN_IDENTITY:-$(find_identity basic "Developer ID Installer")}
fi
if [ "$SIGN" = 1 ] && { [ -z "$CODESIGN_IDENTITY" ] || [ -z "$PKG_SIGN_IDENTITY" ]; }; then
    echo "--sign was given but Developer ID identities are missing" >&2
    echo "  binaries: ${CODESIGN_IDENTITY:-not found}" >&2
    echo "  pkg:      ${PKG_SIGN_IDENTITY:-not found}" >&2
    exit 1
fi
: "${NOTARY_PROFILE:=}" "${GPG_KEY_ID:=}"
export CODESIGN_IDENTITY PKG_SIGN_IDENTITY NOTARY_PROFILE GPG_KEY_ID

# Naming + provenance state of this checkout (#50).
eval "$("$PKG/name.sh" .)"
orig_version=$PKG_VERSION

# Auto-detected signing only applies to a release-tag build — anywhere
# else stays unsigned (with the name mangling marking why) unless
# --sign explicitly forces it.
if [ "$SIGN" = auto ] && [ -n "$PROV_STATUS" ]; then
    CODESIGN_IDENTITY=
    PKG_SIGN_IDENTITY=
fi

# A clean checkout builds inside a worktree — of the `v<version>` tag
# when it exists, else of HEAD itself (a dev build, named -<hash>) —
# so uncommitted state and mid-build branch switches can't touch the
# source. The worktree only needs to survive through collect.sh;
# bundle.sh is source-free. Dirty trees and the opt-outs build in
# place; signing without --allow-dirty additionally *requires* an
# anchor (dirty → refuse).
SRCDIR=.
if [ -z "$ALLOW_DIRTY" ] && [ -z "$NO_WORKTREE" ] &&
    [ -z "$(git status --porcelain)" ]; then
    pv_ref=HEAD
    git rev-parse -q --verify "refs/tags/v$PKG_VERSION" >/dev/null &&
        pv_ref="v$PKG_VERSION"
    SRCDIR="scratch/pkg-src-$PKG_VERSION$PROV_SUFFIX"
    "$PKG/worktree.sh" add "$SRCDIR" "$pv_ref"
    # Names are canonical inside a tag worktree (it *is* the tag),
    # -<hash> inside a commit worktree.
    eval "$("$PKG/name.sh" "$SRCDIR")"
elif [ -n "$PROV_STATUS" ]; then
    # Unanchored in-place build. Signing without --allow-dirty fails;
    # unsigned runs and --allow-dirty only warn — `-unsigned`/`-hash`/
    # `-dirty` names are already non-canonical, and a dirty tree may be
    # the point of a test build.
    if [ -n "$CODESIGN_IDENTITY$PKG_SIGN_IDENTITY" ] && [ -z "$ALLOW_DIRTY" ]; then
        echo "provenance: $PROV_STATUS" >&2
        echo "  build from the tagged commit, or pass --allow-dirty for a test build" >&2
        exit 1
    fi
    if [ -n "$CODESIGN_IDENTITY$PKG_SIGN_IDENTITY" ]; then
        echo "provenance: $PROV_STATUS — signing an unanchored test build" >&2
    else
        echo "  note: provenance: $PROV_STATUS — unsigned build continues" >&2
    fi
fi
orig_head=$(git -C "$SRCDIR" rev-parse HEAD)
orig_status=$PROV_STATUS

export PKG_VERSION PROV_SUFFIX

# Prefer the source dir's own helpers — a worktree build uses the
# tag's tooling; falls back to this checkout's for tags that predate
# them (and resolves to the same files when SRCDIR is this checkout).
helper() {
    h=$SRCDIR/scripts/pkg/$1.sh
    [ -x "$h" ] || h=$PKG/$1.sh
    shift
    "$h" "$@"
}

if [ -z "$SKIP_BUILD" ]; then
    helper build "$SRCDIR"
elif [ ! -x "$OUT/bin/container-distro" ]; then
    echo "--skip-build needs $OUT/bin/container-distro from a prior run" >&2
    exit 1
fi
helper collect --src "$SRCDIR" --out "$OUT"

# Re-check before bundling signs anything: a checkout could have moved
# mid-build — compare HEAD, version, and status against what we started
# with (a worktree shouldn't move, but the check is uniform). The HEAD
# compare also catches a move to a different clean tag, which
# PROV_STATUS alone would miss. Bail leaves OUT as-is for inspection;
# nothing is signed yet.
pv_head=$(git -C "$SRCDIR" rev-parse HEAD)
eval "$("$PKG/name.sh" "$SRCDIR")"
pv_msg=
[ "$pv_head" = "$orig_head" ] ||
    pv_msg="checkout moved mid-build ($orig_head -> $pv_head)"
[ "$PKG_VERSION" = "$orig_version" ] ||
    pv_msg="${pv_msg:+$pv_msg; }version moved mid-build ($orig_version -> $PKG_VERSION)"
[ "$PROV_STATUS" = "$orig_status" ] ||
    pv_msg="${pv_msg:+$pv_msg; }$PROV_STATUS"
[ -z "$pv_msg" ] || {
    echo "provenance: $pv_msg" >&2
    exit 1
}

# Done with the source — bundling works on OUT alone.
[ "$SRCDIR" = . ] || "$PKG/worktree.sh" rm "$SRCDIR"

"$PKG/bundle.sh" --out "$OUT"
