#!/usr/bin/env bash
# scripts/e2e/browser-live-f1.sh — F1 "Comparison runs" for browser live mode.
#
# The spec's acceptance row (docs/superpowers/specs/2026-10-01-browser-live-mode-design.md,
# "Acceptance (v1)", F1): an agent opens both fixture shops, reads both
# prices, reports both and names the cheaper one. Everything the browser
# reaches goes through the runtime's egress proxy with a Restricted allowlist
# of exactly `127.0.0.1`.
#
# This is the LLM-in-the-loop version. The tool-level version (no model, same
# proxy, same fixture) is `cargo test -p mur-core --test browser_live_f1` with
# `MUR_BROWSER_E2E=1`, and runs first here as a gate.
#
# Must run OUTSIDE any agent seal: Chromium needs to spawn, and openssl mints
# the fixture's certificate. A plain terminal or CI is fine.
#
# Needs: a MUR agent runtime that can start, `mur browser setup` done once on
# this machine (npx + @playwright/mcp + chromium headless shell), python3,
# openssl, and a model the agent can call.
#
# Usage:
#   scripts/e2e/browser-live-f1.sh [--model <id>] [--provider <name>] [--keep]
#
#   --model / --provider   forwarded to `mur agent create` (default: the
#                          `mur agent create` defaults on this machine)
#   --keep                 leave the agent and the fixture running for a
#                          manual look (prints the cleanup commands)
#
# Everything it creates is namespaced `e2e_browser_live` and removed on exit.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

AGENT="e2e_browser_live"
MODEL_ARGS=()
KEEP=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)    MODEL_ARGS+=(--model "$2"); shift 2 ;;
    --provider) MODEL_ARGS+=(--provider "$2"); shift 2 ;;
    --keep)     KEEP=1; shift ;;
    -h|--help)  sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

MUR="${MUR:-$REPO_ROOT/target/debug/mur}"
if [[ ! -x "$MUR" ]]; then
  echo "==> Building mur…" >&2
  cargo build -p mur-core --bin mur --quiet
fi

for bin in python3 openssl npx; do
  command -v "$bin" >/dev/null || { echo "FAIL: $bin not on PATH" >&2; exit 1; }
done

FIXTURE_PID=""
cleanup() {
  local rc=$?
  if [[ $KEEP -eq 1 && $rc -eq 0 ]]; then
    echo
    echo "--keep: left running. Clean up with:"
    echo "  kill $FIXTURE_PID"
    echo "  $MUR agent stop $AGENT; $MUR agent remove $AGENT --purge --force"
    return
  fi
  [[ -n "$FIXTURE_PID" ]] && kill "$FIXTURE_PID" 2>/dev/null || true
  "$MUR" agent stop "$AGENT" >/dev/null 2>&1 || true
  "$MUR" agent remove "$AGENT" --purge --force >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> 0/5 tool-level gate (same proxy, same fixture, no model)"
MUR_BROWSER_E2E=1 cargo test -p mur-core --test browser_live_f1 --quiet -- --quiet

echo "==> 1/5 fixture: two HTTPS shops on 127.0.0.1"
FIXTURE_OUT="$(mktemp)"
python3 scripts/e2e/browser-live-fixture.py --parent-pid $$ >"$FIXTURE_OUT" 2>/tmp/browser-live-f1-fixture.log &
FIXTURE_PID=$!
for _ in $(seq 1 50); do
  [[ -s "$FIXTURE_OUT" ]] && break
  sleep 0.1
done
[[ -s "$FIXTURE_OUT" ]] || { echo "FAIL: fixture did not announce itself" >&2; exit 1; }
ANNOUNCE="$(head -n1 "$FIXTURE_OUT")"
field() { python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(eval("d"+sys.argv[2]))' "$ANNOUNCE" "$1"; }
URL_A="$(field '["shops"]["starling"]["url"]')"
URL_B="$(field '["shops"]["magpie"]["url"]')"
PRICE_A="$(field '["shops"]["starling"]["price"]')"
PRICE_B="$(field '["shops"]["magpie"]["price"]')"
CHEAPER="$(field '["cheaper"]')"
CHEAPER_NAME="$(field "[\"shops\"][\"$CHEAPER\"][\"name\"]")"
echo "    A $URL_A  $PRICE_A"
echo "    B $URL_B  $PRICE_B"
echo "    cheaper: $CHEAPER_NAME"

echo "==> 2/5 agent with a Restricted live-mode browser entry"
"$MUR" agent remove "$AGENT" --purge --force >/dev/null 2>&1 || true
"$MUR" agent create "$AGENT" --no-interactive "${MODEL_ARGS[@]}" >/dev/null
# The entry: `mur browser record --mode live`. Trailing args reach
# @playwright/mcp verbatim; the fixture's cert is self-signed.
yes | "$MUR" agent mcp add "$AGENT" browser --no-probe --force \
  --command mur --arg browser --arg record --arg --run --arg live --arg --mode --arg live \
  --arg=-- --arg=--ignore-https-errors >/dev/null
"$MUR" agent mcp set-network "$AGENT" browser --allow-host 127.0.0.1 >/dev/null
# What `mur browser record` spawns on the agent's behalf (perms.rs REQUIRED_BINARIES).
"$MUR" agent perm allow-spawn "$AGENT" playwright-mcp >/dev/null
"$MUR" agent perm allow-spawn "$AGENT" chrome-headless-shell >/dev/null
# Read-only browser tools need no card for F1; F6 covers the ones that do.
"$MUR" agent perm tool-allow "$AGENT" 'mcp__browser__browser_navigate' >/dev/null
"$MUR" agent perm tool-allow "$AGENT" 'mcp__browser__browser_snapshot' >/dev/null
"$MUR" agent perm tool-allow "$AGENT" 'mcp__browser__browser_navigate_back' >/dev/null
"$MUR" agent perm tool-allow "$AGENT" 'mcp__browser__browser_tabs' >/dev/null

echo "==> 3/5 start"
"$MUR" agent start "$AGENT" >/dev/null
for _ in $(seq 1 100); do
  "$MUR" agent status "$AGENT" 2>/dev/null | grep -q "Active: running" && break
  sleep 0.2
done
"$MUR" agent status "$AGENT" | head -3

echo "==> 4/5 the comparison"
TASK="Compare the price of the same product on these two shop pages. Open each page with the browser, read the price shown on it, then answer with BOTH prices exactly as written and the name of the cheaper shop. Shop pages: $URL_A and $URL_B"
MSG="$(python3 -c 'import json,sys; print(json.dumps({"role":"user","parts":[{"kind":"text","text":sys.argv[1]}]}))' "$TASK")"
REPLY="$("$MUR" agent send "$AGENT" "$MSG" 2>&1 || true)"
echo "$REPLY" | sed 's/^/    | /' | head -40

echo "==> 5/5 verdict"
fail=0
for want in "$PRICE_A" "$PRICE_B" "$CHEAPER_NAME"; do
  if echo "$REPLY" | grep -qF -- "$want"; then
    echo "    ✓ reply contains: $want"
  else
    echo "    ✗ reply missing: $want"
    fail=1
  fi
done
echo "    fixture saw:"
sed 's/^/      /' /tmp/browser-live-f1-fixture.log
if grep -q 'host=localhost' /tmp/browser-live-f1-fixture.log; then
  echo "    ✗ a request reached the fixture as 'localhost' — the proxy allowlist was bypassed"
  fail=1
fi

if [[ $fail -ne 0 ]]; then
  echo "FAIL: F1 comparison run" >&2
  echo "agent log tail:" >&2
  "$MUR" agent logs "$AGENT" --tail 40 >&2 || true
  exit 1
fi
echo "OK — F1: both prices read through the egress proxy, cheaper shop named."
