#!/usr/bin/env bash
# Build the lean 3-role export-test team in an isolated MUR_HOME.
# Idempotent-ish: re-running recreates prompts/skills (agents must be removed first).
#
# Roles (focused on the export round-trip + A2A handoff):
#   xx-pm    Product Manager        skill: pm-prd-acceptance
#   xx-rust  Rust Engineer          skill: rust-idiomatic-errors-api
#   xx-qa    QA / Release Engineer  skill: qa-release-gating
#
# Deliverable the team designs (consumer-fun): `mur agent mood` — a playful
# per-agent mood (short phrase + emoji) shown in `list` and the agent card.
set -euo pipefail

REPO="/Volumes/Firecuda4tb/Projects/mur"
BIN="$REPO/target/release/mur"
export MUR_HOME="${MUR_HOME:-/tmp/mur-export-test}"
SK="$REPO/docs/testing/harness-agent-export/skills"

mkdir -p "$MUR_HOME"

mk() { # name  "system prompt"  skill_file
  local name="$1" prompt="$2" skill="$3"
  # provider=anthropic so `mur agent send` hits real Claude via the cc-proxy
  # bridge (ANTHROPIC_BASE_URL/ANTHROPIC_API_KEY come from the launch env).
  "$BIN" agent create "$name" --provider anthropic --model "${MOOD_MODEL:-claude-sonnet-4-5}" >/dev/null 2>&1 || true
  "$BIN" agent prompt set "$name" "$prompt" >/dev/null
  "$BIN" agent skill add "$name" "$SK/$skill" >/dev/null
  echo "  built $name (+skill $skill)"
}

echo "Building export-test team in MUR_HOME=$MUR_HOME"
mk xx-pm \
"You are the Product Manager for the MUR agent platform. Write tight PRDs around why/what (never how), with measurable success, explicit non-goals, and Given/When/Then acceptance criteria. Be concise. Always finish with a single line: 'HANDOFF -> <role>: <artifact they now own>'." \
pm-prd-acceptance.md

mk xx-rust \
"You are a senior Rust Engineer on the MUR platform (edition 2024). Favor idiomatic error handling (Result/Option, ?, thiserror for libs / anyhow for apps), domain-modeled error enums, and a regression test per fix. No unwrap in production paths. Be concise. Always finish with: 'HANDOFF -> <role>: <what to verify/deploy>'." \
rust-idiomatic-errors-api.md

mk xx-qa \
"You are the QA / Release Engineer for MUR. Produce risk-based test plans with explicit exit criteria, tiered regression (smoke/core/full) wired into CI hard-gates, and a tested rollback path. Be concise. Always finish with: 'HANDOFF -> <role>: <go/no-go + residual risk>'." \
qa-release-gating.md

echo "Team ready:"
"$BIN" agent list 2>/dev/null | grep -E 'NAME|xx-' || true
