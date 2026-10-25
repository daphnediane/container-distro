#!/bin/sh
# Collect everything the bundling step needs from --src DIR into
# --out DIR: the compiled release binary (to bin/), the CycloneDX
# SBOM (when cargo cyclonedx is installed), and the payload/doc
# assets — licenses, installer scripts, productbuild resources —
# under collect/. After this step the source checkout is no longer
# needed: a build worktree can be removed, and signing happens on
# the collected copies.
#
#   collect.sh --src DIR --out DIR
#
# Artifact naming comes from $PKG_VERSION/$PROV_SUFFIX when set
# (the caller's pkg/name.sh), else computed here.
set -eu

step() { printf '\n==> %s\n' "$*"; }
HERE=$(cd "$(dirname "$0")" && pwd)

src=
out=
while [ $# -gt 0 ]; do
    case "$1" in
        --src) src=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        *) echo "usage: $0 --src DIR --out DIR" >&2; exit 2 ;;
    esac
done
[ -n "$src" ] && [ -n "$out" ] || { echo "usage: $0 --src DIR --out DIR" >&2; exit 2; }
mkdir -p "$out/bin" "$out/collect"
OUT=$(cd "$out" && pwd)
cd "$src"

[ -n "${PKG_VERSION:-}" ] || {
    eval "$("$HERE/name.sh" .)"
}

TARGET=aarch64-apple-darwin

step "collect binary"
META=$(cargo metadata --no-deps --format-version 1)
R="$(echo "$META" | jq -r '.target_directory')/release"
[ -x "$R/container-distro" ] || {
    echo "release binary missing — run scripts/pkg/build.sh first" >&2
    exit 1
}
# Report whether the binaries carry the embedded dep tree — absent on
# a --skip-build reuse of a plain `cargo build`, or when
# cargo-auditable wasn't installed.
otool -l "$R/container-distro" | grep -q dep-v0 || \
    echo "  note: container-distro has no .dep-v0 dep tree (not built via cargo auditable)"
cp "$R/container-distro" "$OUT/bin/"

step "CycloneDX SBOM"
SBOM="container-distro-$PKG_VERSION$PROV_SUFFIX-sbom.cdx"
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

step "collect assets"
# Everything bundle.sh reads from the source tree, staged under
# collect/ so bundling is source-free.
cp LICENSE THIRD-PARTY-NOTICES.yaml "$OUT/collect/"
# Microsoft WSL's license — not a crate, so
# THIRD-PARTY-NOTICES.yaml doesn't cover it (#31).
cp vendor/microsoft/LICENSE "$OUT/collect/LICENSE-microsoft-wsl"
cp packaging/uninstall-container-distro.sh packaging/update-container-distro.sh \
    packaging/Distribution.xml packaging/welcome.html packaging/conclusion.html \
    "$OUT/collect/"
