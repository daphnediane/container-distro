#!/bin/sh
# Copyright (c) 2026 Daphne Pfister
# SPDX-License-Identifier: BSD-2-Clause
#
# Privilege grant for `container distro`: passwordless sudo (and doas,
# if present) for CONTAINER_USER. Mounted only in distros that allow
# admin rights — restricted distros get an init-assets directory that
# does not contain this script, so no privilege-granting code exists
# inside their guests.
#
# The grant happens once per user and only when the container was armed
# with CONTAINER_ADMIN=1 at create: the /etc/.distro.admin.<name>
# sentinel makes later `init -u` runs no-ops, so admin edits — sudoers
# removal, a different policy — are never reverted. Without the env the
# decision is deferred to a later boot; `container distro set --sudo`
# re-arms it, and removing the sentinel forces re-provisioning.
# Privilege files are only created when absent either way.

set -e

: "${CONTAINER_USER:?}"

# Interpolated into a sudoers.d filename and file contents below —
# the same charset create-user.sh enforces.
case $CONTAINER_USER in
"" | -* | .* | *[!a-zA-Z0-9._-]*)
    echo "grant-admin: unsafe CONTAINER_USER: $CONTAINER_USER" >&2
    exit 1
    ;;
esac

safe_user=$(echo "$CONTAINER_USER" | tr '.' '_')
sentinel=/etc/.distro.admin.$safe_user
[ -f "$sentinel" ] && exit 0
[ "${CONTAINER_ADMIN:-0}" = 1 ] || exit 0

mkdir -p /etc/sudoers.d
sudoers=/etc/sudoers.d/$safe_user
if [ ! -e "$sudoers" ]; then
    echo "$CONTAINER_USER ALL=(ALL) NOPASSWD:ALL" >"$sudoers"
    chmod 440 "$sudoers"
fi

if [ -d /etc/doas.d ] || command -v doas >/dev/null 2>&1; then
    mkdir -p /etc/doas.d
    doas=/etc/doas.d/$safe_user.conf
    [ -e "$doas" ] || echo "permit nopass $CONTAINER_USER" >"$doas"
fi

echo "$CONTAINER_USER" >"$sentinel"
