#!/bin/sh
# Manage build worktrees from the repo root:
#
#   worktree.sh add PATH REF   detached worktree of REF at PATH
#                              (cleans any stale one first)
#   worktree.sh rm  PATH       remove the worktree (or stale dir)
#
# PATH must be under scratch/ — relative to the repo root, or absolute
# into this checkout — with no `..`. Cleanup only touches PATH itself — never
# `git worktree prune`, which would drop every other worktree whose
# directory is missing (e.g. on an unmounted volume).
#
# Signing builds run inside a worktree of the release tag so the source
# can't move mid-build (#50).
set -eu
cd "$(dirname "$0")/../.."

usage() { echo "usage: $0 add PATH REF | rm PATH" >&2; exit 2; }

# Is PATH a registered worktree? `git worktree list --porcelain`
# prints absolute physical paths, so compare against one — the dir
# itself may be gone, so resolve via the repo root instead.
registered() {
    git worktree list --porcelain | grep -Fqx "worktree $(pwd -P)/$1"
}

# Remove PATH if it's present: a registered worktree via git (doubled
# --force also covers a locked one), a leftover unregistered dir via
# rm. A registered-but-missing worktree has nothing on disk — `add
# --force` reuses its entry.
clean() {
    if registered "$1" && [ -d "$1" ]; then
        git worktree remove --force --force "$1"
    elif [ -e "$1" ] && ! registered "$1"; then
        rm -rf "$1"
    fi
}

cmd=${1:-}
path=${2:-}
[ -n "$path" ] || usage
# An absolute PATH into this checkout (logical or physical spelling)
# becomes repo-relative, so the checks below and registered() see one
# form.
arg=$path
root=$(pwd -P)
case "$path" in
    "$root"/*) path=${path#"$root"/} ;;
    "$PWD"/*) path=${path#"$PWD"/} ;;
esac
case "$path" in
    *..*) echo "$0: PATH must not contain .. (got $arg)" >&2; exit 2 ;;
    scratch/?*) ;;
    *) echo "$0: PATH must be under scratch/ (got $arg)" >&2; exit 2 ;;
esac
case "$cmd" in
    add)
        ref=${3:-}
        [ -n "$ref" ] || usage
        clean "$path"
        git worktree add --force --detach "$path" "$ref"
        ;;
    rm)
        clean "$path"
        ;;
    *)
        usage
        ;;
esac
