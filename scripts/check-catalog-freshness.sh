#!/bin/sh
# Catalog freshness gate (#31): verify the vendored Microsoft manifest
# in vendor/microsoft/ is (a) byte-identical to the upstream file at the
# pinned commit and (b) still the latest upstream revision. Drift fails
# the release gate — refresh with scripts/update-microsoft-catalog.sh.
# Needs curl, jq, and network access to GitHub.
set -eu
cd "$(dirname "$0")/.."

VENDOR=vendor/microsoft
UPSTREAM_FILE="$VENDOR/UPSTREAM"
[ -f "$UPSTREAM_FILE" ] || { echo "missing $UPSTREAM_FILE" >&2; exit 1; }

repo=$(sed -n 's/^repo: *//p' "$UPSTREAM_FILE")
path=$(sed -n 's/^path: *//p' "$UPSTREAM_FILE")
pinned=$(sed -n 's/^commit: *//p' "$UPSTREAM_FILE")
[ -n "$repo" ] && [ -n "$path" ] && [ -n "$pinned" ] || {
    echo "$UPSTREAM_FILE: need repo:/path:/commit: lines" >&2
    exit 1
}

raw="https://raw.githubusercontent.com/$repo"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Integrity: the vendored copy must be verbatim upstream — a hand edit
# would otherwise pass the freshness half of the check below.
step="vendored file matches upstream@$pinned"
curl -fsSL "$raw/$pinned/$path" -o "$tmp/upstream.json"
if ! diff -q "$VENDOR/DistributionInfo.json" "$tmp/upstream.json" >/dev/null; then
    echo "FAIL: $VENDOR/DistributionInfo.json differs from upstream@$pinned" >&2
    echo "      restore it with scripts/update-microsoft-catalog.sh" >&2
    exit 1
fi
curl -fsSL "$raw/$pinned/LICENSE" -o "$tmp/LICENSE"
if ! diff -q "$VENDOR/LICENSE" "$tmp/LICENSE" >/dev/null; then
    echo "FAIL: $VENDOR/LICENSE differs from upstream@$pinned" >&2
    exit 1
fi
echo "ok: $step"

# Freshness: has a newer commit touched the manifest since the pin?
latest=$(curl -fsSL \
    "https://api.github.com/repos/$repo/commits?path=$path&per_page=1" |
    jq -r '.[0].sha')
if [ "$latest" = "$pinned" ]; then
    echo "ok: manifest is current upstream ($pinned)"
    exit 0
fi
curl -fsSL "$raw/$latest/$path" -o "$tmp/latest.json"
if diff -q "$VENDOR/DistributionInfo.json" "$tmp/latest.json" >/dev/null; then
    echo "ok: upstream moved to $latest but the manifest is unchanged"
    echo "    (bump commit: in $UPSTREAM_FILE to silence this note)"
    exit 0
fi
echo "FAIL: upstream manifest updated ($pinned -> $latest)" >&2
echo "      run scripts/update-microsoft-catalog.sh and review the diff" >&2
exit 1
