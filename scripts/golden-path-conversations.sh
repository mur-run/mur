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
  dimensions: 1024
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
cat > "$TMPHOME/.mur/conversations/raw/$YDAY/cc_mock.jsonl" <<JSONL
{"v":1,"ts":"${YDAY}T10:00:00Z","src":"claude-code","conv":"mock","role":"user","content":{"t":"text","v":"mock extractive span"},"meta":{},"refs":[]}
JSONL

echo "--- step 9: mur conversations compact ---"
MUR_OLLAMA_MOCK=1 "$MUR" conversations compact | tee /tmp/gp-out-9.txt
test -f "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: no summary/${YDAY}.md"; exit 1; }
grep -q "## Extractive spans" "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: summary missing Extractive section"; exit 1; }
grep -q "## Abstractive narrative" "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: summary missing Narrative section"; exit 1; }

# ── Step 9.5: reindex --spans-only + hash-mock span selection ─────────────
# Rebuild layer=2 span rows for the yesterday summary using the hash mock,
# so Step 10's ask can pick the most query-relevant span deterministically.
echo "--- step 9.5: mur conversations reindex --spans-only (hash mock) ---"
MUR_OLLAMA_MOCK=hash "$MUR" conversations reindex --spans-only | tee /tmp/gp-step-9.5.txt
grep -q "reindexed spans:" /tmp/gp-step-9.5.txt \
  || { echo "FAIL step 9.5: reindex did not report span rebuild"; exit 1; }

# ── Step 10: ask ──────────────────────────────────────────────────────────
echo "--- step 10: mur ask --json ---"
MUR_OLLAMA_MOCK=hash "$MUR" ask "mock extractive span" --json > /tmp/gp-step-10.json
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
jq -e '[.hits_used[] | .layer] | any(. == 2)' /tmp/gp-step-10.json \
  || { echo "FAIL step 10: no layer=2 hit in hits_used (collapsed tree mixes layers)"; exit 1; }

# ── Step 11.5: compact cascades into rollup for 7 consecutive days ────────
echo "--- step 11.5: compact cascades into rollup (7 seeded days) ---"
# Seed 7 consecutive days in 2026-W16 (Apr 13-19), which hasn't been touched yet
# by earlier steps. Seed raw data, compact each day, then run cascade.
TESTWEEK="2026-W16"
for d in $(seq 13 19); do
  DATE=$(printf "2026-04-%02d" "$d")
  RAW_DIR="$TMPHOME/.mur/conversations/raw/$DATE"
  mkdir -p "$RAW_DIR"
  # Seed fresh raw data with meaningful content
  cat > "$RAW_DIR/cc_w16.jsonl" <<JSONL
{"v":1,"ts":"${DATE}T10:00:00Z","src":"claude-code","conv":"w16","role":"user","content":{"t":"text","v":"shipped several fixes and refactors for day $d"},"meta":{},"refs":[]}
JSONL
done
# Compact each day individually (targeted --date — no cascade)
for d in $(seq 13 19); do
  DATE=$(printf "2026-04-%02d" "$d")
  MUR_OLLAMA_MOCK=hash "$MUR" conversations compact --date "$DATE" > /dev/null
done
# Now run compact with no --date so the cascade fires and rollup sweeps
MUR_OLLAMA_MOCK=hash "$MUR" conversations compact 2>&1 | tee /tmp/gp-step-11.5b.txt
grep -q "rollup sweep" /tmp/gp-step-11.5b.txt \
  || { echo "FAIL step 11.5: compact did not cascade into rollup"; exit 1; }

# ── Step 12: explicit rollup --all-missing ───────────────────────────────
echo "--- step 12: mur conversations rollup --all-missing (hash mock) ---"
MUR_OLLAMA_MOCK=hash "$MUR" conversations rollup --all-missing --max-weeks 4 --max-months 2 2>&1 | tee /tmp/gp-step-12.txt
grep -q "rolled up" /tmp/gp-step-12.txt \
  || { echo "FAIL step 12: rollup --all-missing did not emit sweep report"; exit 1; }
test -f "$TMPHOME/.mur/conversations/summary/weekly/2026-W16.md" \
  || { echo "FAIL step 12: weekly md 2026-W16.md missing"; exit 1; }

# ── Step 13: reindex --rollups-only + doctor ─────────────────────────────
echo "--- step 13: mur conversations reindex --rollups-only ---"
MUR_OLLAMA_MOCK=hash "$MUR" conversations reindex --rollups-only 2>&1 | tee /tmp/gp-step-13a.txt
grep -q "reindexed rollups:" /tmp/gp-step-13a.txt \
  || { echo "FAIL step 13: reindex --rollups-only missing report"; exit 1; }
MUR_OLLAMA_MOCK=hash "$MUR" conversations doctor 2>&1 | tee /tmp/gp-step-13b.txt
grep -q "weekly rollups:" /tmp/gp-step-13b.txt \
  || { echo "FAIL step 13: doctor missing weekly rollups line"; exit 1; }
grep "weekly rollups:" /tmp/gp-step-13b.txt | grep -q "2026-W16" \
  || { echo "FAIL step 13: doctor weekly rollups should include 2026-W16"; exit 1; }
grep -q "monthly rollups:" /tmp/gp-step-13b.txt \
  || { echo "FAIL step 13: doctor missing monthly rollups line"; exit 1; }

# ── Step 14: ask surfaces rollup hit via collapsed tree ──────────────────
echo "--- step 14: mur ask --json (expect layer=3 or layer=4 rollup hit) ---"
# Use the exact narrative from the week-level mock to match hash embedding
MUR_OLLAMA_MOCK=hash "$MUR" ask "Mock narrative: this week the developer shipped several fixes and refactors." --json > /tmp/gp-step-14.json
cat /tmp/gp-step-14.json
jq -e '.hits_used | length >= 1' /tmp/gp-step-14.json \
  || { echo "FAIL step 14: no hits_used"; exit 1; }
# At least one hit must be layer=3 or layer=4 — proves collapsed tree surfaced a rollup.
jq -e '[.hits_used[] | .layer] | any(. == 3 or . == 4)' /tmp/gp-step-14.json \
  || { echo "FAIL step 14: no rollup hit (layer=3 or layer=4) in hits_used"; exit 1; }

# ── Step 16: mur ask --continue appends follow-up turn ─────────────────
echo "--- step 16: mur ask --continue (multi-turn) ---"
MUR_OLLAMA_MOCK=1 "$MUR" ask "what did I ship this week?" > /tmp/gp-step-16a.txt 2>&1
MUR_OLLAMA_MOCK=1 "$MUR" ask --continue "what about the prior week?" > /tmp/gp-step-16b.txt 2>&1
test -f "$TMPHOME/.mur/conversations/ask-session.jsonl" \
  || { echo "FAIL step 16: ask-session.jsonl missing"; exit 1; }
lines=$(wc -l < "$TMPHOME/.mur/conversations/ask-session.jsonl")
[ "$lines" -eq 2 ] \
  || { echo "FAIL step 16: expected 2 turns in session, got $lines"; exit 1; }
# Second turn must have non-null rewritten_question
jq -e '.rewritten_question != null' \
  <(tail -1 "$TMPHOME/.mur/conversations/ask-session.jsonl") > /dev/null \
  || { echo "FAIL step 16: second turn missing rewritten_question"; exit 1; }
# Second turn must have non-skipped rewriter_status
status=$(jq -r '.rewriter_status' \
  <(tail -1 "$TMPHOME/.mur/conversations/ask-session.jsonl"))
[ "$status" != "skipped" ] \
  || { echo "FAIL step 16: second turn rewriter_status is 'skipped', expected rewrote/no_rewrite_needed/failed"; exit 1; }

# ── Step 17: mur ask --show-session prints summary ─────────────────────
echo "--- step 17: mur ask --show-session ---"
"$MUR" ask --show-session 2>&1 | tee /tmp/gp-step-17.txt
grep -q "turns: 2" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not report turn count"; exit 1; }
grep -q "what did I ship this week" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not echo first question"; exit 1; }
grep -q "session:" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not print session path"; exit 1; }

# ── Step 18: Phase 3.5 Stage 1b fires under tight budget ──────────────
echo "--- step 18: mur ask --json (expect .stage_1b compressed_count+cache_hits > 0) ---"
# Seed a rich daily summary (2026-04-10) with a long extractive span so
# Stage 1b has >= 400-char content to compress. All earlier steps produced
# short mock spans (< 400 chars) that Stage 1b would skip. This summary is
# pre-crafted (no compact needed) and reindexed at layer=2 before asking.
#
# The span text is one long clause (no ". " breaks → 1 sentence → Stage 1
# heuristic compression is skipped per COMPRESS_MIN_SENTENCES=4; Stage 1b
# then picks it up as a long hit and compresses it via the mock Ollama call).
GP_RICH_DATE="2026-04-10"
GP_RICH_DIR="$TMPHOME/.mur/conversations/summary"
# The exact span text is stored in a variable so the ask query can use the
# same string — hash-mock embedding is text-deterministic, so matching text
# → matching vector → cosine=1.0 → guaranteed top hit.
# shellcheck disable=SC2016
GP_RICH_SPAN='Phase 3.5 shipped Stage 1b LLM-abstractive compression into the mur ask overflow cascade — after heuristic Stage 1 (sentence-level extractive scoring), Stage 1b calls an Ollama model to produce shorter per-hit summaries when the prompt still overflows the max_context_tokens budget; key design decisions include a 5-second timeout with soft-fail semantics, a sha256-keyed filesystem cache at conversations/cache/abstractive/ so repeat queries skip the LLM call entirely (cache_hits), a MIN_CONTENT_CHARS=400 guard that skips trivially short hits to avoid wasting LLM calls on content that is already compact, and the compressed field on ResolvedHit which carries either Heuristic or Abstractive provenance so downstream callers can distinguish between the two compression stages in observability output'
mkdir -p "$GP_RICH_DIR"
cat > "$GP_RICH_DIR/${GP_RICH_DATE}.md" <<RICHMD
---
schema: 1
kind: day
date: 2026-04-10
generated_at: 2026-04-10T12:00:00Z
generated_by:
  extractive_model: qwen3:14b
  abstractive_model: qwen3:14b
  mur_version: test
duration_ms: 100
conv_count: 1
msg_count: 4
sources: [cc]
pattern_refs: []
keywords: [phase, stage, compression, tokens, overflow, cascade, abstractive, pipeline, budget, hits]
links:
  prev: null
  next: null
warnings: []
input_content_sha: aabbcc
---

## Extractive spans

[1] _{cc/rich-conv @L1}_:
> ${GP_RICH_SPAN}

## Abstractive narrative

Phase 3.5 shipped Stage 1b LLM-abstractive compression into the mur ask overflow cascade after heuristic Stage 1 sentence-level extractive scoring calls an Ollama model to produce shorter per-hit summaries when the prompt still overflows the max_context_tokens budget with key design decisions including a 5-second timeout with soft-fail semantics and a sha256-keyed filesystem cache at conversations/cache/abstractive/ so repeat queries skip the LLM call entirely

RICHMD

# Reindex the new rich summary at layer=2 using the hash mock so the ask
# query (same text as span) gets the same vector → cosine=1.0 → top result.
MUR_OLLAMA_MOCK=hash "$MUR" conversations reindex --spans-only > /tmp/gp-step-18-reindex.txt 2>&1
grep -q "reindexed spans:" /tmp/gp-step-18-reindex.txt \
  || { echo "FAIL step 18: reindex before ask failed"; cat /tmp/gp-step-18-reindex.txt; exit 1; }

# Nudge the budget tight on the fly for this one invocation via a config
# override. The rich span (>=400 chars, 1 sentence so Stage 1 skips it,
# Stage 1b compresses it) guarantees Stage 1b fires with compressed_count>0.
GP_CONFIG_OVERRIDE="$TMPHOME/.mur/config.yaml"
# Back up existing config (if any) and drop a tighter one.
if [ -f "$GP_CONFIG_OVERRIDE" ]; then
    cp "$GP_CONFIG_OVERRIDE" "$GP_CONFIG_OVERRIDE.bak"
fi
cat > "$GP_CONFIG_OVERRIDE" <<'EOF'
conversations:
  ask:
    max_context_tokens: 400
    response_tokens: 200
    summarize_hits_enabled: true
    compress_hits_enabled: true
EOF

# Ask with the exact span text as query — hash mock vector matches exactly.
MUR_OLLAMA_MOCK=hash "$MUR" ask --json "$GP_RICH_SPAN" > /tmp/gp-step-18.json 2>/tmp/gp-step-18.err
test -s /tmp/gp-step-18.json || { echo "FAIL step 18: empty JSON output; stderr:"; cat /tmp/gp-step-18.err; exit 1; }
# stage_1b may be absent on non-overflow — but under this budget it MUST fire.
jq -e '(.stage_1b.compressed_count // 0) + (.stage_1b.cache_hits // 0) > 0' /tmp/gp-step-18.json \
  || { echo "FAIL step 18: Stage 1b didn't fire (compressed+cache_hits = 0); JSON:"; cat /tmp/gp-step-18.json; exit 1; }

# Restore prior config to avoid surprising downstream re-runs.
if [ -f "$GP_CONFIG_OVERRIDE.bak" ]; then
    mv "$GP_CONFIG_OVERRIDE.bak" "$GP_CONFIG_OVERRIDE"
else
    rm -f "$GP_CONFIG_OVERRIDE"
fi

echo ""
echo "=== ALL 18 STEPS GREEN ==="
