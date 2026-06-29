#!/usr/bin/env bash
# Gate 0 — Parallel Tracks PoC validation
# Validates A6 (tree-sitter extraction reliability); documents A1 (approach diversity) as a manual step.
#
# Usage: bash scripts/parallel_poc.sh
# Prereqs: ORT_STRATEGY=download, mur-core builds

set -euo pipefail

echo "=== Gate 0: Parallel Tracks PoC Validation ==="
echo ""
echo "--- A6: tree-sitter extraction reliability ---"
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::tests::gate0_tree_sitter_extraction_on_real_source 2>&1 | tail -5
echo "A6 PASS: tree-sitter extraction verified on real mur-core source."
echo ""
echo "--- A1: approach diversity (manual step) ---"
echo "To validate that diverse approach prompts produce different implementations:"
echo "  1. Run: mur fleet run <fleet-name>  (with 2+ tracks configured)"
echo "  2. Run: mur fleet judge <fleet-name>"
echo "  3. Run: mur fleet compare <fleet-name>"
echo "  4. Check that scores differ across tracks (similarity <= 0.60)"
echo ""
echo "Save results to: docs/superpowers/validation/parallel-poc-results.md"
