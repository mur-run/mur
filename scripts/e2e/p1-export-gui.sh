#!/usr/bin/env bash
#
# P1.9 — End-to-end smoke for `mur agent export --format gui`.
#
# This script exercises the orchestration layer of the export pipeline.
# On a host without tauri-cli installed, the doctor + export both fail
# fast at the prereq phase — that IS the test (the pipeline correctly
# refuses to attempt a build it can't complete). On a fully-equipped CI
# host, the full pipeline runs and produces an artifact.
#
# Exit code 0  → orchestration verified (either fail-fast OR full build)
# Exit code !0 → unexpected failure
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

echo "▸ Building mur CLI"
cargo build -p mur-core --bin mur >/dev/null

MUR=./target/debug/mur

# ── 1. Doctor reports correct shape ────────────────────────────────
echo "▸ Doctor (gui)"
DOCTOR_OUT="$($MUR agent doctor --format gui --json 2>/dev/null || true)"
echo "$DOCTOR_OUT" | python3 -c '
import json, sys
data = json.load(sys.stdin)
names = {r["name"] for r in data}
expected = {"cargo", "rustc", "host-target", "node", "npm", "tauri-cli"}
missing = expected - names
assert not missing, f"missing checks in doctor output: {missing}"
print("  ✓ doctor returns expected check shape")
'

# ── 2. Help shows GUI flags ────────────────────────────────────────
echo "▸ Help"
HELP="$($MUR agent export --help 2>&1)"
for flag in --theme --icon --clone-identity --skip-notarize; do
  echo "$HELP" | grep -- -qF "$flag" >/dev/null 2>&1 || \
    echo "$HELP" | grep -qF -- "$flag" || \
    { echo "MISSING $flag in --help"; exit 1; }
done
echo "  ✓ all GUI flags exposed via --help"

# ── 3. Create a throwaway agent + attempt export ───────────────────
TMPHOME="$(mktemp -d)"
trap 'rm -rf "$TMPHOME"' EXIT
echo "▸ Throwaway agent at MUR_HOME=$TMPHOME"
export MUR_HOME="$TMPHOME"

$MUR agent create p1-9-test-agent --no-interactive --display-name "p1.9 test" --model llama3.2:3b >/dev/null

OUT_PATH="$TMPHOME/MyAgent.app"
EXPORT_OUT="$($MUR agent export p1-9-test-agent -o "$OUT_PATH" --format gui --theme dark --skip-notarize 2>&1 || true)"
echo "$EXPORT_OUT" | head -20

if echo "$EXPORT_OUT" | grep -q "missing prerequisites for gui export"; then
  echo "  ✓ pipeline correctly fail-fast at prereq phase (tauri-cli not installed)"
elif [ -e "$OUT_PATH" ]; then
  echo "  ✓ pipeline produced artifact at $OUT_PATH"
  ls -la "$OUT_PATH"
else
  echo "  ✗ unexpected: pipeline did not fail-fast and did not produce artifact"
  exit 2
fi

echo
echo "▸ P1.9 smoke OK"
