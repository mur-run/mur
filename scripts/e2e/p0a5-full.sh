#!/usr/bin/env bash
# Full P0a.5 smoke runner.
#
# Runs the two P0a.5 E2E scripts in sequence:
#   1. identity-handshake — build `mur` + `mur-agent-runtime` in the mur
#      workspace, create an agent, verify identity + agent-card.
#   2. commander-autoregister — run the mur-commander bridge + registry +
#      observability integration tests against the feature branch.
#
# The commander script auto-skips if `~/Projects/mur-commander/` is not
# present, so this runner works on hosts where only the mur repo is
# checked out.
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "=== G1: identity + agent-card smoke ==="
scripts/e2e/p0a5-identity-handshake.sh

echo "=== G2: commander auto-register + collector smoke ==="
scripts/e2e/p0a5-commander-autoregister.sh

echo "=== P0a.5 E2E: ALL PASS ==="
