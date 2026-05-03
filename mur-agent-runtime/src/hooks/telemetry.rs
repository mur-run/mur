//! `TelemetryHook` — emits an OTel-GenAI 2026 span event for every fired
//! hook via the `TelemetryEmitter` trait carried in `HookCtx`.
//!
//! Sensitive payloads (`gen_ai.input.messages`, `output.messages`,
//! `tool.call.{arguments,result}`) are intentionally NOT included here;
//! capturing them is opt-in via
//! `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true` and
//! mur's redaction pipeline (M8 / B0).

use mur_common::AgentProfile;
use mur_common::telemetry::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::hooks::{
    A2AEnvelopeView, Hook, HookCtx, HookError, MessagePatch, OutboundView, Phase, PostToolUsePatch,
    PromptPatch, PromptView, ShutdownReason, Step, ToolCall, ToolResult, TriggerKind,
    TriggerPayload,
};

pub struct TelemetryHook;

impl TelemetryHook {
    pub fn new() -> Self {
        Self
    }

    async fn emit(&self, ctx: &HookCtx, phase: Phase, attrs: serde_json::Value) {
        let merged = json!({
            MUR_AGENT_NAME: ctx.agent_name,
            MUR_AGENT_UUID: ctx.agent_uuid,
            MUR_TASK_ID: ctx.run_id,
            MUR_HOOK_NAME: "TelemetryHook",
            MUR_HOOK_PHASE: format!("{phase:?}"),
            "attrs": attrs,
        });
        ctx.telemetry
            .emit_span_event(METHOD_HOOK_FIRED, merged)
            .await;
    }
}

impl Default for TelemetryHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hook for TelemetryHook {
    fn name(&self) -> &str {
        "TelemetryHook"
    }

    async fn on_startup(
        &self,
        ctx: &HookCtx,
        profile: &AgentProfile,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.emit(
            ctx,
            Phase::Startup,
            json!({
                GEN_AI_AGENT_ID: ctx.agent_uuid,
                GEN_AI_AGENT_NAME: profile.name,
                GEN_AI_OPERATION_NAME: "create_agent",
            }),
        )
        .await;
        Ok(())
    }

    async fn on_trigger_fired(
        &self,
        ctx: &HookCtx,
        kind: TriggerKind,
        _payload: &TriggerPayload,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.emit(
            ctx,
            Phase::TriggerFired,
            json!({ MUR_TRIGGER_KIND: format!("{kind:?}") }),
        )
        .await;
        Ok(())
    }

    async fn on_message_received(
        &self,
        ctx: &HookCtx,
        env: &A2AEnvelopeView,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.emit(
            ctx,
            Phase::MessageReceived,
            json!({
                GEN_AI_OPERATION_NAME: "invoke_agent",
                "method": env.method,
                MUR_A2A_PEER_PUBKEY: env.from_pubkey,
            }),
        )
        .await;
        Ok(())
    }

    async fn on_prompt_submit(
        &self,
        ctx: &HookCtx,
        _: &PromptView,
        _: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        self.emit(
            ctx,
            Phase::PromptSubmit,
            json!({ GEN_AI_OPERATION_NAME: "chat" }),
        )
        .await;
        Ok(PromptPatch::noop())
    }

    async fn pre_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        _: &CancellationToken,
    ) -> Result<crate::hooks::Decision, HookError> {
        self.emit(
            ctx,
            Phase::PreToolUse,
            json!({
                GEN_AI_OPERATION_NAME: "execute_tool",
                GEN_AI_TOOL_NAME: call.tool_name,
                GEN_AI_TOOL_CALL_ID: call.call_id,
                MUR_MCP_SERVER: call.mcp_server,
            }),
        )
        .await;
        Ok(crate::hooks::Decision::Allow)
    }

    async fn post_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        result: &ToolResult,
        _: &CancellationToken,
    ) -> Result<PostToolUsePatch, HookError> {
        self.emit(
            ctx,
            Phase::PostToolUse,
            json!({
                GEN_AI_OPERATION_NAME: "execute_tool",
                GEN_AI_TOOL_NAME: call.tool_name,
                GEN_AI_TOOL_CALL_ID: call.call_id,
                MUR_MCP_SERVER: call.mcp_server,
                "duration_ms": result.duration_ms,
                "ok": result.ok,
            }),
        )
        .await;
        Ok(PostToolUsePatch::default())
    }

    async fn on_step_finish(
        &self,
        ctx: &HookCtx,
        step: &Step,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.emit(
            ctx,
            Phase::StepFinish,
            json!({
                GEN_AI_OPERATION_NAME: "chat",
                GEN_AI_RESPONSE_MODEL: step.model,
                GEN_AI_USAGE_INPUT_TOKENS: step.usage_input_tokens,
                GEN_AI_USAGE_OUTPUT_TOKENS: step.usage_output_tokens,
                GEN_AI_RESPONSE_FINISH_REASONS: [step.finish_reason.clone()],
            }),
        )
        .await;
        Ok(())
    }

    async fn on_message_send(
        &self,
        ctx: &HookCtx,
        view: &OutboundView,
        _: &CancellationToken,
    ) -> Result<MessagePatch, HookError> {
        self.emit(
            ctx,
            Phase::MessageSend,
            json!({
                GEN_AI_OPERATION_NAME: "invoke_agent",
                "recipient": view.recipient,
            }),
        )
        .await;
        Ok(MessagePatch::noop())
    }

    async fn on_error(
        &self,
        ctx: &HookCtx,
        err: &HookError,
        phase: Phase,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.emit(
            ctx,
            Phase::Error,
            json!({
                ERROR_TYPE: format!("{phase:?}"),
                "message": err.to_string(),
            }),
        )
        .await;
        Ok(())
    }

    async fn on_shutdown(
        &self,
        ctx: &HookCtx,
        reason: ShutdownReason,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.emit(
            ctx,
            Phase::Shutdown,
            json!({ "reason": format!("{reason:?}") }),
        )
        .await;
        Ok(())
    }
}
