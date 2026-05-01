//! `B0SafetyHook` — multimodal rules implementation (M3.8).
//!
//! Currently implements rules 13-22 from roadmap §6.1 (the multimodal
//! consumer-safe baseline). The text-only rules (1-12) wait for M8
//! per the roadmap timeline.
//!
//! Wiring (M3.8.1):
//! * `on_prompt_submit` reads `<agent_home>/telemetry/inputs.jsonl`
//!   for the current turn, looks up each entry's
//!   `<agent_home>/telemetry/inputs/{sha256}.txt` sidecar, and builds
//!   a `PromptPatch` with one `wrap_untrusted` per entry plus the
//!   `after_untrusted_input` turn-flag.
//!
//! The matching `pre_tool_use` gate that consumes the turn-flag and
//! denies side-effect tools lands in M3.8.2. The remaining roadmap
//! rules (sandbox attestation, GrantStore lookup, post_tool_use
//! redaction, on_message_received untrusted flag) land with the M8
//! text-only baseline.

use tokio_util::sync::CancellationToken;

use crate::hooks::{Hook, HookCtx, HookError, PromptPatch, PromptView, UntrustedWrapper};
use mur_common::multimodal::ProvenanceLedger;

/// Turn-flag raised by `on_prompt_submit` when at least one untrusted
/// multimodal artifact was attached to the current turn. Read by
/// `pre_tool_use` to decide whether to gate side-effect tools.
const TURN_FLAG_AFTER_UNTRUSTED: &str = "after_untrusted_input";

pub struct B0SafetyHook;

impl B0SafetyHook {
    pub fn new() -> Self {
        Self
    }
}

impl Default for B0SafetyHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hook for B0SafetyHook {
    fn name(&self) -> &str {
        "B0SafetyHook"
    }

    async fn on_prompt_submit(
        &self,
        ctx: &HookCtx,
        _view: &PromptView,
        _tok: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        let agent_home = ctx.agent_home();
        let turn_id = ctx.turn_id();
        let ledger = ProvenanceLedger::new(agent_home.join("telemetry/inputs.jsonl"));
        let entries = ledger
            .read_turn(turn_id)
            .map_err(|e| HookError::Runtime(format!("read_turn: {e}")))?;
        if entries.is_empty() {
            return Ok(PromptPatch::noop());
        }

        let mut wrappers = Vec::with_capacity(entries.len());
        for e in entries {
            let txt_path = agent_home
                .join("telemetry/inputs")
                .join(format!("{}.txt", e.sha256));
            // M3.8.0 always writes the sidecar (empty file when there's no
            // text). A missing file means we're reading an older ledger
            // pre-M3.8.0 — fall back to an empty wrapper rather than
            // silently dropping the provenance entry.
            let content = std::fs::read_to_string(&txt_path).unwrap_or_default();
            // Heuristic: PDF entries begin with the "--- page" marker that
            // the pipeline (M3.4.2) prepends per page. Everything else is
            // treated as image OCR text. The tag drives prompt
            // spotlighting downstream.
            let tag = if content.contains("--- page") {
                "untrusted_pdf_text"
            } else {
                "untrusted_image_text"
            };
            wrappers.push(UntrustedWrapper {
                tag: tag.into(),
                source: e.source.clone(),
                content,
            });
        }

        Ok(PromptPatch {
            wrap_untrusted: wrappers,
            turn_flags: vec![TURN_FLAG_AFTER_UNTRUSTED.into()],
            ..PromptPatch::noop()
        })
    }
}
