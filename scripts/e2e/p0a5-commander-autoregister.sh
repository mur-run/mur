#!/usr/bin/env bash
# P0a.5 cross-repo E2E: verify that the commander's MurmurBridge auto-
# registers a murmur agent by observing its `running.lock` file.
#
# This script is a library-level smoke, not a full daemon launch: it
# builds the commander's unit + integration tests for the bridge + registry
# which exercise the real file-watcher logic against a real
# `running.lock` payload. A full daemon launch requires commander
# database/Auth/plist setup and is out of scope for a simple smoke.
#
# Prereqs: `~/Projects/mur-commander/` exists and the `feat/murmur-bridge`
# branch (or a descendant) is checked out.
set -euo pipefail

MUR_REPO="${MUR_REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
COMMANDER_REPO="${COMMANDER_REPO:-$HOME/Projects/mur-commander}"

if [ ! -d "$COMMANDER_REPO" ]; then
  echo "SKIP: $COMMANDER_REPO not present — run only on a host with both repos checked out"
  exit 0
fi

# Run the bridge + registry integration tests against the real filesystem.
cd "$COMMANDER_REPO"
cargo test -p mur-engine --test murmur_bridge --test agent_registry_uuid -- --test-threads=1 2>&1 \
  | tee /tmp/p0a5-commander-autoregister.log
if ! grep -q "test result: ok" /tmp/p0a5-commander-autoregister.log; then
  echo "FAIL: commander bridge/registry tests did not pass"
  exit 1
fi

# Also exercise the redaction + spool + collector path so a failure in
# the observability pipeline surfaces in the same script.
cargo test -p mur-engine --test redaction --test spool --test collector -- --test-threads=1 2>&1 \
  | tee /tmp/p0a5-commander-collector.log
if ! grep -q "test result: ok" /tmp/p0a5-commander-collector.log; then
  echo "FAIL: commander observability tests did not pass"
  exit 1
fi

echo "OK: commander auto-register + observability smoke passed"
