#!/usr/bin/env bash
#
# v1 D1 voice end-to-end acceptance.
#
# Static-analysis level — no actual ONNX or whisper inference (those
# need ~900 MB of model fixtures that don't ship in CI). Verifies:
#
#   1. mur-agent-gui builds clean across the configured targets.
#   2. All voice unit + integration tests pass.
#   3. Frontend (Vite + tsc strict) builds clean.
#   4. cargo fmt + clippy clean on voice modules.
#   5. mur agent doctor still works (regression gate from M0).
#
# Functional voice inference is verified by the M1.6.4 bench harness
# (deferred to v1.0 release; ships the GGUF + ONNX fixtures) and by
# the manual acceptance demo flow on dev hardware.
#
# Usage:
#   scripts/e2e/v1-d1-voice.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

GUI_MANIFEST="mur-agent-gui/src-tauri/Cargo.toml"

echo "==> Frontend deps (mur-agent-gui/ui)"
(cd mur-agent-gui/ui && npm ci --silent --prefer-offline)

echo "==> Frontend build (tsc strict + vite)"
(cd mur-agent-gui/ui && npm run build)

echo "==> Backend cargo build (mur-agent-gui)"
cargo build --manifest-path "$GUI_MANIFEST" --quiet

echo "==> Voice unit + integration tests"
cargo test --manifest-path "$GUI_MANIFEST" --lib voice --quiet
cargo test --manifest-path "$GUI_MANIFEST" --test voice_manifest --quiet

echo "==> cargo fmt --check (mur-agent-gui)"
cargo fmt --manifest-path "$GUI_MANIFEST" -- --check

echo "==> cargo clippy --lib (mur-agent-gui, deny warnings)"
cargo clippy --manifest-path "$GUI_MANIFEST" --lib -- -D warnings

echo "==> mur agent doctor (regression gate from M0)"
cargo run --quiet -p mur-core -- agent doctor --json | jq -e '.[] | select(.name == "hooks") | .status == "ok"' >/dev/null

echo "✅ v1 D1 voice e2e: PASS"
