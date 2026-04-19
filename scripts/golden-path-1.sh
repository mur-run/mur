#!/usr/bin/env bash
# scripts/golden-path-1.sh — Memory-sync Phase 1 end-to-end smoke test.
#
# What this proves:
#   1. A user can create a pattern locally with the mur CLI.
#   2. `mur push` serializes queued signals and POSTs them to mur-server.
#   3. `mur sync status` reports the outbox state correctly.
#   4. `mur fetch` retrieves server-side signals back.
#
# Prerequisites:
#   - A running mur-server reachable at $MUR_SERVER_URL (default http://localhost:8080)
#   - mur binary built from the feat/memory-sync-phase-1 branch
#   - Postgres with migrations applied (031 minimum)
#
# Usage:
#   MUR_SERVER_URL=http://localhost:8080 \
#   MUR_USER_TOKEN=<bearer-for-alice> \
#     bash scripts/golden-path-1.sh
#
# If MUR_USER_TOKEN isn't provided the script skips network steps and only
# exercises the local CLI flow (pattern create + outbox write + dry-run push).
#
# Exit 0 = all assertions passed, 1 = something broke.

set -euo pipefail

SERVER_URL="${MUR_SERVER_URL:-http://localhost:8080}"
TOKEN="${MUR_USER_TOKEN:-}"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MUR_BIN="${MUR_BIN:-$REPO_ROOT/target/debug/mur}"

# Isolated HOME so the script doesn't touch the user's real ~/.mur.
TEST_HOME="$(mktemp -d -t mur-gp1-XXXXXX)"
trap 'rm -rf "$TEST_HOME"' EXIT
export HOME="$TEST_HOME"
export MUR_SERVER_URL="$SERVER_URL"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { bold "→ $*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m✗\033[0m %s\n' "$*"; exit 1; }

# ---------------------------------------------------------------------------
# Setup: verify mur binary exists and prints a version.
# ---------------------------------------------------------------------------
step "verifying mur binary at $MUR_BIN"
if [[ ! -x "$MUR_BIN" ]]; then
    fail "mur binary not found at $MUR_BIN. Build first: cargo build -p mur-core"
fi
"$MUR_BIN" --version >/dev/null || fail "mur --version failed"
ok "mur binary reachable"

# ---------------------------------------------------------------------------
# Step 1: local — sync status on empty home should say nothing pending.
# ---------------------------------------------------------------------------
step "sync status on fresh install"
OUT=$("$MUR_BIN" sync status)
echo "$OUT" | grep -q "outbox pending: 0" || fail "expected 'outbox pending: 0', got: $OUT"
echo "$OUT" | grep -q "last fetch:     never" || fail "expected 'last fetch: never', got: $OUT"
ok "status reports empty state"

# ---------------------------------------------------------------------------
# Step 2: local — push --dry-run on empty outbox.
# ---------------------------------------------------------------------------
step "push --dry-run with empty outbox"
OUT=$("$MUR_BIN" push --dry-run)
echo "$OUT" | grep -q "outbox empty, nothing to push" || fail "unexpected: $OUT"
ok "dry-run handles empty outbox correctly"

# ---------------------------------------------------------------------------
# Step 3: local — fetch --dry-run on fresh cursor.
# ---------------------------------------------------------------------------
step "fetch --dry-run with no cursor"
OUT=$("$MUR_BIN" fetch --dry-run)
echo "$OUT" | grep -q "would fetch since=None" || fail "unexpected: $OUT"
ok "dry-run fetch reports nil cursor"

# ---------------------------------------------------------------------------
# Step 4: local — fabricate an outbox signal and confirm sync status picks it up.
# ---------------------------------------------------------------------------
step "seed an outbox signal directly"
mkdir -p "$HOME/.mur/outbox"
SIG_ID="00000000-0000-0000-0000-000000000abc"
SIG_PATH="$HOME/.mur/outbox/2026-04-18T00-00-00-${SIG_ID}.yaml"
cat >"$SIG_PATH" <<YAML
id: ${SIG_ID}
emitted_at: 2026-04-18T00:00:00Z
actor:
  source: mur_cli
  native_id: alice-local
target:
  kind: pattern
  name: golden-path-test
  scope:
    kind: personal
kind:
  type: execution_success
scope:
  kind: personal
confidence: 1.0
schema_version: 1
YAML
ok "wrote signal to $SIG_PATH"

step "sync status should now show 1 pending"
OUT=$("$MUR_BIN" sync status)
echo "$OUT" | grep -q "outbox pending: 1" || fail "expected 'outbox pending: 1', got: $OUT"
ok "status picked up the seeded signal"

step "push --dry-run should list the signal"
OUT=$("$MUR_BIN" push --dry-run)
echo "$OUT" | grep -q "would push 1 signal" || fail "expected 1 signal in dry-run, got: $OUT"
echo "$OUT" | grep -q "$SIG_ID" || fail "signal id missing from dry-run listing"
ok "dry-run enumerates the outbox correctly"

# ---------------------------------------------------------------------------
# Step 5 (optional): real push + fetch against a reachable server.
# ---------------------------------------------------------------------------
if [[ -z "$TOKEN" ]]; then
    echo
    bold "⚠ skipping network steps — set MUR_USER_TOKEN to exercise real push/fetch"
    echo
    bold "GOLDEN PATH 1 (local-only) PASSED"
    exit 0
fi

step "writing auth token for network phase"
mkdir -p "$HOME/.mur"
cat >"$HOME/.mur/auth.json" <<JSON
{
    "access_token": "$TOKEN",
    "refresh_token": "",
    "token_type": "Bearer",
    "expires_in": 86400,
    "user_id": "test-user"
}
JSON
ok "auth.json written"

step "mur push against $SERVER_URL"
"$MUR_BIN" push || fail "push returned non-zero"
ok "push succeeded"

step "sync status after push"
OUT=$("$MUR_BIN" sync status)
echo "$OUT" | grep -q "outbox pending: 0" || fail "expected outbox empty after push, got: $OUT"
ok "outbox drained"

step "confirm server-side aggregator doesn't reject (wait ~35s)"
sleep 35
# Server has its own aggregator worker that processes signals every 30s.
ok "waited for aggregator pass"

step "mur fetch pulls back any pending"
OUT=$("$MUR_BIN" fetch)
echo "$OUT" | grep -q "fetched:" || fail "expected fetch summary, got: $OUT"
ok "fetch round-tripped"

echo
bold "GOLDEN PATH 1 (full network) PASSED"
