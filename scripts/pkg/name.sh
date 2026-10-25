#!/bin/sh
# Print eval-able naming vars for the checkout at $1 (default .):
#
#   PKG_VERSION   Cargo.toml package version
#   PROV_SUFFIX   artifact-name suffix: "-<hash>" when HEAD isn't the
#                 matching tag plus "-dirty" on a dirty tree (empty at
#                 tag on a clean tree) — applied ahead of "-unsigned"
#   PROV_STATUS   human-readable reason the checkout isn't anchored,
#                 empty when it is exactly v<version> and clean
#
# The suffix means a canonical artifact name always denotes tag
# content (#50); unanchored builds can't masquerade.
set -eu
cd "${1:-.}"

PKG_VERSION=$(cargo metadata --no-deps --format-version 1 |
    jq -r '.packages[0].version')
tag=$(git describe --exact-match --tags HEAD 2>/dev/null || true)
suffix=
status=
if [ "$tag" != "v$PKG_VERSION" ]; then
    suffix="-$(git rev-parse --short HEAD)"
    status="HEAD is not at the v$PKG_VERSION tag (found: ${tag:-none})"
fi
if [ -n "$(git status --porcelain)" ]; then
    suffix="$suffix-dirty"
    [ -z "$status" ] || status="$status; "
    status="${status}working tree is dirty"
fi

# Shell-quote each value so `eval "$(name.sh DIR)"` is safe.
shq() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
printf 'PKG_VERSION=%s\nPROV_SUFFIX=%s\nPROV_STATUS=%s\n' \
    "$(shq "$PKG_VERSION")" "$(shq "$suffix")" "$(shq "$status")"
