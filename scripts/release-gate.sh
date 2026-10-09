#!/bin/sh
# Pre-release gate: everything doc/release.md step 1 requires, in one
# command (#16). Exits nonzero on the first failure.
# PROPTEST_CASES=<n> deepens the parser-fuzz pass inside `cargo test`.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --all -- --check

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test --workspace (PROPTEST_CASES=${PROPTEST_CASES:-default})"
cargo test --workspace

step "init assets syntax"
sh -n assets/*

step "cargo audit"
cargo audit

step "cargo deny check"
cargo deny check

step "THIRD-PARTY-NOTICES.yaml freshness"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
cargo bundle-licenses -f yaml -o "$tmp"
if ! diff -q THIRD-PARTY-NOTICES.yaml "$tmp" >/dev/null; then
    echo "THIRD-PARTY-NOTICES.yaml is stale — regenerate:"
    echo "  cargo bundle-licenses -f yaml -o THIRD-PARTY-NOTICES.yaml"
    exit 1
fi

printf '\nrelease gate: all checks passed\n'
