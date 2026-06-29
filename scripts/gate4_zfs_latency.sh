#!/usr/bin/env bash
set -euo pipefail

echo "=== Gate 4: create_track latency ==="
echo ""

# Run the benchmark test with cargo
ORT_STRATEGY=download cargo test -p mur-core \
  "parallel::backend::detect::tests::bench_create_track" \
  -- --ignored --nocapture 2>&1

echo ""
echo "Pass criteria: ZFS mean ≤ 10ms | git worktree mean ≤ 150ms"
