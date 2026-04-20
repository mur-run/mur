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

echo ""
echo "=== ALL 8 STEPS GREEN ==="
