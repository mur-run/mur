#!/usr/bin/env bash
#
# Murmur P0a E2E smoke runner.
#
# Runs every integration test in mur-agent-runtime + mur-core, plus any
# `#[ignore]`-gated E2E paths (full bin export, etc.). Each test owns its
# own MUR_HOME via tempfile::TempDir, so the suite is hermetic and safe
# to run on a developer machine without touching ~/.mur.
#
# Usage:
#   scripts/e2e/run-all.sh           # default: all integration tests
#   scripts/e2e/run-all.sh --no-ignored
#                                    # skip the slow `--ignored` e2e set
#   scripts/e2e/run-all.sh --coverage
#                                    # also run cargo-llvm-cov (must be installed)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

INCLUDE_IGNORED=1
RUN_COVERAGE=0
for arg in "$@"; do
  case "$arg" in
    --no-ignored)  INCLUDE_IGNORED=0 ;;
    --coverage)    RUN_COVERAGE=1 ;;
    -h|--help)
      sed -n '2,15p' "$0"; exit 0 ;;
    *)
      echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

echo "==> Building workspace (release)…"
cargo build --workspace --release --quiet

echo "==> Pre-building agent runtime debug binary (needed by some integration tests)…"
cargo build -p mur-agent-runtime --bin mur-agent-runtime --quiet

echo "==> Running default integration tests across the workspace…"
cargo test --workspace --all-targets --quiet

if [[ "$INCLUDE_IGNORED" == "1" ]]; then
  echo "==> Running #[ignore]-gated E2E tests…"
  # cargo emits a warning if a target has no #[ignore]'d tests; that's fine.
  cargo test --workspace --all-targets --quiet -- --ignored || {
    echo "ignored-test pass had failures (see output above)" >&2
    exit 1
  }
fi

echo "==> Running companion Phase 1.1 E2E smoke..."
"$REPO_ROOT/scripts/e2e/companion-phase11.sh"

echo "==> Running v1 D1 voice E2E smoke..."
"$REPO_ROOT/scripts/e2e/v1-d1-voice.sh"

echo "==> Running D2 onboarding E2E smoke..."
"$REPO_ROOT/scripts/e2e/v1-d2-onboarding.sh"

echo "==> Running D3 drag-drop E2E smoke..."
"$REPO_ROOT/scripts/e2e/v1-d3-dragdrop.sh"

echo "==> Running D4 character cards E2E smoke..."
"$REPO_ROOT/scripts/e2e/v1-d4-card.sh"

echo "==> Running D5 companion-GUI bridge E2E smoke..."
"$REPO_ROOT/scripts/e2e/v1-d5-gui-bridge.sh"

if [[ "$RUN_COVERAGE" == "1" ]]; then
  if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "cargo-llvm-cov not installed (cargo install cargo-llvm-cov)" >&2
    exit 1
  fi
  echo "==> Generating coverage report…"
  cargo llvm-cov --workspace --summary-only
fi

echo "✅ E2E smoke suite passed"
