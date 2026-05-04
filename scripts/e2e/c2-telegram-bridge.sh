#!/usr/bin/env bash
# scripts/e2e/c2-telegram-bridge.sh — Track C2 Telegram bridge acceptance.
#
# Acceptance gates (roadmap §5.4):
#  1. CLI scaffold writes telegram.yaml under $MUR_HOME/agents/<bridge>/
#     (mocked BotFather + mocked keychain via MUR_TELEGRAM_KEYCHAIN_BACKEND).
#  2. Inbound text round-trip — long-poll + dedupe + ACK + sign.
#  3. Voice path — getFile + whisper-rs transcription wrapper.
#  4. Document / photo path — multimodal pipeline (D3) + B0 wrapping.
#  5. Outbound MCP `chat.send_message` — token-bucket rate-limit kicks in.
#
# Notes:
#  - First run does a release build of mur-core + mur-agent-runtime (cold
#    cache can take 4-6 minutes on a laptop; warm cache is sub-30s).
#  - All cargo tests run under `--test-threads=1` because the integration
#    tests share the keychain env var and mock backend.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/6 build mur-core + mur-agent-runtime (release)"
cargo build -p mur-core -p mur-agent-runtime --release --quiet

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
export MUR_HOME="$WORKDIR"
export MUR_TELEGRAM_KEYCHAIN_BACKEND=mock
mkdir -p "$MUR_HOME/agents"

echo "==> 2/6 scaffold telegram bridge (mock keychain)"
./target/release/mur agent companion connector add tg \
    --platform telegram \
    --default-route coach \
    --bot-token "1234:fakefake" \
    --bot-username MyAgentBot \
    --chat-id 100 \
    --ack
test -f "$MUR_HOME/agents/tg/telegram.yaml" \
    || { echo "FAIL: telegram.yaml not written"; exit 1; }
test -f "$MUR_HOME/agents/tg/profile.yaml" \
    || { echo "FAIL: profile.yaml not written"; exit 1; }

echo "==> 3/6 inbound text round-trip"
cargo test --release -p mur-agent-runtime --quiet \
    --test c2_telegram_inbound -- --test-threads=1

echo "==> 4/6 voice round-trip (whisper-rs wrapper)"
cargo test --release -p mur-agent-runtime --quiet \
    --test c2_telegram_voice -- --test-threads=1

echo "==> 5/6 document / photo round-trip (B0 multimodal)"
cargo test --release -p mur-agent-runtime --quiet \
    --test c2_telegram_files -- --test-threads=1

echo "==> 6/6 outbound MCP + rate-limit"
cargo test --release -p mur-agent-runtime --quiet \
    --test c2_telegram_outbound -- --test-threads=1

echo "✅ Track C2 telegram bridge E2E passed"
