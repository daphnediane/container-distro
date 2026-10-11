#!/bin/sh
# Upload the signed release artifacts (scripts/build-pkg-signed.sh
# output under scratch/pkg/) over the GitHub draft release for a tag,
# replacing the unsigned build CI attached. Usage:
#
#   scripts/upload-release-artifacts.sh vX.Y.Z
set -eu
cd "$(dirname "$0")/.."

tag=${1:?"usage: $0 vX.Y.Z"}
version=${tag#v}
pkg="scratch/pkg/container-distro-$version.pkg"
tarball="scratch/pkg/container-distro-aarch64-apple-darwin-v$version.tar.gz"

[ -f "$pkg" ] || { echo "missing $pkg — run scripts/build-pkg-signed.sh first" >&2; exit 1; }
[ -f "$tarball" ] || { echo "missing $tarball — run scripts/build-pkg-signed.sh first" >&2; exit 1; }
pkgutil --check-signature "$pkg" | grep -q "Developer ID Installer" || {
    echo "$pkg is not Developer-ID signed — refusing to upload it as the signed asset" >&2
    exit 1
}

gh release upload "$tag" --clobber "$pkg" "$tarball"
# The CI-built unsigned pkg has no place on a signed release.
gh release delete-asset "$tag" "container-distro-$version-unsigned.pkg" -y 2>/dev/null || true
