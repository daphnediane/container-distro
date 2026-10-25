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

# Signed uploads add assets to a draft; a published release's assets
# must never be swapped — people may hold their bytes already (#50).
gh release view "$tag" --json isDraft --jq .isDraft | grep -qx true || {
    echo "$tag is not a draft release — refusing to modify it" >&2
    exit 1
}
pkg="scratch/pkg/container-distro-$version.pkg"
tarball="scratch/pkg/container-distro-aarch64-apple-darwin-v$version.tar.gz"
sig="$tarball.sig"
asc="$tarball.asc"

[ -f "$pkg" ] || { echo "missing $pkg — run scripts/build-pkg-signed.sh first" >&2; exit 1; }
[ -f "$tarball" ] || { echo "missing $tarball — run scripts/build-pkg-signed.sh first" >&2; exit 1; }
[ -f "$sig" ] || { echo "missing $sig — run scripts/build-pkg-signed.sh first" >&2; exit 1; }
pkgutil --check-signature "$pkg" | grep -q "Developer ID Installer" || {
    echo "$pkg is not Developer-ID signed — refusing to upload it as the signed asset" >&2
    exit 1
}
codesign --verify --detached="$sig" "$tarball" || {
    echo "$sig does not verify against $tarball — refusing to upload it" >&2
    exit 1
}
if [ -f "$asc" ]; then
    gpg --batch --verify "$asc" "$tarball" || {
        echo "$asc does not verify against $tarball — refusing to upload it" >&2
        exit 1
    }
fi

files="$pkg $tarball $sig"
[ -f "$asc" ] && files="$files $asc"
gh release upload "$tag" --clobber $files
# The CI-built unsigned artifacts have no place on a signed release.
gh release delete-asset "$tag" "container-distro-$version-unsigned.pkg" -y 2>/dev/null || true
gh release delete-asset "$tag" "container-distro-aarch64-apple-darwin-v$version-unsigned.tar.gz" -y 2>/dev/null || true
