#!/usr/bin/env bash
#
# Companion Phase 1.1 E2E smoke (Spec §8.4).
#
# This script covers the CLI surface of `mur agent companion ...` against a
# tmpfs MUR_HOME. Steps that depend on MockClock advancement or running-agent
# state (Spec §8.4 steps 3, 4-tick, 5, 6-tick, 7-rendered) live in the
# integration test suite and are intentionally NOT duplicated here:
#
#   - Step 3  (reactive sys_prompt zh-TW directive)  → tests/voice_snapshots.rs
#   - Step 4  (clock-advanced tick → inbox file)     → tests/companion_gating.rs
#   - Step 5  (ack --bad bandit weight × 0.5)        → Phase 1.1 does not persist
#                                                       bandit-state to disk (M5.7)
#   - Step 6  (clock-gated send-vs-skip)             → tests/companion_gating.rs
#   - Step 7  (why-did-you-message full render)      → tested as smoke only; full
#                                                       render needs a real MessageSent
#                                                       in the ledger (integration test)
#
# Time budget: < 90 s (cargo build dominates; runtime is < 5 s).
#
# Usage:
#   scripts/e2e/companion-phase11.sh
#   scripts/e2e/companion-phase11.sh --release    # use release binary

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

PROFILE_DIR="debug"
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_DIR="release" ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# Build the binary we need.
if [[ "$PROFILE_DIR" == "release" ]]; then
  echo "==> Building workspace (release)..."
  cargo build --workspace --release --quiet
else
  echo "==> Building workspace (debug)..."
  cargo build --workspace --quiet
fi

MUR_BIN="$REPO_ROOT/target/$PROFILE_DIR/mur"
[[ -x "$MUR_BIN" ]] || { echo "expected binary at $MUR_BIN" >&2; exit 1; }

# Hermetic temp HOME.
TMPHOME="$(mktemp -d -t mur-companion-e2e.XXXXXX)"
export MUR_HOME="$TMPHOME"
trap 'rm -rf "$TMPHOME"' EXIT

# We don't want create to drop a real binary into ~/.local/bin.
export MUR_AGENT_BIN_DIR="$TMPHOME/bin"
mkdir -p "$MUR_AGENT_BIN_DIR"

AGENT_NAME="test-darwin"

# ── Step 1: create agent ─────────────────────────────────────────────────────

echo "==> [1/8] mur agent create $AGENT_NAME --provider stub"
"$MUR_BIN" agent create "$AGENT_NAME" --provider stub
[[ -d "$TMPHOME/agents/$AGENT_NAME" ]] || { echo "agent home not created" >&2; exit 1; }

# ── Step 2: companion init + voice eject ─────────────────────────────────────

ANSWERS_FILE="$TMPHOME/companion-answers.yaml"
cat > "$ANSWERS_FILE" <<'YAML'
locale: zh-TW
name_for_user: 朋友
relationship: friend
formality: casual
extra_instructions: ""
YAML

echo "==> [2/8] mur agent companion init $AGENT_NAME --answers $ANSWERS_FILE"
"$MUR_BIN" agent companion init "$AGENT_NAME" --answers "$ANSWERS_FILE"

# init writes relationship.json but not voice.md; eject produces voice.md.
REL_PATH="$TMPHOME/agents/$AGENT_NAME/companion/relationship.json"
[[ -f "$REL_PATH" ]] || { echo "relationship.json not written" >&2; exit 1; }
grep -q "zh-TW" "$REL_PATH" || { echo "locale zh-TW not in relationship.json" >&2; exit 1; }

"$MUR_BIN" agent companion voice eject "$AGENT_NAME"
VOICE_PATH="$TMPHOME/agents/$AGENT_NAME/companion/voice.md"
[[ -f "$VOICE_PATH" ]] || { echo "voice.md not written by voice eject" >&2; exit 1; }
[[ -s "$VOICE_PATH" ]] || { echo "voice.md is empty" >&2; exit 1; }
echo "    voice.md: $(wc -c < "$VOICE_PATH") bytes"

# ── Step 3: skipped ──────────────────────────────────────────────────────────

echo "==> [3/8] (skipped) reactive sys_prompt zh-TW directive -- covered by tests/voice_snapshots.rs"

# ── Step 4: proactive enable ─────────────────────────────────────────────────

PROFILE_YAML="$TMPHOME/agents/$AGENT_NAME/profile.yaml"

echo "==> [4/8] mur agent companion proactive enable $AGENT_NAME"
"$MUR_BIN" agent companion proactive enable "$AGENT_NAME"
grep -q "enabled: true" "$PROFILE_YAML" || { echo "companion.proactive.enabled not set in profile.yaml" >&2; exit 1; }
echo "    (clock-advanced tick -> inbox file is covered by tests/companion_gating.rs)"

# ── Step 5: skipped ──────────────────────────────────────────────────────────

echo "==> [5/8] (skipped) ack --bad bandit weight x0.5 -- Phase 1.1 does not persist bandit-state to disk (M5.7)"

# ── Step 6: quiet --for + --off ──────────────────────────────────────────────

echo "==> [6/8] mur agent companion quiet $AGENT_NAME --for 1h"
"$MUR_BIN" agent companion quiet "$AGENT_NAME" --for 1h
grep -q "paused_until:" "$PROFILE_YAML" || { echo "paused_until not set in profile.yaml" >&2; exit 1; }
echo "    (clock-gated send-vs-skip behaviour is covered by tests/companion_gating.rs)"

# Clear the pause before step 7.
"$MUR_BIN" agent companion quiet "$AGENT_NAME" --off
# After --off, paused_until is skip_serializing_if = Option::is_none, so field
# must either be absent or set to null. Either way: must NOT contain a timestamp.
if grep -q "^paused_until:" "$PROFILE_YAML" 2>/dev/null; then
  grep -q "paused_until: null" "$PROFILE_YAML" \
    || { echo "paused_until not cleared after --off" >&2; exit 1; }
fi

# ── Step 7: why-did-you-message (smoke — empty ledger) ───────────────────────

echo "==> [7/8] mur agent companion why-did-you-message $AGENT_NAME (no msg-id; expect empty)"
# Without a message id, lists last 7 days of MessageSent events -- should run
# cleanly with 0 sends and print the "(no MessageSent events...)" notice.
OUTPUT=$("$MUR_BIN" agent companion why-did-you-message "$AGENT_NAME" 2>&1)
echo "    output: $OUTPUT"
echo "$OUTPUT" | grep -q "no MessageSent events" \
  || { echo "expected 'no MessageSent events' message, got: $OUTPUT" >&2; exit 1; }

# ── Step 8: rhythm wipe ──────────────────────────────────────────────────────

echo "==> [8/8] mur agent companion rhythm wipe $AGENT_NAME --yes"
"$MUR_BIN" agent companion rhythm wipe "$AGENT_NAME" --yes

# Voice config in profile.yaml must be preserved.
grep -q "relationship: friend" "$PROFILE_YAML" \
  || { echo "relationship not preserved in profile.yaml after wipe" >&2; exit 1; }
grep -q "locale: zh-TW" "$PROFILE_YAML" \
  || { echo "locale not preserved in profile.yaml after wipe" >&2; exit 1; }

# Inbox must not exist or be empty.
INBOX_DIR="$TMPHOME/agents/$AGENT_NAME/companion/inbox"
if [[ -d "$INBOX_DIR" ]]; then
  count=$(find "$INBOX_DIR" -type f | wc -l | tr -d ' ')
  [[ "$count" == "0" ]] || { echo "inbox has $count files after wipe" >&2; exit 1; }
fi

echo "OK companion-phase11 E2E smoke passed"
