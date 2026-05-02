#!/usr/bin/env bash
# scripts/e2e/v1-d4-card.sh — D4 character cards E2E acceptance.
#
# Acceptance gates (roadmap §4.4):
#   1. CCv3 + extensions.mur schema round-trips (card_schema_roundtrip).
#   2. JCS canonical bytes + Ed25519 sign/verify (card_canonical_signature).
#   3. SillyTavern PNG v3 + v2 importers
#      (card_png_import_v3, card_png_import_v2).
#   4. Character.AI scraped JSON normalizer (card_cai_import).
#   5. Inbox quarantine — import never mutates companion/
#      (card_inbox_quarantine).
#   6. accept_card promotes inbox → profile.companion + writes a
#      `card_import` provenance entry on turn 1 (card_accept_promotes).
#   7. CLI dispatch round-trip (card_cli_dispatch).
#   8. M4.8.1 — a malicious description in data.description still flows
#      through B0SafetyHook: wrapped under source = "card_import" and
#      side-effect tools are denied on the same turn
#      (card_malicious_description_blocks +
#       b0_after_card_import_deny).

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/3 build mur-core (release)"
cargo build --release -p mur-core --quiet

echo "==> 2/3 D4 card schema + import + accept + CLI round-trip"
cargo test --release -p mur-core --quiet \
    --test card_schema_roundtrip \
    --test card_canonical_signature \
    --test card_png_import_v3 \
    --test card_png_import_v2 \
    --test card_cai_import \
    --test card_inbox_quarantine \
    --test card_accept_promotes \
    --test card_cli_dispatch \
    --test card_malicious_description_blocks

echo "==> 3/3 B0SafetyHook handles card_import provenance entries"
cargo test --release -p mur-agent-runtime --quiet \
    --test b0_after_card_import_deny

echo "✅ D4 character cards E2E passed"
