#!/bin/sh
# Maintainer reinstall from this checkout: the binary, then everything
# around it — man pages, completions, the `cm` alias, the `container
# distro` plugin copy. Equivalent to:
#
#   cargo install --path . && container-distro install-all
#
# The plugin step self-elevates (sudo re-exec) for the root-owned
# plugin dir, so no sudo prefix is needed here.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo install --path ."
cargo install --path .

step "container-distro install-all"
container-distro install-all
