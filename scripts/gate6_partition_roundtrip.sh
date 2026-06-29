#!/usr/bin/env bash
# Gate 6: Semantic Partition Mode deterministic round-trip (no live agents).
#
# Verifies the partition planner + merger produce a fully-implemented file from
# disjoint per-agent regions, via Rust unit tests of the model pipeline.
#
# Pass criterion: all partition unit tests green.
#
# Usage: bash scripts/gate6_partition_roundtrip.sh
set -euo pipefail

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${PWD}/target}"
export ORT_STRATEGY="${ORT_STRATEGY:-download}"
export CARGO_TARGET_DIR

echo "Gate 6: partition planner + merge round-trip"
cargo test -p mur-core --lib parallel::partition -- --nocapture
cargo test -p mur-core --lib cmd::fleet::partition_cmd -- --nocapture
cargo test -p mur-core --lib cmd::fleet::run::tests::partition -- --nocapture

echo ""
echo "GATE 6: PASS"
