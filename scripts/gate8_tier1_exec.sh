#!/usr/bin/env bash
# Gate 8: Tier 1 worktree-execution wiring (no live agents).
# Pass criterion: the run-module unit tests are green — gate, fan-out cap, and
# per-track cwd routing — plus the no-`parallel:` path stays unchanged.
set -euo pipefail
echo "Gate 8: Tier 1 worktree execution (run-module wiring)"
ORT_STRATEGY=download MUR_WEB_DIST="${MUR_WEB_DIST:-$HOME/Projects/mur-web/dist}" \
    cargo test -p mur-core --lib cmd::fleet::run -- --nocapture
echo ""
echo "GATE 8: PASS"
