#!/usr/bin/env bash
# E2E smoke for the model registry: create an agent, rewrite its profile to
# use `model_ref:`, run it under MUR_AGENT_FORCE_ECHO=1, send a ping, expect
# the echo runner's reply.
#
# This script is hermetic — it stands up a fresh $HOME inside a tempdir so
# it doesn't touch the developer's real ~/.mur.

set -euo pipefail

if [[ "${VERBOSE:-0}" == "1" ]]; then
    set -x
fi

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

TMP=$(mktemp -d)
cleanup() {
    if [[ -n "${RUNTIME_PID:-}" ]] && kill -0 "$RUNTIME_PID" 2>/dev/null; then
        kill "$RUNTIME_PID" 2>/dev/null || true
        wait "$RUNTIME_PID" 2>/dev/null || true
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT

export HOME="$TMP"
mkdir -p "$HOME/.mur"

cat > "$HOME/.mur/models.yaml" <<'YAML'
schema_version: 1
models:
  echo_dev:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
YAML

echo "==> Building mur (release)" >&2
cargo build --release --bin mur --bin mur-agent-runtime --quiet

MUR="$REPO_ROOT/target/release/mur"

echo "==> Creating agent echo_a" >&2
"$MUR" agent create echo_a --provider ollama --model llama3.2:3b --yes

# Rewrite the inline model: block to a model_ref pointer. We keep the inline
# block (legacy fallback) but also add model_ref; resolve_model_entry prefers
# model_ref when present.
PROFILE="$HOME/.mur/agents/echo_a/profile.yaml"
if ! grep -q '^model_ref:' "$PROFILE"; then
    printf '\nmodel_ref: echo_dev\n' >> "$PROFILE"
fi

echo "==> Starting runtime under MUR_AGENT_FORCE_ECHO=1" >&2
MUR_AGENT_FORCE_ECHO=1 "$MUR" agent start echo_a >/tmp/mur-echo-a.log 2>&1 &
RUNTIME_PID=$!
sleep 2

if ! kill -0 "$RUNTIME_PID" 2>/dev/null; then
    echo "FAIL: runtime exited prematurely" >&2
    cat /tmp/mur-echo-a.log >&2 || true
    exit 1
fi

echo "==> Sending ping" >&2
OUT=$("$MUR" agent send echo_a 'ping' 2>&1 || true)
echo "$OUT"

if ! echo "$OUT" | grep -qi 'echo'; then
    echo "FAIL: expected echo runner output, got:" >&2
    echo "$OUT" >&2
    exit 1
fi

"$MUR" agent stop echo_a >/dev/null 2>&1 || true
echo "OK: registry-driven echo smoke passed"
