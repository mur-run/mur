//! `B0SafetyHook` — multimodal rules implementation (M3.8).
//!
//! Currently implements rules 13-22 from roadmap §6.1 (the multimodal
//! consumer-safe baseline). The text-only rules (1-12) wait for M8
//! per the roadmap timeline.
//!
//! Wiring:
//! * `on_prompt_submit` (M3.8.1) reads
//!   `<agent_home>/telemetry/inputs.jsonl` for the current turn, looks
//!   up each entry's `<agent_home>/telemetry/inputs/{sha256}.txt`
//!   sidecar, and builds a `PromptPatch` with one `wrap_untrusted` per
//!   entry plus the `after_untrusted_input` turn-flag.
//! * `pre_tool_use` (M3.8.2) checks for the `after_untrusted_input`
//!   turn-flag and denies side-effect tools (delete / spawn / send /
//!   egress / network / .write / .publish) via `Decision::AskUser`,
//!   per roadmap §4.3 step 9.
//!
//! The remaining roadmap rules (sandbox attestation, GrantStore
//! lookup, post_tool_use redaction, on_message_received untrusted
//! flag) land with the M8 text-only baseline.

use tokio_util::sync::CancellationToken;

use crate::hooks::{
    AskDefault, Decision, Hook, HookCtx, HookError, PromptPatch, PromptView, ToolCall,
    UntrustedWrapper,
};
use mur_common::multimodal::ProvenanceLedger;
use mur_common::permissions::ScopeKey;

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

    async fn pre_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        _tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        if !ctx
            .turn_flags()
            .iter()
            .any(|f| f == TURN_FLAG_AFTER_UNTRUSTED)
        {
            return Ok(Decision::Allow);
        }
        if !is_side_effect_tool(call.name()) {
            return Ok(Decision::Allow);
        }
        // M3.8.2 only needs the scope key as a stable identifier for the
        // user-facing prompt; the per-input schema-hash dimension belongs
        // to the M8 GrantStore lookup. Tag the key with the rule name so
        // future grants are scoped to "after-untrusted-input + this tool"
        // rather than the tool alone.
        let scope_key = ScopeKey {
            agent_id: ctx.agent_uuid.clone(),
            tool_name: format!("{}::{}", TURN_FLAG_AFTER_UNTRUSTED, call.name()),
            input_schema_hash: String::new(),
        };
        Ok(Decision::AskUser {
            scope_key,
            prompt: format!(
                "An attached image or PDF may contain instructions. Allow `{}` to run anyway?",
                call.name()
            ),
            default: AskDefault::Deny,
        })
    }
}

/// Side-effect tool name classifier. Errs on the safe side: better to
/// ask once than to silently fire a deletion / spawn / egress.
///
/// The match list mirrors roadmap §4.3 step 9 (`delete*`, `spawn*`,
/// `*.send`, `egress*`, `network.*`, `.write`, `.publish`).
fn is_side_effect_tool(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("delete")
        || n.ends_with("delete")
        || n.starts_with("spawn")
        || n.ends_with("spawn")
        || n.contains(".send")
        || n.starts_with("send")
        || n.starts_with("egress")
        || n.starts_with("network.")
        || n.contains(".write")
        || n.contains(".publish")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_effect_classifier_matches_expected_names() {
        for n in [
            "fs.delete",
            "delete_user",
            "process.spawn",
            "spawn_shell",
            "messaging.send",
            "send_email",
            "egress.http",
            "network.http_get",
            "fs.write",
            "feed.publish",
        ] {
            assert!(is_side_effect_tool(n), "{n} should be side-effect");
        }
        for n in ["fs.read", "search.query", "patterns.list", "config.get"] {
            assert!(!is_side_effect_tool(n), "{n} should be allowed");
        }
    }
}
