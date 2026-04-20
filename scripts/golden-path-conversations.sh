#!/usr/bin/env bash
# golden-path-conversations.sh
# End-to-end smoke test for mur's conversations Phase 1.
# Runs under an isolated $HOME via mktemp so it never touches real ~/.mur.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MUR="${MUR:-$REPO_ROOT/target/debug/mur}"
if [[ ! -x "$MUR" ]]; then
  echo "Building mur…"
  (cd "$REPO_ROOT" && cargo build -p mur-core --bin mur)
  MUR="$REPO_ROOT/target/debug/mur"
fi

TMPHOME="$(mktemp -d)"
trap 'rm -rf "$TMPHOME"' EXIT
export HOME="$TMPHOME"

echo "=== Using isolated HOME=$TMPHOME ==="

# 0. Seed config with conversations.enabled=true
mkdir -p "$TMPHOME/.mur"
cat > "$TMPHOME/.mur/config.yaml" <<'YAML'
conversations:
  enabled: true
  retention_days: 30
  poll_interval_secs: 300
  sources:
    claude_code: true
    cursor: true
    gemini: true
    aider:
      enabled: true
      watched_dirs: []
embedding:
  provider: ollama
  model: qwen3-embedding:0.6b
  dimensions: 16
  ollama_endpoint: http://localhost:11434
YAML

# 1. Empty state
echo "--- step 1: mur chat list on empty archive ---"
"$MUR" chat list | tee /tmp/gp-out-1.txt
grep -q "no conversations" /tmp/gp-out-1.txt || { echo "FAIL: expected 'no conversations'"; exit 1; }

# 2. Seed a raw day directly
TODAY="$(date -u +%Y-%m-%d)"
mkdir -p "$TMPHOME/.mur/conversations/raw/$TODAY"
cat > "$TMPHOME/.mur/conversations/raw/$TODAY/cc_sess1.jsonl" <<JSONL
{"v":1,"ts":"${TODAY}T10:00:00Z","src":"claude-code","conv":"sess1","role":"user","content":{"t":"text","v":"test LanceDB RaBitQ compression"},"meta":{},"refs":[]}
{"v":1,"ts":"${TODAY}T10:00:05Z","src":"claude-code","conv":"sess1","role":"assistant","content":{"t":"text","v":"RaBitQ compresses vectors 32x with recall@10 within 1%"},"meta":{},"refs":[]}
JSONL

echo "--- step 2: mur chat list with 1 day seeded ---"
"$MUR" chat list | tee /tmp/gp-out-2.txt
grep -q "$TODAY" /tmp/gp-out-2.txt || { echo "FAIL: day not listed"; exit 1; }
grep -q "2 msgs" /tmp/gp-out-2.txt || { echo "FAIL: msg count wrong"; exit 1; }

# 3. mur chat show
echo "--- step 3: mur chat show ---"
"$MUR" chat show "$TODAY" | tee /tmp/gp-out-3.txt
grep -q "LanceDB RaBitQ" /tmp/gp-out-3.txt || { echo "FAIL: content not visible"; exit 1; }

# 4. mur chat raw
echo "--- step 4: mur chat raw ---"
"$MUR" chat raw "$TODAY" sess1 | tee /tmp/gp-out-4.txt
grep -q "RaBitQ compresses" /tmp/gp-out-4.txt || { echo "FAIL: raw content missing"; exit 1; }

# 5. Doctor
echo "--- step 5: doctor ---"
"$MUR" conversations doctor | tee /tmp/gp-out-5.txt
grep -q "raw day-dirs: 1" /tmp/gp-out-5.txt || { echo "FAIL: doctor did not see seeded day"; exit 1; }

# 6. Migrate (no flag = dry-run per BP2) on empty commander layout
echo "--- step 6: migrate (default dry-run) ---"
"$MUR" conversations migrate | tee /tmp/gp-out-6.txt
grep -q "long_term.jsonl: 0 lines" /tmp/gp-out-6.txt || { echo "FAIL: dry-run output malformed"; exit 1; }

# 7. Cleanup (nothing old enough to delete)
echo "--- step 7: cleanup ---"
"$MUR" conversations cleanup | tee /tmp/gp-out-7.txt
grep -q "Scanned 1 dirs, deleted 0" /tmp/gp-out-7.txt || { echo "FAIL: cleanup output unexpected"; exit 1; }

# 8. Seed an old day + a summary, then cleanup should delete it
echo "--- step 8: cleanup of old day with summary ---"
OLD="2025-01-01"
mkdir -p "$TMPHOME/.mur/conversations/raw/$OLD"
cat > "$TMPHOME/.mur/conversations/raw/$OLD/cc_old.jsonl" <<JSONL
{"v":1,"ts":"${OLD}T10:00:00Z","src":"claude-code","conv":"old","role":"user","content":{"t":"text","v":"old turn"},"meta":{},"refs":[]}
JSONL
mkdir -p "$TMPHOME/.mur/conversations/summary"
printf -- '---\ndate: %s\n---\n# old\n' "$OLD" > "$TMPHOME/.mur/conversations/summary/$OLD.md"
"$MUR" conversations cleanup | tee /tmp/gp-out-8.txt
grep -q "deleted 1" /tmp/gp-out-8.txt || { echo "FAIL: cleanup should have deleted old day"; exit 1; }
[[ ! -d "$TMPHOME/.mur/conversations/raw/$OLD" ]] || { echo "FAIL: old raw dir still exists"; exit 1; }

# ── Step 9: compact ───────────────────────────────────────────────────────
# Seed yesterday's raw so compact has something to process
# (compact_missing only scans days < today per design).
YDAY="$(date -u -v-1d +%Y-%m-%d 2>/dev/null || date -u -d 'yesterday' +%Y-%m-%d)"
mkdir -p "$TMPHOME/.mur/conversations/raw/$YDAY"
cat > "$TMPHOME/.mur/conversations/raw/$YDAY/cc_yd.jsonl" <<JSONL
{"v":1,"ts":"${YDAY}T10:00:00Z","src":"claude-code","conv":"yd","role":"user","content":{"t":"text","v":"mock extractive span seeded for compact golden-path"},"meta":{},"refs":[]}
JSONL

echo "--- step 9: mur conversations compact ---"
MUR_OLLAMA_MOCK=1 "$MUR" conversations compact | tee /tmp/gp-out-9.txt
test -f "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: no summary/${YDAY}.md"; exit 1; }
grep -q "## Extractive spans" "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: summary missing Extractive section"; exit 1; }
grep -q "## Abstractive narrative" "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: summary missing Narrative section"; exit 1; }

# ── Step 10: ask ──────────────────────────────────────────────────────────
echo "--- step 10: mur ask --json ---"
MUR_OLLAMA_MOCK=1 "$MUR" ask "what compression techniques did I discuss" --json > /tmp/gp-step-10.json
cat /tmp/gp-step-10.json
# The mock ollama answer contains a [cit: 2026-04-19 claude-code/mock:L1] anchor
# that the grounding filter will strip (it's not one of the real valid anchors
# from prompt::render). So we verify:
#   - the command succeeded and wrote JSON
#   - the response has at least one HitInfo (real retrieval happened)
jq -e '.hits_used | length >= 1' /tmp/gp-step-10.json \
  || { echo "FAIL step 10: no hits_used — retrieval did not fire"; exit 1; }
jq -e '.answer | type == "string"' /tmp/gp-step-10.json \
  || { echo "FAIL step 10: missing or non-string .answer"; exit 1; }

echo ""
echo "=== ALL 10 STEPS GREEN ==="
