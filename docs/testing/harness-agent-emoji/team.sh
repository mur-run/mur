#!/usr/bin/env bash
# Harness launcher for the tt-* agent team. Starts each mur agent runtime with
# Anthropic-via-bridge credentials so message/send hits real Claude (not echo).
set -u
export MUR_HOME=/tmp/mur-harness-home
source ~/.zshrc 2>/dev/null   # ANTHROPIC_API_KEY (sk-ant-oat*) + ANTHROPIC_BASE_URL (cc-proxy bridge)
REPO=/Volumes/Firecuda4tb/Projects/mur
BIN=/Users/david/.local/bin/mur_agent_
LOGDIR="$MUR_HOME/runlogs"; mkdir -p "$LOGDIR"
AGENTS="tt-pm tt-arch tt-rust tt-devops tt-review tt-sec tt-qa"

start() { /Users/david/.local/bin/mur_agent_"$1" >"$LOGDIR/$1.out" 2>&1 & echo "  started $1 pid=$!"; }
stop()  { "$REPO/target/release/mur" agent stop "$1" >/dev/null 2>&1 && echo "  stopped $1"; }

case "${1:-}" in
  # NOTE: launch start-all via the harness with run_in_background so the
  # `wait` keeps all 7 children alive across tool calls.
  start-all) for n in $AGENTS; do start "$n"; done; wait ;;
  stop-all)  for n in $AGENTS; do stop "$n"; done ;;
  start) start "$2"; wait ;;
  stop)  stop "$2" ;;
  *) echo "usage: $0 {start-all|stop-all|start <name>|stop <name>}"; exit 1 ;;
esac
