#!/bin/sh
# Update container-distro to the latest release (or VERSION), modeled
# on Apple's update-container.sh but a thin wrapper — the installed
# binary does the release lookup, verification (API-reported SHA-256,
# Developer ID signature, same-team check), and the Installer.app
# handoff itself. Shipped in the pkg's bin/ so the package layout
# carries an updater like Apple's does. Nothing needs stopping first:
# cm/container-distro are per-invocation binaries, not daemons.
#
#   update-container-distro.sh [VERSION] [--force]
#
# VERSION accepts 0.5.0 or v0.5.0; a downgrade asks first. Non-pkg
# (cargo) installs update via `cargo install --locked container-distro`
# or `cargo binstall` instead.
set -u

CD=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/container-distro
if [ ! -x "$CD" ]; then
    echo "container-distro not found next to $0" >&2
    exit 1
fi
exec "$CD" update "$@"
