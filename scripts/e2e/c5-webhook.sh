#!/usr/bin/env bash
# scripts/e2e/c5-webhook.sh — Track C5 webhook acceptance.
#
# Drives the harness-level acceptance for M5.1–M5.5: the config
# schema, Axum handler, supervisor wiring, pipeline bridge, and
# rate limiter. The full bundle-level e2e (real signed bundle +
# `mur agent webhook enable` + curl from outside the box) needs a
# release build and a free port and is documented in the cookbook
# manual matrix instead.
#
# Acceptance gates (spec §5.6):
#  1. `transport.webhook` block deserializes from a synthetic
#     profile.yaml without dropping fields (mur-common).
#  2. `mur agent webhook` CLI verbs compile + serialize correctly
#     (mur-core agent_webhook unit tests).
#  3. Webhook handler accepts signed text, rejects unsigned/wrong-
#     sig/wrong-slug/oversize/malformed/unknown-kind requests
#     (mur-agent-runtime transport::webhook unit tests).
#  4. `dispatch_to_pipeline` writes telemetry/inputs.jsonl with the
#     correct provenance source on text+image kinds, errors clean
#     on invalid base64.
#  5. Token-bucket limiter exhausts → 429 then refills.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/3 mur-common schema (transport.webhook deserialization)"
cargo test -p mur-common --quiet -- --quiet

echo "==> 2/3 mur-core CLI (agent_webhook drift guards)"
cargo test -p mur-core --lib --quiet agent_webhook -- --quiet

echo "==> 3/3 mur-agent-runtime webhook (handler + dispatch + limiter)"
cargo test -p mur-agent-runtime --lib --quiet transport::webhook -- --quiet

echo
echo "OK — Track C5 webhook acceptance gates passed (harness-level)."
echo "Bundle-level QA (real listener bound to localhost, signed curl"
echo "from outside) lives in the cookbook manual matrix at"
echo "docs/cookbook/c5-webhook.md."
