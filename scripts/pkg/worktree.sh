#!/bin/sh
# Manage build worktrees from the repo root:
#
#   worktree.sh add PATH REF   detached worktree of REF at PATH
#                              (cleans any stale one first), prints PATH
#   worktree.sh rm  PATH       remove the worktree (or stale dir)
#
# Signing builds run inside a worktree of the release tag so the source
# can't move mid-build (#50).
set -eu
cd "$(dirname "$0")/../.."

cmd=${1:?"usage: $0 add PATH REF | rm PATH"}
case "$cmd" in
    add)
        path=${2:?usage: $0 add PATH REF}
        ref=${3:?usage: $0 add PATH REF}
        git worktree remove --force "$path" 2>/dev/null || {
            rm -rf "$path"
            git worktree prune
        }
        git worktree add --detach "$path" "$ref"
        printf '%s\n' "$path"
        ;;
    rm)
        path=${2:?usage: $0 rm PATH}
        git worktree remove --force "$path" 2>/dev/null || {
            rm -rf "$path"
            git worktree prune
        }
        ;;
    *)
        echo "usage: $0 add PATH REF | rm PATH" >&2
        exit 2
        ;;
esac
