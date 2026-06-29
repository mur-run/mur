#!/usr/bin/env bash
# Gate 7: P3 concurrent merge zero-dep round-trip (no live agents).
# Pass criterion: all concurrent-merge unit tests green.
set -euo pipefail
echo "Gate 7: concurrent merge (StructuralMerger + hunk + stats + cmd)"
ORT_STRATEGY=download MUR_WEB_DIST="${MUR_WEB_DIST:-$HOME/Projects/mur-web/dist}" \
    cargo test -p mur-core --lib concurrent -- --nocapture
echo ""
echo "GATE 7: ✅ PASS"
