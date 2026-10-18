#!/bin/sh
# Refresh vendor/microsoft/ to the latest upstream catalog (#31):
# resolve the newest commit touching distributions/DistributionInfo.json,
# vendor that exact revision + LICENSE, and pin it in UPSTREAM.
# Needs curl, jq, and network access to GitHub.
set -eu
cd "$(dirname "$0")/.."

VENDOR=vendor/microsoft
UPSTREAM_FILE="$VENDOR/UPSTREAM"
repo=$(sed -n 's/^repo: *//p' "$UPSTREAM_FILE")
path=$(sed -n 's/^path: *//p' "$UPSTREAM_FILE")
[ -n "$repo" ] && [ -n "$path" ] || {
    echo "$UPSTREAM_FILE: need repo:/path: lines" >&2
    exit 1
}

latest=$(curl -fsSL \
    "https://api.github.com/repos/$repo/commits?path=$path&per_page=1" |
    jq -r '.[0].sha')
[ -n "$latest" ] && [ "$latest" != null ] || {
    echo "could not resolve latest commit for $repo/$path" >&2
    exit 1
}

raw="https://raw.githubusercontent.com/$repo"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$raw/$latest/$path" -o "$tmp/DistributionInfo.json"
curl -fsSL "$raw/$latest/LICENSE" -o "$tmp/LICENSE"

# Sanity: it must still parse as our catalog schema before we vendor it.
if ! python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
assert isinstance(d.get("ModernDistributions"), dict), "no ModernDistributions"
' "$tmp/DistributionInfo.json"; then
    echo "downloaded file does not look like a DistributionInfo.json" >&2
    exit 1
fi

mv "$tmp/DistributionInfo.json" "$VENDOR/DistributionInfo.json"
mv "$tmp/LICENSE" "$VENDOR/LICENSE"
cat > "$UPSTREAM_FILE" <<EOF
repo: $repo
path: $path
commit: $latest
EOF
echo "vendored $repo/$path @ $latest"
echo "review: git diff $VENDOR"
