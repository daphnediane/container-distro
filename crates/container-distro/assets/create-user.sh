#!/bin/sh
# Copyright (c) 2026 Daphne Pfister
# SPDX-License-Identifier: BSD-2-Clause
#
# First-time account setup for `container distro`: creates CONTAINER_USER
# with CONTAINER_UID/CONTAINER_GID and home CONTAINER_HOME, populated from
# /etc/skel, with passwordless sudo (and doas, if present).
#
# Edits /etc/passwd, /etc/group and /etc/shadow directly so it works on
# images without useradd/adduser. Safe to run more than once.

set -e

: "${CONTAINER_USER:?}" "${CONTAINER_UID:?}" "${CONTAINER_GID:?}" "${CONTAINER_HOME:?}"
shell=${CONTAINER_SHELL:-/bin/sh}

# has_field FILE FIELD VALUE
has_field() {
    [ -f "$1" ] && cut -d: -f"$2" "$1" | grep -qx "$3"
}

if ! has_field /etc/group 3 "$CONTAINER_GID"; then
    group=$CONTAINER_USER
    if has_field /etc/group 1 "$group"; then
        group="$CONTAINER_USER-$CONTAINER_GID"
    fi
    echo "$group:x:$CONTAINER_GID:" >>/etc/group
fi

if has_field /etc/passwd 3 "$CONTAINER_UID"; then
    : # uid already provisioned
elif has_field /etc/passwd 1 "$CONTAINER_USER"; then
    echo "create-user: user $CONTAINER_USER exists with a different uid; leaving it alone" >&2
else
    echo "$CONTAINER_USER:x:$CONTAINER_UID:$CONTAINER_GID::$CONTAINER_HOME:$shell" >>/etc/passwd
    if [ -f /etc/shadow ]; then
        echo "$CONTAINER_USER:!:19000:0:99999:7:::" >>/etc/shadow
    fi
fi

if [ ! -d "$CONTAINER_HOME" ]; then
    mkdir -p "$CONTAINER_HOME"
    if [ -d /etc/skel ]; then
        cp -a /etc/skel/. "$CONTAINER_HOME"/
    fi
    chown -R "$CONTAINER_UID:$CONTAINER_GID" "$CONTAINER_HOME"
fi

mkdir -p /etc/sudoers.d
sudoers=/etc/sudoers.d/$(echo "$CONTAINER_USER" | tr '.' '_')
echo "$CONTAINER_USER ALL=(ALL) NOPASSWD:ALL" >"$sudoers"
chmod 440 "$sudoers"

if [ -d /etc/doas.d ] || command -v doas >/dev/null 2>&1; then
    mkdir -p /etc/doas.d
    echo "permit nopass $CONTAINER_USER" >/etc/doas.d/wsl-compat.conf
fi
