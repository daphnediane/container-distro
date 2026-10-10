#!/bin/sh
# Maintainer reinstall from this checkout: binaries, the `container
# distro` plugin copy, and man pages. Equivalent to:
#
#   cargo install --path . && sudo container-distro install-plugin \
#     && container-distro install-man && cm --install-man
#
# `command -v` hands sudo the freshly installed binary explicitly —
# sudo's secure_path may not include $CARGO_HOME/bin, and the plugin
# copy must come from this install, not wherever PATH happens to point.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo install --path ."
cargo install --path .

step "container-distro install-plugin (sudo)"
sudo "$(command -v container-distro)" install-plugin

step "container-distro install-man"
container-distro install-man

step "cm --install-man"
cm --install-man
