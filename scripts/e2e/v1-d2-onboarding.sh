#!/usr/bin/env bash
# scripts/e2e/v1-d2-onboarding.sh — D2 onboarding wizard E2E acceptance.
#
# Acceptance gates (roadmap §4.2):
#   1. Wizard completes in <= 120 seconds (--answers path; non-interactive).
#   2. `companion preview <name> --situation morning_greeting --no-llm`
#      output contains the first_memory text.
#   3. MockClock-driven runtime test advances 72 h and produces a
#      proactive message body containing the first_memory string
#      (the cargo test in onboarding_morning_greeting_72h.rs).
#
# The script uses a tmpfs MUR_HOME so it never pollutes ~/.mur, and
# redirects MUR_AGENT_BIN_DIR so `mur agent create` doesn't drop a
# symlink under ~/.local/bin.
#
# Usage:
#   scripts/e2e/v1-d2-onboarding.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

# ── Hermetic temp HOME ───────────────────────────────────────────────────────

TMPHOME="$(mktemp -d -t mur-d2-onboarding-e2e.XXXXXX)"
export MUR_HOME="$TMPHOME"
export MUR_AGENT_BIN_DIR="$TMPHOME/bin"
mkdir -p "$MUR_AGENT_BIN_DIR"
trap 'rm -rf "$TMPHOME"' EXIT

# ── 1/3 build ────────────────────────────────────────────────────────────────

echo "==> 1/3 build mur (release)"
cargo build --release --bin mur --quiet

MUR="$REPO_ROOT/target/release/mur"
[[ -x "$MUR" ]] || { echo "expected binary at $MUR" >&2; exit 1; }

# ── 2/3 create agent + run wizard via --answers ──────────────────────────────

echo "==> 2/3 create agent + run wizard via --answers"
ANSWERS="$TMPHOME/onboarding-answers.yaml"
cat >"$ANSWERS" <<'YAML'
locale: en-US
name_for_user: David
agent_display_name: Mochi
relationship: friend
formality: casual
extra_instructions: ""
first_memory: "Sunday in Taipei"
proactive_tier: warm_only
YAML

t0=$(date +%s)
"$MUR" agent create d2-test --provider stub
"$MUR" agent companion init d2-test --answers "$ANSWERS"
t1=$(date +%s)
elapsed=$((t1 - t0))
echo "    wizard --answers elapsed ${elapsed}s"
[[ $elapsed -le 120 ]] || { echo "FAIL: wizard took ${elapsed}s (> 120s)" >&2; exit 1; }

# ── 3/3 preview morning_greeting ─────────────────────────────────────────────

echo "==> 3/3 preview morning_greeting (--no-llm)"
out=$("$MUR" agent companion preview d2-test --situation morning_greeting --no-llm)
echo "$out" | grep -q "Sunday in Taipei" || {
  echo "FAIL: preview did not include first_memory; got:" >&2
  echo "$out" >&2
  exit 1
}

# ── 72h MockClock acceptance (cargo test) ────────────────────────────────────

echo "==> 72h MockClock acceptance (cargo test)"
cargo test -p mur-agent-runtime --test onboarding_morning_greeting_72h --release --quiet

echo "✅ D2 onboarding E2E passed"
