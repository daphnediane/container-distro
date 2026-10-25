#!/bin/sh
# Compile the release binaries in DIR — pure build step, nothing
# copied out (pkg/collect.sh gathers outputs afterward). Prefers
# `cargo auditable` so binaries embed their dep tree (`cargo audit
# bin` can scan an installed copy — interim until native cargo SBOM
# embedding lands). To reuse an existing build, don't invoke this at
# all — that's the orchestrator's --skip-build.
set -eu

dir=${1:?"usage: $0 DIR"}
cd "$dir"

step() { printf '\n==> %s\n' "$*"; }

if command -v cargo-auditable >/dev/null; then
    step "cargo auditable build --release"
    cargo auditable build --release
else
    step "cargo build --release"
    cargo build --release
    echo "  (no cargo auditable — binaries won't embed their dep tree; recommended for releases)"
fi
