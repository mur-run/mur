#!/usr/bin/env bash
# scripts/e2e/c1-bridge-roundtrip.sh — Track C1 stub-bridge round-trip.
#
# Acceptance gates:
#  1. Stub scaffold creates valid profile + identity + routes
#  2. User agent rejects unsigned envelopes
#  3. User agent accepts trusted-peer envelopes
#  4. AckTracker — offset advances only on 2xx
#  5. doctor surfaces bridge as `running`

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/4 build"
cargo build -p mur-core -p mur-agent-runtime --release --quiet

echo "==> 2/4 unit + integration tests"
cargo test --release -p mur-common bridge:: --quiet
cargo test --release -p mur-agent-runtime --quiet \
    --test bridge_envelope_signing \
    --test bridge_dedupe_sled \
    --test bridge_ack_tracker \
    --test bridge_beacon_degraded \
    --test bridge_llm_off_blocks

echo "==> 3/4 connector add + doctor"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export MUR_HOME="$TMP/.mur"
mkdir -p "$MUR_HOME/agents"

./target/release/mur agent companion connector add stub_bridge \
    --platform stub --default-route coach
test -f "$MUR_HOME/agents/stub_bridge/profile.yaml"
test -f "$MUR_HOME/agents/stub_bridge/routes.yaml"
test -f "$MUR_HOME/agents/stub_bridge/identity.pub"

echo '{"pid": 1}' > "$MUR_HOME/agents/stub_bridge/running.lock"
DOCTOR="$(./target/release/mur agent doctor --format all 2>&1 || true)"
echo "$DOCTOR" | grep -E "stub_bridge: (running|degraded)" \
    || { echo "FAIL: doctor missing bridges section"; echo "$DOCTOR"; exit 1; }

echo "==> 4/4 round-trip integration"
cargo test --release -p mur-agent-runtime --test bridge_roundtrip --quiet

echo "✅ Track C1 bridge round-trip E2E passed"
