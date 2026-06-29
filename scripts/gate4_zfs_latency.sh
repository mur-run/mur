#!/usr/bin/env bash
set -euo pipefail

echo "=== Gate 4: create_track latency ==="
echo ""

# Backend is auto-detected via detect_backend():
#   ZFS native  — Linux/FreeBSD with 'zfs' CLI available
#   OrbStack    — macOS with mur-zfs-agent socket at ~/.orbstack/run/sockets/
#   Lima        — macOS/Linux with Lima VM named "mur-zfs"
#   git worktree — all other machines (macOS dev, CI, etc.)
#
# For git baseline: run on a machine without ZFS (macOS without OrbStack/Lima).
# For ZFS path:     run on a Linux host with 'zfs' installed and project on a ZFS pool.
# Run the benchmark test with cargo
ORT_STRATEGY=download cargo test -p mur-core \
  "parallel::backend::detect::tests::bench_create_track" \
  -- --ignored --nocapture 2>&1

echo ""
echo "Pass criteria: ZFS mean ≤ 10ms | git worktree mean ≤ 150ms"
