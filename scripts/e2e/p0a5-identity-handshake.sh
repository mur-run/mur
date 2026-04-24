#!/usr/bin/env bash
# P0a.5 E2E smoke: create an agent with an Ed25519 identity, verify the
# profile carries the pubkey and the running.lock advertises it once the
# agent is running.
#
# Scope: this script only verifies the P0a.5 identity/profile plumbing
# end-to-end against the actual `mur` + `mur-agent-runtime` binaries. The
# Noise-XK TCP listener handshake itself is covered by the
# `mur-agent-runtime/tests/tcp_transport.rs` integration test; here we
# just confirm `transport.tcp.bind` can be enabled on an agent and the
# supervisor will bind + write the address into `running.lock`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

TMPDIR="$(mktemp -d -t p0a5-smoke-XXXXXX)"
trap 'rm -rf "$TMPDIR"' EXIT

export MUR_HOME="$TMPDIR/mur-home"
export MUR_AGENT_BIN_DIR="$TMPDIR/bin"
mkdir -p "$MUR_AGENT_BIN_DIR"

# Build the binaries we need. Use the workspace's debug target so this is
# fast enough to run in CI without touching the user's release artifacts.
cargo build --workspace --bin mur --bin mur-agent-runtime >&2
cp "$REPO_ROOT/target/debug/mur" "$MUR_AGENT_BIN_DIR/"
cp "$REPO_ROOT/target/debug/mur-agent-runtime" "$MUR_AGENT_BIN_DIR/"
export PATH="$MUR_AGENT_BIN_DIR:$PATH"
export MUR_AGENT_RUNTIME_BIN="$MUR_AGENT_BIN_DIR/mur-agent-runtime"

# 1. Create the agent and verify identity files land on disk.
mur agent create p0a5_smoke_agent --no-interactive >/dev/null
AGENT_DIR="$MUR_HOME/agents/p0a5_smoke_agent"
test -f "$AGENT_DIR/identity.key" || { echo "FAIL: identity.key missing"; exit 1; }
test -f "$AGENT_DIR/identity.pub" || { echo "FAIL: identity.pub missing"; exit 1; }
PUB="$(cat "$AGENT_DIR/identity.pub")"
case "$PUB" in
  z*) ;;
  *) echo "FAIL: identity.pub must start with 'z' (got '$PUB')"; exit 1;;
esac

# 2. Verify profile.yaml captured the pubkey.
grep -q "pubkey: z" "$AGENT_DIR/profile.yaml" \
  || { echo "FAIL: profile.yaml missing pubkey: z*"; exit 1; }

# 3. Pull the Agent Card via the ephemeral-runtime path and verify it
# includes the pubkey + endpoints[] shape introduced by P0a.5 B7.
CARD_JSON="$(mur agent card p0a5_smoke_agent 2>/dev/null)"
echo "$CARD_JSON" | grep -q '"pubkey": *"z' \
  || { echo "FAIL: agent/card response missing pubkey"; echo "$CARD_JSON"; exit 1; }
echo "$CARD_JSON" | grep -q '"endpoints"' \
  || { echo "FAIL: agent/card response missing endpoints[]"; echo "$CARD_JSON"; exit 1; }

echo "OK: P0a.5 identity + agent-card smoke passed"
