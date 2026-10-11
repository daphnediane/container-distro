#!/bin/sh
# Build a fake GitHub-releases tree for testing `cm --update` /
# `container distro update` without published releases (DEV.md
# "Testing self-update"). The update code only ever reads
# <base>/releases/latest, <base>/releases/tags/vX.Y.Z, and each
# asset's browser_download_url — so a static file server over this
# tree impersonates api.github.com end to end.
#
# Each PKG argument becomes a release asset; the tag comes from the
# filename — container-distro-0.5.0.pkg → v0.5.0,
# container-distro-0.4.5-unsigned.pkg → v0.4.5 — and files sharing a
# tag land in the same manifest. The real sha256 goes into each
# asset's `digest` field.
#
#   scripts/make-fake-releases.sh [-o OUTDIR] [-p PORT] [-l TAG]
#                                 [--bad-digest] PKG...
#
#   -o OUTDIR      output tree (default scratch/fake-gh)
#   -p PORT        port the download URLs point at (default 8123)
#   -l, --latest   also serve TAG as releases/latest
#                  (default: the last PKG's tag)
#   --bad-digest   corrupt every asset digest (tamper-detection test)
#
# Then:
#   python3 -m http.server 8123 -d scratch/fake-gh &
#   CONTAINER_DISTRO_RELEASES_URL=http://127.0.0.1:8123 \
#       cm --update --dry-run --force --dev
set -eu
cd "$(dirname "$0")/.."

OUT=scratch/fake-gh
PORT=8123
LATEST=
BAD=
while [ $# -gt 0 ]; do
    case "$1" in
        -o) OUT=$2; shift 2 ;;
        -p) PORT=$2; shift 2 ;;
        -l | --latest) LATEST=$2; shift 2 ;;
        --bad-digest) BAD=1; shift ;;
        *) break ;;
    esac
done
[ $# -ge 1 ] || {
    echo "usage: $0 [-o OUTDIR] [-p PORT] [-l TAG] [--bad-digest] PKG..." >&2
    exit 2
}

rm -rf "$OUT"
mkdir -p "$OUT/releases/tags"

# Stage every pkg under download/<tag>/ — a filename that doesn't match
# the release-asset shape is a usage error, not a weird release.
for pkg in "$@"; do
    base=$(basename "$pkg")
    case $base in
        container-distro-*-unsigned.pkg | container-distro-*.pkg) ;;
        *) echo "$base: not a container-distro-<ver>.pkg filename" >&2; exit 2 ;;
    esac
    ver=${base#container-distro-}
    ver=${ver%.pkg}
    ver=${ver%-unsigned}
    tag="v$ver"
    mkdir -p "$OUT/download/$tag"
    cp "$pkg" "$OUT/download/$tag/$base"
    LAST=$tag
done

# One manifest per tag dir, listing every staged asset.
for dir in "$OUT/download"/*/; do
    tag=$(basename "$dir")
    {
        printf '{"tag_name":"%s","assets":[' "$tag"
        first=1
        for f in "$dir"*; do
            n=$(basename "$f")
            d=$(shasum -a 256 "$f" | cut -d' ' -f1)
            if [ -n "$BAD" ]; then
                d=0000000000000000000000000000000000000000000000000000000000000000
            fi
            [ "$first" = 1 ] || printf ','
            first=0
            printf '{"name":"%s","browser_download_url":"http://127.0.0.1:%s/download/%s/%s","digest":"sha256:%s"}' \
                "$n" "$PORT" "$tag" "$n" "$d"
        done
        printf ']}'
    } > "$OUT/releases/tags/$tag"
done

LATEST=${LATEST:-$LAST}
cp "$OUT/releases/tags/$LATEST" "$OUT/releases/latest"

find "$OUT" -type f | sort
echo "serve with: python3 -m http.server $PORT -d $OUT"
