#!/usr/bin/env bash
# scripts/e2e/v1-d5-gui-bridge.sh — D5 companion-GUI bridge E2E.
#
# Acceptance gates (roadmap §4.5):
# 1. Inbox parser handles every CompanionMessage shape the runtime emits.
# 2. notify-based watcher delivers a fresh .md write within < 1s.
# 3. companion_ack rewrites the response line atomically.
# 4. companion_why returns the ledger chain for one msg_id.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/2 build mur-agent-gui tests (release)"
(cd mur-agent-gui/src-tauri && cargo build --tests --release --quiet)

echo "==> 2/2 D5 bridge gates"
(cd mur-agent-gui/src-tauri && cargo test --release --quiet \
    --test bridge_event_parse \
    --test bridge_scanner \
    --test bridge_watcher \
    --test bridge_state \
    --test bridge_notify \
    --test bridge_ack \
    --test bridge_why \
    --test bridge_acceptance)

echo "✅ D5 companion-GUI bridge E2E passed"
