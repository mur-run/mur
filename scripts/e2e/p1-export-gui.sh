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

# Ensure cargo is in PATH. On some CI runners (e.g. macos-14 ARM64),
# Homebrew's rustup-init can shadow ~/.cargo/bin/cargo.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

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
trap 'pkill -f "p1-9-test" 2>/dev/null || true; rm -rf "$TMPHOME" "$HOME/.cache/mur/p1-9-test"' EXIT
echo "▸ Throwaway agent at MUR_HOME=$TMPHOME"
export MUR_HOME="$TMPHOME"
export MUR_AGENT_BIN_DIR="$TMPHOME/bin"

$MUR agent create p1-9-test-agent --no-interactive --display-name "p1.9 test" --model llama3.2:3b >/dev/null

OUT_PATH="$TMPHOME/MyAgent.app"
EXPORT_OUT="$($MUR agent export p1-9-test-agent -o "$OUT_PATH" --format gui --theme dark --skip-notarize 2>&1 || true)"
echo "$EXPORT_OUT" | tail -10

if echo "$EXPORT_OUT" | grep -q "missing prerequisites for gui export"; then
  echo "  ✓ pipeline correctly fail-fast at prereq phase (tauri-cli not installed)"
  echo
  echo "▸ P1.9 smoke OK (quick mode — full E2E skipped)"
  exit 0
fi

if [ ! -e "$OUT_PATH" ]; then
  echo "  ✗ unexpected: pipeline did not fail-fast and did not produce artifact"
  exit 2
fi

echo "  ✓ pipeline produced artifact at $OUT_PATH"

# ── 4. (full mode) Launch the .app and verify bootstrap + sidecar ──
if [ "${FULL_E2E:-0}" != "1" ]; then
  echo
  echo "▸ P1.9 smoke OK (set FULL_E2E=1 to also launch + verify)"
  exit 0
fi

# macOS Tauri rejects current_exe() paths that contain symlinks;
# mktemp returns /var/folders/... which is a symlink to /private/var.
# Copy the bundle to ~/.cache/mur/p1-9-test/ (a stable, non-symlinked
# location) before launching.
STABLE_DIR="$HOME/.cache/mur/p1-9-test"
rm -rf "$STABLE_DIR"
mkdir -p "$STABLE_DIR"
cp -R "$OUT_PATH" "$STABLE_DIR/"
STABLE_BUNDLE="$STABLE_DIR/$(basename "$OUT_PATH")"
echo "▸ Full E2E: copied bundle to $STABLE_BUNDLE for launch"

case "$OSTYPE" in
  darwin*)   EXE="$STABLE_BUNDLE/Contents/MacOS/mur-agent-gui" ;;
  linux*)    EXE="$STABLE_BUNDLE" ;;
  msys*|cygwin*) EXE="$STABLE_BUNDLE" ;;
  *)         echo "unsupported OS $OSTYPE"; exit 3 ;;
esac
[ -x "$EXE" ] || { echo "executable not found at $EXE"; exit 3; }

# Pre-create the agent home so bootstrap is a no-op on the existing
# install, then verify the sidecar manager spawns a runtime that
# claims the running.lock.
"$EXE" >/tmp/p1-9-gui.log 2>&1 &
GUI_PID=$!
echo "  GUI launched (pid=$GUI_PID); waiting for running.lock"

LOCK="$TMPHOME/agents/p1-9-test-agent/running.lock"
for _ in $(seq 1 30); do
  if [ -e "$LOCK" ]; then
    echo "  ✓ running.lock appeared at $LOCK"
    break
  fi
  sleep 1
done

if [ ! -e "$LOCK" ]; then
  echo "  ✗ running.lock did not appear within 30s"
  cat /tmp/p1-9-gui.log | tail -20 | sed 's/^/    | /'
  kill -TERM $GUI_PID 2>/dev/null || true
  exit 4
fi

# Tear down cleanly.
kill -TERM $GUI_PID 2>/dev/null || true
wait $GUI_PID 2>/dev/null || true
sleep 1
if [ -e "$LOCK" ]; then
  echo "  ⚠ running.lock still present after SIGTERM; supervisor may be stuck"
fi

echo
echo "▸ P1.9 FULL E2E OK — bundle launches, bootstraps, spawns runtime, writes running.lock"
