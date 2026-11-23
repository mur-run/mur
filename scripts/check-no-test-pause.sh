#!/bin/sh
# Fail the release if a shipped binary was built with mur-common's `test-pause`
# feature (spec v3.12 review). That feature lets a file in
# $MUR_TEST_PAUSE_DIR stall a process while it holds the lifecycle lock, so it
# must never reach users.
#
# Usage: scripts/check-no-test-pause.sh <binary>...
#
# Two independent checks:
#   1. each binary is searched for the marker bytes that mur-common embeds when
#      the feature is on (`mur_common::test_pause::BUILD_MARKER`; a unit test
#      keeps this string in step with it);
#   2. the release dependency graph must not enable the feature at all.
set -eu

MARKER='MUR_BUILT_WITH_TEST_PAUSE_FEATURE'
FEATURE='test-pause'

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <binary>..." >&2
    exit 2
fi

status=0
for bin in "$@"; do
    if [ ! -f "$bin" ]; then
        echo "check-no-test-pause: not a file: $bin" >&2
        status=1
        continue
    fi
    if grep -aqF "$MARKER" "$bin"; then
        echo "check-no-test-pause: $bin was built with the $FEATURE feature" >&2
        status=1
    fi
done

# The inverted tree (`-i mur-common`) is the one that prints a
# `mur-common feature "<name>"` line for every feature that is switched on.
# Capture it in a file first: a pipe would hide a cargo failure behind grep's
# exit status, and a failed check must fail the release, not pass it.
tree_out="$(mktemp)"
trap 'rm -f "$tree_out"' EXIT
if ! cargo tree --workspace --locked --target all -e features --prefix none \
    -i mur-common >"$tree_out"; then
    echo "check-no-test-pause: cargo tree failed; cannot tell whether the workspace enables mur-common/$FEATURE" >&2
    exit 1
fi
if ! grep -qF 'mur-common feature "default"' "$tree_out"; then
    echo "check-no-test-pause: unexpected cargo tree output (no mur-common feature lines)" >&2
    exit 1
fi
if grep -qF "mur-common feature \"$FEATURE\"" "$tree_out"; then
    echo "check-no-test-pause: the workspace enables mur-common/$FEATURE" >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "check-no-test-pause: ok ($# binaries)"
fi
exit "$status"
