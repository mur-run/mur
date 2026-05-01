#!/usr/bin/env bash
# scripts/e2e/v1-d3-dragdrop.sh — D3 drag-drop pipeline E2E acceptance.
#
# Acceptance gates (roadmap §4.3):
# 1. Sandboxed decoder + Unicode scrubber + HEIC EXIF strip
#    (multimodal_decoder_e2e + multimodal_unicode_scrubber + multimodal_heic).
# 2. PDF prompt-injection scenario: invisible <1pt text extracted +
#    quarantined + wrapped in <untrusted_pdf_text>
#    (multimodal_pdf_js_disabled + multimodal_pipeline_pdf + b0_untrusted_wrapper).
# 3. Side-effect tool deny on the same turn after untrusted input
#    (b0_side_effect_deny).

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/3 build mur-agent-decoder + GUI tests (release)"
(cd mur-agent-gui/src-tauri && cargo build --tests --bin mur-agent-decoder --release --quiet)

echo "==> 2/3 multimodal pipeline gates"
(cd mur-agent-gui/src-tauri && cargo test --release --quiet \
    --test multimodal_decoder_e2e \
    --test multimodal_decoder_protocol \
    --test multimodal_dedupe \
    --test multimodal_heic \
    --test multimodal_icloud_fallback \
    --test multimodal_pdf_js_disabled \
    --test multimodal_pipeline_image \
    --test multimodal_pipeline_pdf \
    --test multimodal_unicode_scrubber \
    --test multimodal_ocr_dispatch \
    --test multimodal_commands)

echo "==> 3/3 B0SafetyHook acceptance"
cargo test --release -p mur-agent-runtime --quiet \
    --test b0_untrusted_wrapper \
    --test b0_side_effect_deny

echo "✅ D3 drag-drop E2E passed"
