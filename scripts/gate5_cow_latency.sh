#!/usr/bin/env bash
# Gate 5: COW vs regular copy latency.
#
# Creates a synthetic ~100 MB directory on the SAME filesystem as the project,
# then clones it with APFS cp -c / Linux --reflink=auto and a regular cp.
#
# Pass criterion:
#   macOS  APFS clone:    ≤ 100ms for 100 MB
#   Linux  Btrfs reflink: ≤ 100ms for 100 MB
#
# To test against the real target/ dir (requires enough free space):
#   bash scripts/gate5_cow_latency.sh --real
#
# Usage: bash scripts/gate5_cow_latency.sh [--real]

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
USE_REAL=0
[ "${1:-}" = "--real" ] && USE_REAL=1

# ── Prepare source directory ──────────────────────────────────────────────────
TMPBASE="$REPO_ROOT/.gate5-tmp"
mkdir -p "$TMPBASE"
trap 'rm -rf "$TMPBASE"' EXIT

if [ "$USE_REAL" -eq 1 ]; then
  REAL_TARGET="$REPO_ROOT/target"
  if [ ! -d "$REAL_TARGET" ]; then
    echo "ERROR: $REAL_TARGET not found. Run: cargo build"
    exit 1
  fi
  SRC="$REAL_TARGET"
  SIZE_MB=$(du -sm "$SRC" 2>/dev/null | awk '{print $1}')
  echo "Mode: real target/ (${SIZE_MB} MB) — requires ${SIZE_MB} MB free on same filesystem"
else
  # Synthetic: create ~100 MB of files (10×10 MB)
  SRC="$TMPBASE/src"
  mkdir -p "$SRC/deps" "$SRC/build"
  echo -n "Creating synthetic 100 MB test dir... "
  for i in $(seq 1 10); do
    dd if=/dev/urandom bs=1m count=10 of="$SRC/deps/lib${i}.rlib" 2>/dev/null
  done
  dd if=/dev/urandom bs=1m count=5 of="$SRC/build/output" 2>/dev/null
  SIZE_MB=$(du -sm "$SRC" 2>/dev/null | awk '{print $1}')
  echo "${SIZE_MB} MB"
fi

echo "Source: $SRC  (${SIZE_MB} MB, same APFS volume)"
echo ""

PASS=0
FAIL=0

# ── macOS APFS clone ──────────────────────────────────────────────────────────
if [[ "$(uname)" == "Darwin" ]]; then
  DST="$TMPBASE/apfs-clone"
  rm -rf "$DST"
  echo -n "macOS cp -cR (APFS clone):        "
  START=$(python3 -c "import time; print(int(time.time()*1000))")
  cp -cR "$SRC" "$DST"
  END=$(python3 -c "import time; print(int(time.time()*1000))")
  MS=$((END - START))
  THRESHOLD=100
  if [ "$MS" -le "$THRESHOLD" ]; then
    echo "${MS}ms  ✅  PASS (≤ ${THRESHOLD}ms for ${SIZE_MB} MB)"
    PASS=$((PASS + 1))
  else
    echo "${MS}ms  ❌  FAIL (> ${THRESHOLD}ms — may not be on APFS, or cross-volume)"
    FAIL=$((FAIL + 1))
  fi
fi

# ── Linux reflink ─────────────────────────────────────────────────────────────
if [[ "$(uname)" == "Linux" ]]; then
  DST="$TMPBASE/reflink"
  rm -rf "$DST"
  echo -n "Linux cp --reflink=auto (Btrfs):  "
  START=$(date +%s%3N)
  cp --reflink=auto -a "$SRC" "$DST"
  END=$(date +%s%3N)
  MS=$((END - START))
  THRESHOLD=100
  if [ "$MS" -le "$THRESHOLD" ]; then
    echo "${MS}ms  ✅  PASS (≤ ${THRESHOLD}ms for ${SIZE_MB} MB)"
    PASS=$((PASS + 1))
  else
    echo "${MS}ms  ⚠   SLOW (> ${THRESHOLD}ms — may not be Btrfs/XFS; copy_build_cache falls back gracefully)"
    FAIL=$((FAIL + 1))
  fi
fi

# ── Baseline: regular cp (no COW) ────────────────────────────────────────────
DST_REG="$TMPBASE/regular"
rm -rf "$DST_REG"
echo -n "Baseline regular cp -r:            "
START_R=$(python3 -c "import time; print(int(time.time()*1000))" 2>/dev/null || date +%s%3N)
cp -r "$SRC" "$DST_REG"
END_R=$(python3 -c "import time; print(int(time.time()*1000))" 2>/dev/null || date +%s%3N)
MS_REG=$((END_R - START_R))
echo "${MS_REG}ms  (baseline — no pass/fail)"

echo ""
echo "─────────────────────────────────────────"
if [ "$FAIL" -eq 0 ] && [ "$PASS" -gt 0 ]; then
  echo "GATE 5: ✅ PASS"
else
  echo "GATE 5: ❌ FAIL  (pass=$PASS fail=$FAIL)"
  exit 1
fi
