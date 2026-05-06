#!/usr/bin/env bash
# scripts/e2e/b0-m9.3.5-description-probe.sh — B0 M9.3.5 acceptance.
#
# Drives the harness-level acceptance for M9.3.5.1 (probe_mcp_descriptions
# helper + cmd_mcp_pin live probe) and M9.3.5.2 (`mur agent mcp inspect
# --probe`). Closes the description-hash half of B0 rule 6.
#
# Acceptance gates (spec
# `docs/superpowers/specs/2026-05-06-b0-m9.3.5-description-hash-probe-design.md` §6):
#  1. `probe_mcp_descriptions` helper exists + ProbeError::Timeout
#     fires when the MCP doesn't respond inside the budget (M9.3.5.1).
#  2. `cmd_mcp_pin` populates `description_hash` on probe success +
#     leaves it unchanged on probe failure (M9.3.5.1).
#  3. `inspect_one_probed` upgrades `InspectStatus::Clean` →
#     `DescriptionDrift` (exit 2) when the live tools/list differs
#     from the pinned hash; `BinaryDrift` → `BothDrifted` (exit 3)
#     when both halves drift (M9.3.5.2).
#  4. `--probe` is opt-in; default `inspect` stays binary-only and
#     fast (M9.3.5.2).
#
# The unit tests in `mur-core::cmd::agent_mcp_pin::tests` cover the
# logic via in-process callers; a real-MCP integration test would
# need a fixture binary that responds to JSON-RPC over stdio (the
# existing `tests/fixtures/mock_mcp/main.rs` is built only as part
# of mur-agent-runtime's test suite).

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/2 mur-core agent_mcp_pin (helpers + inspect/pin probe)"
cargo test -p mur-core --lib --quiet agent_mcp_pin

echo "==> 2/2 mur-agent-runtime mock_mcp + mcp_client integration"
cargo test -p mur-agent-runtime --test mcp_client --quiet

echo
echo "OK — B0 M9.3.5 description-hash probe acceptance gates passed."
echo "Manual bundle-level QA (real MCP install, observe \`mur agent mcp"
echo "inspect --probe\` exit codes match drift state) lives in the"
echo "cookbook at docs/cookbook/b0-mcp-install-verify.md (\"Periodic"
echo "drift checks\" section)."
