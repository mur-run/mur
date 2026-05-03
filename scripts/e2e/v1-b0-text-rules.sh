#!/usr/bin/env bash
# scripts/e2e/v1-b0-text-rules.sh — B0 text-only rules acceptance.
#
# Acceptance gates (roadmap §6.1):
# Rules 1, 2, 3, 5, 7, 8, 11 — each has at least one positive +
# negative test in mur-agent-runtime/tests/b0_rule*.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/2 build mur-agent-runtime tests (release)"
cargo build -p mur-agent-runtime --tests --release --quiet

echo "==> 2/2 B0 text-rules gates"
cargo test --release -p mur-agent-runtime --quiet \
    --test b0_rule1_fs_confinement \
    --test b0_rule2_network_allowlist \
    --test b0_rule3_spotlight_tool_results \
    --test b0_rule5_spawn_deny \
    --test b0_rule7_secret_prefilter \
    --test b0_rule8_memory_redaction \
    --test b0_rule11_mcp_signature

echo "✅ B0 text-rules E2E passed"
