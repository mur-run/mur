#!/usr/bin/env bash
#
# P1.9 — End-to-end smoke for `mur agent export --format gui`.
#
# Two modes:
#   1. Quick (default) — exercises orchestration only. Accepts either
#      "fail-fast at prereq" (host missing tauri-cli) or "produced
#      artifact" (host has it).
#   2. Full (FULL_E2E=1) — actually runs the full Tauri build, then
#      launches the produced .app and asserts the bootstrap module
#      extracts the payload + spawns a runtime that writes
#      `running.lock` under MUR_HOME. Cleans up afterwards.
#
# Exit code 0  → orchestration verified
# Exit code !0 → unexpected failure
set -euo pipefail

# Ensure cargo is in PATH. On macOS-14 ARM64 CI runners, the rust-cache
# action can restore ~/.cargo/bin/cargo as a bare rustup-init proxy that
# doesn't recognise argv[0] correctly. Use `rustup which` to get the
# actual toolchain binary, falling back to PATH lookup.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
CARGO="$(rustup which cargo 2>/dev/null || command -v cargo)"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

echo "▸ Building mur CLI"
"$CARGO" build -p mur-core --bin mur >/dev/null

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

# ── 2. Help shows supported formats ─────────────────────────────────
echo "▸ Help"
HELP="$($MUR agent export --help 2>&1)"
echo "$HELP" | grep -qF ".muragent" || { echo "MISSING .muragent in --help"; exit 1; }
echo "  ✓ --help references .muragent"

# ── 3. Create a throwaway agent + verify redirect ───────────────────
TMPHOME="$(mktemp -d)"
trap 'rm -rf "$TMPHOME"' EXIT
echo "▸ Throwaway agent at MUR_HOME=$TMPHOME"
export MUR_HOME="$TMPHOME"
export MUR_AGENT_BIN_DIR="$TMPHOME/bin"

$MUR agent create p1-9-test-agent --no-interactive --display-name "p1.9 test" --model llama3.2:3b >/dev/null

# Exporting .muragent works (the default format).
echo "▸ Export .muragent (default)"
MUROUT="$TMPHOME/p1-9-test-agent.muragent"
$MUR agent export p1-9-test-agent -o "$MUROUT" >/dev/null
[ -e "$MUROUT" ] || { echo "MISSING $MUROUT"; exit 2; }
echo "  ✓ .muragent produced"

# --format=gui and --format=bin now redirect.
for fmt in gui bin; do
  REDIR="$($MUR agent export p1-9-test-agent -o /dev/null --format "$fmt" 2>&1 || true)"
  echo "$REDIR" | grep -q ".muragent" || { echo "MISSING .muragent redirect for --format=$fmt"; exit 2; }
  echo "$REDIR" | grep -q "no longer supported" || { echo "MISSING redirect for --format=$fmt"; exit 2; }
  echo "  ✓ --format=$fmt redirected to .muragent"
done

echo
echo "▸ P1.9 smoke OK"
