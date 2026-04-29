//! `CompanionVoiceHook` — bridges the companion voice composition into
//! the hook chain. Phase 1.1 ships voice as a free function in
//! `companion::voice::compose_*`; this hook stores the pre-composed
//! voice text and injects it as a `PromptPatch::set_system_prefix` on
//! every `on_prompt_submit`.
//!
//! Locale-mismatch detection (i18n) and the linter were applied in
//! companion phase 1.1 in the proactive outbox path; for the reactive
//! prompt path the hook just sets the system prefix. Re-rendering on
//! profile change lands when the supervisor wires hot-reload (out of
//! M0 scope).

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::hooks::{Hook, HookCtx, HookError, MessagePatch, OutboundView, PromptPatch, PromptView};

pub struct CompanionVoiceHook {
    rendered_voice: Arc<String>,
}

impl CompanionVoiceHook {
    /// `rendered_voice` is the output of `companion::voice::compose_in_memory`
    /// (or `compose_with_overrides`) — supervisor renders once at agent
    /// startup based on the active `CompanionConfig`.
    pub fn new(rendered_voice: Arc<String>) -> Self {
        Self { rendered_voice }
    }
}

#[async_trait::async_trait]
impl Hook for CompanionVoiceHook {
    fn name(&self) -> &str {
        "CompanionVoiceHook"
    }

    async fn on_prompt_submit(
        &self,
        _ctx: &HookCtx,
        _view: &PromptView,
        _tok: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        if self.rendered_voice.is_empty() {
            return Ok(PromptPatch::noop());
        }
        Ok(PromptPatch {
            set_system_prefix: Some((*self.rendered_voice).clone()),
            ..PromptPatch::noop()
        })
    }

    async fn on_message_send(
        &self,
        _ctx: &HookCtx,
        view: &OutboundView,
        _tok: &CancellationToken,
    ) -> Result<MessagePatch, HookError> {
        // Phase 1.1's locale tagging stays in the outbox path; for the
        // reactive on_message_send hook we just preserve any existing
        // locale from the OutboundView. M8 (B0) layers the linter +
        // i18n.ensure_locale here.
        let mut patch = MessagePatch::noop();
        if let Some(locale) = view.locale.clone() {
            patch.set_locale = Some(locale);
        }
        Ok(patch)
    }
}
