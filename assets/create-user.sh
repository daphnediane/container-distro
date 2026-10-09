#!/bin/sh
# Copyright (c) 2026 Daphne Pfister
# SPDX-License-Identifier: BSD-2-Clause
#
# First-time account setup for `container distro`: creates CONTAINER_USER
# with CONTAINER_UID/CONTAINER_GID and home CONTAINER_HOME, populated from
# /etc/skel. Privilege grants (passwordless sudo/doas) live in the
# separate grant-admin.sh, which `init -u` runs next when it is mounted —
# restricted distros mount an init-assets directory without it.
#
# Edits /etc/passwd, /etc/group and /etc/shadow directly so it works on
# images without useradd/adduser. Provisions each user once: the
# /etc/.distro.user.<name> sentinel means later `init -u` runs are
# no-ops, so admin edits — sudoers removal, shell changes, even
# deleting the account — are never reverted. Remove the sentinel to
# force re-provisioning. A changed CONTAINER_USER has its own sentinel
# and is still provisioned.

set -e

: "${CONTAINER_USER:?}" "${CONTAINER_UID:?}" "${CONTAINER_GID:?}" "${CONTAINER_HOME:?}"
shell=${CONTAINER_SHELL:-/bin/sh}

# These values are interpolated into root-run file edits below —
# /etc/passwd lines, a sudoers.d filename, mkdir/chown paths — so hold
# them to safe shapes before writing anything.
case $CONTAINER_USER in
"" | -* | .* | *[!a-zA-Z0-9._-]*)
    echo "create-user: unsafe CONTAINER_USER: $CONTAINER_USER" >&2
    exit 1
    ;;
esac
case $CONTAINER_UID$CONTAINER_GID in
*[!0-9]* | "")
    echo "create-user: non-numeric CONTAINER_UID/CONTAINER_GID" >&2
    exit 1
    ;;
esac
case $CONTAINER_HOME in
"" | [!/]* | *[!a-zA-Z0-9._/-]* | */../* | */.. | ../* | ..)
    echo "create-user: unsafe CONTAINER_HOME: $CONTAINER_HOME" >&2
    exit 1
    ;;
esac

# The per-user sentinel is the provisioned marker; checks below only
# guard partial re-runs after a mid-provision failure.
safe_user=$(echo "$CONTAINER_USER" | tr '.' '_')
sentinel=/etc/.distro.user.$safe_user
[ -f "$sentinel" ] && exit 0

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

# Written last: a failed provision retries on the next boot.
echo "$CONTAINER_USER:$CONTAINER_UID:$CONTAINER_GID" >"$sentinel"
