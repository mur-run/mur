//! `HookChain` dispatch — phase-aware semantics:
//!
//! * **Gate**: serial; first non-`Allow` `Decision` short-circuits.
//! * **Mutate**: serial; folds patches deterministically.
//! * **Observe**: parallel `join_all`; errors logged, never propagated.
//!
//! Cancellation: hook implementations receive `&CancellationToken`;
//! the dispatcher additionally checks the token between hooks for
//! gate / mutate phases (observe runs the whole batch).

use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::hooks::{
    A2AEnvelopeView, Decision, Hook, HookCtx, HookError, MessagePatch, OutboundView, Phase,
    PostToolUsePatch, PromptPatch, PromptView, ShutdownReason, Step, ToolCall, ToolResult,
    TriggerKind, TriggerPayload,
};
use mur_common::AgentProfile;

#[derive(Clone)]
pub struct HookChain {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookChain {
    pub fn new(hooks: Vec<Arc<dyn Hook>>) -> Self {
        Self { hooks }
    }

    pub fn empty() -> Self {
        Self { hooks: vec![] }
    }

    pub fn names(&self) -> Vec<&str> {
        self.hooks.iter().map(|h| h.name()).collect()
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    // ───── Gate ─────
    pub async fn pre_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        for h in &self.hooks {
            if tok.is_cancelled() {
                return Ok(Decision::Abort);
            }
            match h.pre_tool_use(ctx, call, tok).await? {
                Decision::Allow => continue,
                deny => return Ok(deny),
            }
        }
        Ok(Decision::Allow)
    }

    // ───── Mutate (fold) ─────
    pub async fn on_prompt_submit(
        &self,
        ctx: &HookCtx,
        view: &PromptView,
        tok: &CancellationToken,
    ) -> PromptPatch {
        let mut acc = PromptPatch::noop();
        for h in &self.hooks {
            if tok.is_cancelled() {
                return acc;
            }
            match h.on_prompt_submit(ctx, view, tok).await {
                Ok(p) => acc = acc.merge(p),
                Err(e) => warn!(handler = h.name(), error = %e, "on_prompt_submit failed"),
            }
        }
        acc
    }

    pub async fn on_message_send(
        &self,
        ctx: &HookCtx,
        view: &OutboundView,
        tok: &CancellationToken,
    ) -> MessagePatch {
        let mut acc = MessagePatch::noop();
        for h in &self.hooks {
            if tok.is_cancelled() {
                return acc;
            }
            match h.on_message_send(ctx, view, tok).await {
                Ok(p) => acc = acc.merge(p),
                Err(e) => warn!(handler = h.name(), error = %e, "on_message_send failed"),
            }
        }
        acc
    }

    // ───── Observe (parallel) ─────
    pub async fn on_startup(&self, ctx: &HookCtx, profile: &AgentProfile, tok: &CancellationToken) {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.on_startup(ctx, profile, tok))
            .collect::<Vec<_>>();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            if let Err(e) = res {
                warn!(handler = self.hooks[i].name(), error = %e, "on_startup failed");
            }
        }
    }

    pub async fn on_trigger_fired(
        &self,
        ctx: &HookCtx,
        kind: TriggerKind,
        payload: &TriggerPayload,
        tok: &CancellationToken,
    ) {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.on_trigger_fired(ctx, kind.clone(), payload, tok))
            .collect::<Vec<_>>();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            if let Err(e) = res {
                warn!(handler = self.hooks[i].name(), error = %e, "on_trigger_fired failed");
            }
        }
    }

    pub async fn on_message_received(
        &self,
        ctx: &HookCtx,
        env: &A2AEnvelopeView,
        tok: &CancellationToken,
    ) {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.on_message_received(ctx, env, tok))
            .collect::<Vec<_>>();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            if let Err(e) = res {
                warn!(handler = self.hooks[i].name(), error = %e, "on_message_received failed");
            }
        }
    }

    /// `post_tool_use` is observe-with-mutate-hatch (HOOK_SCHEMA_VERSION
    /// 2). Hooks run in parallel via `join_all`; errors are logged and
    /// not propagated. Successful patches are folded in chain order
    /// using `PostToolUsePatch::merge` (last-write-wins for
    /// `replace_output`) and the merged patch is returned to the
    /// supervisor, which decides whether to rewrite
    /// `ToolResult.output` before persisting.
    pub async fn post_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        result: &ToolResult,
        tok: &CancellationToken,
    ) -> PostToolUsePatch {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.post_tool_use(ctx, call, result, tok))
            .collect::<Vec<_>>();
        let mut acc = PostToolUsePatch::noop();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            match res {
                Ok(p) => acc = acc.merge(p),
                Err(e) => {
                    warn!(handler = self.hooks[i].name(), error = %e, "post_tool_use failed");
                }
            }
        }
        acc
    }

    pub async fn on_step_finish(&self, ctx: &HookCtx, step: &Step, tok: &CancellationToken) {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.on_step_finish(ctx, step, tok))
            .collect::<Vec<_>>();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            if let Err(e) = res {
                warn!(handler = self.hooks[i].name(), error = %e, "on_step_finish failed");
            }
        }
    }

    pub async fn on_error(
        &self,
        ctx: &HookCtx,
        err: &HookError,
        phase: Phase,
        tok: &CancellationToken,
    ) {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.on_error(ctx, err, phase, tok))
            .collect::<Vec<_>>();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            if let Err(e) = res {
                warn!(handler = self.hooks[i].name(), error = %e, "on_error failed");
            }
        }
    }

    pub async fn on_shutdown(
        &self,
        ctx: &HookCtx,
        reason: ShutdownReason,
        tok: &CancellationToken,
    ) {
        let futs = self
            .hooks
            .iter()
            .map(|h| h.on_shutdown(ctx, reason.clone(), tok))
            .collect::<Vec<_>>();
        for (i, res) in futures::future::join_all(futs)
            .await
            .into_iter()
            .enumerate()
        {
            if let Err(e) = res {
                warn!(handler = self.hooks[i].name(), error = %e, "on_shutdown failed");
            }
        }
    }
}
