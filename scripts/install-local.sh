#!/bin/sh
# Maintainer reinstall from this checkout: binaries, the `container
# distro` plugin copy, and man pages. Equivalent to:
#
#   cargo install --path . && scripts/build-distrofs.sh --install ~/.cargo \
#     && container-distro install-plugin \
#     && container-distro install-man && cm --install-man
#
# install-plugin self-elevates (sudo re-exec) for the root-owned plugin
# dir, so no sudo prefix is needed here.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo install --path ."
cargo install --path .

# The guest file server goes beside the cargo-installed binaries
# (<root>/libexec/container-distro/), where they and — via
# install-plugin — the plugin copy look for it.
step "scripts/build-distrofs.sh --install"
scripts/build-distrofs.sh --install "${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"

step "container-distro install-plugin"
container-distro install-plugin

step "container-distro install-man"
container-distro install-man

step "cm --install-man"
cm --install-man
