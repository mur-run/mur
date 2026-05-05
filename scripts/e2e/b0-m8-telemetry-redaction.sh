#!/usr/bin/env bash
# scripts/e2e/b0-m8-telemetry-redaction.sh — B0 M8 telemetry hardening.
#
# Drives the harness-level acceptance for M8.1 (telemetry redactor +
# envelope chokepoint) and M8.3 (companion zero-network audit). Closes
# B0 rules 9 + 12.
#
# Acceptance gates (spec
# `docs/superpowers/specs/2026-05-05-b0-m8-telemetry-hardening-design.md` §6):
#  1. `redact_secrets` replaces every credential pattern from the M7.5
#     regex set with `[REDACTED:<label>]`; `Cow::Borrowed` on clean
#     strings (no allocation hot path).
#  2. `redact_home_path` collapses `/Users/<u>/`, `/home/<u>/`,
#     `C:\Users\<u>\` to `~/` so error chains can't leak OS account
#     names.
#  3. `redact_envelope` walks `Value::Object` / `Value::Array` and
#     applies both redactors to every string leaf in place;
#     numbers / bools / nulls untouched.
#  4. The redactor is wired into `TelemetryWriter::new`'s spawn loop
#     between `event_to_notification` and `f.write_all`, so every
#     Event variant + every hook attribute inherits redaction.
#  5. Companion source files (13 of them) embed via `include_str!`
#     and pass three audit checks: no forbidden tokens, drift guard
#     vs. `pub mod` count, allow-list on `crate::llm::*` references.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/2 mur-agent-runtime redactor (rule 9): secrets + paths + envelope"
cargo test -p mur-agent-runtime --lib --quiet redact

echo "==> 2/2 mur-agent-runtime companion network audit (rule 12)"
cargo test -p mur-agent-runtime --lib --quiet companion::network_audit

echo
echo "OK — B0 M8 telemetry hardening acceptance gates passed (harness-level)."
echo "Manual bundle-level checks (tail real telemetry/<date>.jsonl after"
echo "running an agent and confirm secrets / home paths are redacted)"
echo "live in the cookbook at docs/cookbook/b0-telemetry-redaction.md."
