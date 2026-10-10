#!/bin/sh
# Release check (docs/design/release.md step 1): pre-journal distros
# adopt and migrate correctly under this build. Journaled state landed
# after 0.4.2, so every upgrade from <=0.4.2 exercises these paths
# exactly once -- and only against the real per-user state dir
# (~/Library/Application Support/container-distro) and the real
# `container` appRoot; there is no isolation to test them under. The
# check therefore only ever touches distro names under its own $PREFIX
# and cleans them up on exit.
#
#   A. A stopped pre-journal distro is auto-adopted on first contact:
#      journal written uchg-locked, disk and live config carried into
#      it (a sentinel file survives the first start).
#   B. A pre-journal distro created *after* the migration marker is
#      refused by ops until `cm --migrate` adopts it -- even while it
#      is still running, and without a spec-drift warning.
#   C. A legacy preserved/<name>.{json,ext4} pair (an interrupted 0.4.2
#      `set`'s staging) folds into the journal: the spec parses and the
#      staged disk becomes the distro's filesystem.
#
# Re-runnable. Env overrides:
#   OLD_VERSION  newest pre-journal release (default 0.4.2)
#   OLD_ROOT     its cargo-install root (default scratch/old-$OLD_VERSION;
#                recreate with `cargo install --locked
#                container-distro@$OLD_VERSION --root $OLD_ROOT`)
#   NEW_DIR      dir holding the new cm/container-distro (target/debug)
#   IMAGE        image for the test distros (alpine:latest)
#   PREFIX       distro-name prefix (relcheck)
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

OLD_VERSION=${OLD_VERSION:-0.4.2}
OLD_ROOT=${OLD_ROOT:-scratch/old-$OLD_VERSION}
OLDCD=$OLD_ROOT/bin/container-distro
NEW_DIR=${NEW_DIR:-target/debug}
NEWCD=$NEW_DIR/container-distro
NEWCM=$NEW_DIR/cm
IMAGE=${IMAGE:-alpine:latest}
PREFIX=${PREFIX:-relcheck}

A=$PREFIX-auto   # auto-adopted while stopped
B=$PREFIX-adopt  # explicitly adopted while running
C=$PREFIX-fold   # legacy preserved/ pair folded

STATE="$HOME/Library/Application Support/container-distro"
JOURNALS=$STATE/distros
PRES=$STATE/preserved
MARKER=$STATE/journalized
SENTINEL_PATH=/etc/relcheck-sentinel
SENTINEL=pre-journal-disk

locked() { [ "$(stat -f %Sf "$1" 2>/dev/null)" = uchg ]; }
need_locked() { locked "$1" || fail "$1 is not uchg-locked"; }

# Re-opening the migration window re-runs the pass idempotently:
# journaled names are skipped, so this only re-adopts what is still
# legitimately pre-journal.
drop_marker() {
    if [ -e "$MARKER" ]; then
        chflags nouchg "$MARKER"
        rm -f "$MARKER"
    fi
}

cleanup() {
    for n in "$A" "$B" "$C"; do
        "$NEWCD" rm -f "$n" >/dev/null 2>&1 && continue
        "$OLDCD" rm -f "$n" >/dev/null 2>&1 && continue
        container stop "$n" >/dev/null 2>&1 || true
        container delete "$n" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT

step "preflight"
[ -x "$OLDCD" ] || fail "no $OLDCD -- recreate it:
  cargo install --locked container-distro@$OLD_VERSION --root $OLD_ROOT"
"$OLDCD" --version | grep -F "$OLD_VERSION" >/dev/null ||
    fail "$OLDCD is not $OLD_VERSION -- the check needs the newest pre-journal release"
command -v container >/dev/null 2>&1 || [ -x /usr/local/bin/container ] ||
    fail "container CLI not found"
cargo build --bins
[ -x "$NEWCD" ] && [ -x "$NEWCM" ] || fail "cargo build did not produce $NEW_DIR binaries"
cleanup # leftovers from an aborted run

step "A: auto-adoption of a stopped pre-journal distro"
"$OLDCD" create -n "$A" "$IMAGE"
"$OLDCD" run --root -n "$A" -- sh -c "echo $SENTINEL >$SENTINEL_PATH"
"$OLDCD" stop "$A"
drop_marker
out=$("$NEWCD" list 2>&1) || { echo "$out"; fail "list failed"; }
echo "$out" | grep -F "adopted pre-journal distro \`$A\`" >/dev/null ||
    { echo "$out"; fail "$A was not auto-adopted"; }
[ -f "$JOURNALS/$A/spec.json" ] || fail "no journal spec for $A"
need_locked "$JOURNALS/$A"
need_locked "$JOURNALS/$A/spec.json"
need_locked "$MARKER"
"$NEWCD" run --root -n "$A" -- cat "$SENTINEL_PATH" | grep -F "$SENTINEL" >/dev/null ||
    fail "adopted $A lost its filesystem"
"$NEWCD" stop "$A"
"$NEWCD" set "$A" --cpus 2
grep -F '"cpus": 2' "$JOURNALS/$A/spec.json" >/dev/null ||
    fail "set did not re-journal $A"
"$NEWCM" -l | grep -F "$A" >/dev/null || fail "cm -l does not list adopted $A"

step "B: explicit adopt of a running pre-journal distro"
"$OLDCD" create -n "$B" "$IMAGE" # 0.4.2 create boots -- adoption lands on a live container
if refuse=$("$NEWCD" run -n "$B" -- true 2>&1); then
    fail "journal-less $B ran without adoption"
fi
echo "$refuse" | grep -F "predates journaled state" >/dev/null ||
    { echo "$refuse"; fail "wrong refusal for $B"; }
"$NEWCM" --migrate "$B" | grep -F "adopted" >/dev/null ||
    fail "cm --migrate did not adopt $B"
runout=$("$NEWCD" run -n "$B" -- true 2>&1) || { echo "$runout"; fail "adopted $B does not run"; }
echo "$runout" | grep -F "drifted" >/dev/null &&
    { echo "$runout"; fail "adopted spec drifted from the live container"; }
"$NEWCD" stop "$B"
need_locked "$JOURNALS/$B"
need_locked "$JOURNALS/$B/rootfs.ext4" # running disk folded into the journal at stop

step "C: legacy preserved/ set-staging folds into the journal"
[ -f "$JOURNALS/$A/rootfs.ext4" ] || fail "$A has no journaled disk to stage from"
# A real 0.4.2 pair is preserved/<name>.{json,ext4}, both uchg-locked;
# the .json is the spec the interrupted `set` was heading for. It is
# fabricated here from $A's journal: auto-adopted specs carry no
# `origin` field, so they are exactly the 0.4.2 DistroSpec shape. If
# DistroSpec grows a required field, stage a real pair instead (kill a
# 0.4.2 `set` mid-flight).
[ "$(grep -c "\"name\": \"$A\"" "$JOURNALS/$A/spec.json")" = 1 ] ||
    fail "$A's spec has no single name field to rewrite"
chflags nouchg "$PRES" 2>/dev/null || true
sed "s/\"name\": \"$A\"/\"name\": \"$C\"/" "$JOURNALS/$A/spec.json" >"$PRES/$C.json"
cp -c "$JOURNALS/$A/rootfs.ext4" "$PRES/$C.ext4" 2>/dev/null ||
    cp "$JOURNALS/$A/rootfs.ext4" "$PRES/$C.ext4"
chflags uchg "$PRES/$C.json" "$PRES/$C.ext4" "$PRES"
drop_marker
out=$("$NEWCD" list 2>&1) || { echo "$out"; fail "list failed"; }
echo "$out" | grep -F "folded \`$C\`'s staged filesystem" >/dev/null ||
    { echo "$out"; fail "staged pair was not folded"; }
need_locked "$JOURNALS/$C"
need_locked "$JOURNALS/$C/spec.json"
need_locked "$JOURNALS/$C/rootfs.ext4"
[ ! -e "$PRES/$C.json" ] && [ ! -e "$PRES/$C.ext4" ] ||
    fail "folded pair left behind in preserved/"
"$NEWCD" run --root -n "$C" -- cat "$SENTINEL_PATH" | grep -F "$SENTINEL" >/dev/null ||
    fail "folded $C lost the staged filesystem"
"$NEWCD" stop "$C"

printf '\nlegacy adoption check: all phases passed\n'
