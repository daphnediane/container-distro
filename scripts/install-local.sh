#!/bin/sh
# Maintainer reinstall from this checkout: binaries, the `container
# distro` plugin copy, and man pages. Equivalent to:
#
#   cargo install --path . && container-distro install-plugin \
#     && container-distro install-man && cm --install-man
#
# install-plugin self-elevates (sudo re-exec) for the root-owned plugin
# dir, so no sudo prefix is needed here.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo install --path ."
cargo install --path .

step "container-distro install-plugin"
container-distro install-plugin

step "container-distro install-man"
container-distro install-man

step "cm --install-man"
cm --install-man
