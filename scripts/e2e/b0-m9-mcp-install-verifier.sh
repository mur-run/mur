#!/usr/bin/env bash
# scripts/e2e/b0-m9-mcp-install-verifier.sh — B0 M9 MCP rug-pull defense.
#
# Drives the harness-level acceptance for M9.1 (schema), M9.2 (install
# hashing), M9.3 (startup verify), and M9.4 (inspect / pin CLI). Closes
# B0 rule 6 — the supply-chain story for MCP servers.
#
# Acceptance gates (spec
# `docs/superpowers/specs/2026-05-06-b0-m9-mcp-install-verifier-design.md` §8):
#  1. `McpServerEntry` schema round-trips with all four pin fields
#     optional + `#[serde(default, skip_serializing_if = "Option::is_none")]`
#     so pre-M9 profiles deserialize unchanged (M9.1).
#  2. `agent_mcp_pin` helpers compute deterministic SHA-256 over both
#     the binary and the canonical-JSON tools/list response; tool
#     order is significant; description text changes are detected
#     (M9.2).
#  3. `B0SafetyHook::on_startup` refuses to spawn on hard binary
#     drift with a `mur agent mcp inspect/pin/remove` hint; soft-
#     fails on missing/unreadable binaries; pre-M9 entries skipped
#     cleanly (M9.3).
#  4. `mur agent mcp inspect` exit codes are stable (0/1/4/5 in v1;
#     2/3 reserved for M9.3.5); `mur agent mcp pin` preserves
#     publisher metadata unless `--publisher-*` overrides (M9.4).

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/3 mur-common schema (McpServerEntry pin fields)"
cargo test -p mur-common --lib --quiet mcp_pin

echo "==> 2/3 mur-core install + inspect/pin helpers"
cargo test -p mur-core --lib --quiet agent_mcp_pin
cargo test -p mur-common --lib --quiet canonical

echo "==> 3/3 mur-agent-runtime startup verifier"
cargo test -p mur-agent-runtime --lib --quiet pin_verify

# Linux-only integration tests run automatically when CARGO_BUILD_TARGET
# matches; macOS skips them via #![cfg(target_os = "linux")] gate.
if [[ "$(uname -s)" == "Linux" ]]; then
    echo "==> 3a (Linux only) startup-verify integration"
    cargo test -p mur-agent-runtime --test b0_rule6_mcp_pin --quiet
fi

echo
echo "OK — B0 M9 MCP install verifier acceptance gates passed (harness-level)."
echo "Manual bundle-level QA (real MCP install, drift the binary on disk,"
echo "confirm supervisor refuses to start) lives in the cookbook at"
echo "docs/cookbook/b0-mcp-install-verify.md."
