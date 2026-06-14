//! 10-method async `Hook` trait. Default impls are no-ops; the chain dispatcher
//! treats methods according to their phase semantics:
//!
//! * **Gate** (`pre_tool_use`): serial + short-circuit on `Decision != Allow`.
//! * **Mutate** (`on_prompt_submit`, `on_message_send`): serial + fold patches.
//! * **Observe** (the other 7): parallel `join_all`; errors logged not propagated.
//!
//! The trait surface is **frozen** (A0 contract). User-extensibility (config-driven
//! handler picker, plugins, WASM, scripted, visual editor) lands in A1+ (v2)
//! without changing this surface — see roadmap §3.3.

pub mod chain;
pub mod decision;
pub mod patch;
pub mod types;

pub mod b0;
pub mod b0_helpers;
pub mod builder;
pub mod companion_voice;
pub mod compress;
pub mod ledger;
pub mod telemetry;
pub mod trash_guard;
pub mod voice_input;

pub use b0::B0SafetyHook;
pub use chain::HookChain;
pub use compress::CompressHook;
pub use decision::Decision;
pub use patch::{MessagePatch, PostToolUsePatch, PromptPatch, UntrustedWrapper};
pub use trash_guard::TrashGuard;
pub use types::{
    A2AEnvelopeView, AskDefault, ErrorAction, HookCtx, HookError, OutboundView, Phase, PromptView,
    ShutdownReason, Step, TelemetryEmitter, ToolCall, ToolResult, TriggerKind, TriggerPayload,
};

use mur_common::AgentProfile;
use tokio_util::sync::CancellationToken;

/// Schema version for the frozen hook trait surface. Any breaking change
/// (method rename, signature change, dispatch semantics shift, new phase
/// inserted) must bump this value.
///
/// Version history:
/// * v1 — initial freeze (10 methods, observe phase returns `()`).
/// * v2 — `post_tool_use` returns `Result<PostToolUsePatch, HookError>`
///   (was `Result<(), HookError>`) so B0 rule 8 (M7.6) can redact
///   PII in `memory.*` tool outputs before they're persisted.
pub const HOOK_SCHEMA_VERSION: u32 = 2;

#[async_trait::async_trait]
pub trait Hook: Send + Sync {
    /// Identifier for telemetry / logging (e.g. "TelemetryHook").
    fn name(&self) -> &str {
        "Hook"
    }

    // ───── Gate: serial + short-circuit on Decision != Allow ─────
    async fn pre_tool_use(
        &self,
        _ctx: &HookCtx,
        _call: &ToolCall,
        _tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        Ok(Decision::Allow)
    }

    // ───── Mutate: serial + fold patches ─────
    async fn on_prompt_submit(
        &self,
        _ctx: &HookCtx,
        _view: &PromptView,
        _tok: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        Ok(PromptPatch::noop())
    }

    async fn on_message_send(
        &self,
        _ctx: &HookCtx,
        _view: &OutboundView,
        _tok: &CancellationToken,
    ) -> Result<MessagePatch, HookError> {
        Ok(MessagePatch::noop())
    }

    // ───── Observe: parallel join_all; errors logged, never propagated ─────
    async fn on_startup(
        &self,
        _ctx: &HookCtx,
        _profile: &AgentProfile,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        Ok(())
    }

    async fn on_trigger_fired(
        &self,
        _ctx: &HookCtx,
        _trigger: TriggerKind,
        _payload: &TriggerPayload,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        Ok(())
    }

    async fn on_message_received(
        &self,
        _ctx: &HookCtx,
        _envelope: &A2AEnvelopeView,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        Ok(())
    }

    /// Observe phase with an opt-in mutate hatch: hooks that don't need
    /// to mutate the result return `PostToolUsePatch::default()`. The
    /// chain runner folds patches (last-write-wins for
    /// `replace_output`) and returns the merged patch to the supervisor
    /// so it can rewrite `ToolResult.output` before persistence.
    async fn post_tool_use(
        &self,
        _ctx: &HookCtx,
        _call: &ToolCall,
        _result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<PostToolUsePatch, HookError> {
        Ok(PostToolUsePatch::default())
    }

    async fn on_step_finish(
        &self,
        _ctx: &HookCtx,
        _step: &Step,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        Ok(())
    }

    async fn on_error(
        &self,
        _ctx: &HookCtx,
        _err: &HookError,
        _phase: Phase,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        Ok(())
    }

    async fn on_shutdown(
        &self,
        _ctx: &HookCtx,
        _reason: ShutdownReason,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        Ok(())
    }
}
