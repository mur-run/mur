//! Hook-trait shared value types. All `Send + Sync + 'static` so they can
//! cross `Arc<dyn Hook>` boundaries.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::SystemTime;

use crate::companion::clock::Clock;

pub type RunId = String; // ULID or task UUID

/// Context passed to every hook invocation.
#[derive(Clone)]
pub struct HookCtx {
    pub agent_name: String,
    pub agent_uuid: String,
    pub run_id: RunId,
    pub clock: Arc<dyn Clock>,
    pub telemetry: Arc<dyn TelemetryEmitter>,
}

/// Sink for OTel-GenAI span events emitted by `TelemetryHook`.
/// M0.1 keeps the surface minimal; M0.3.1 implements a real adapter
/// over the existing `telemetry_writer::Event` channel.
#[async_trait::async_trait]
pub trait TelemetryEmitter: Send + Sync {
    async fn emit_span_event(&self, name: &str, attrs: serde_json::Value);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Phase {
    Startup,
    TriggerFired,
    MessageReceived,
    PromptSubmit,
    PreToolUse,
    PostToolUse,
    StepFinish,
    MessageSend,
    Error,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShutdownReason {
    Sigterm,
    Grace,
    RekeyRestart,
    Crash(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TriggerKind {
    Webhook,
    Cron,
    Message,
    Manual,
    Companion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerPayload {
    pub source: String,
    pub data: serde_json::Value,
    pub received_at: SystemTime,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum AskDefault {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub mcp_server: Option<String>,
    pub call_id: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub ok: bool,
    pub output: serde_json::Value,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub model: String,
    pub usage_input_tokens: u64,
    pub usage_output_tokens: u64,
    pub finish_reason: String,
    pub was_compaction: bool,
}

/// Read-only view of the prompt builder state. Mutations happen via PromptPatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptView {
    pub system: Option<String>,
    pub messages: Vec<serde_json::Value>,
}

/// Read-only view of an outbound message. Mutations happen via MessagePatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundView {
    pub recipient: Option<String>,
    pub body: String,
    pub locale: Option<String>,
}

/// Read-only view of an inbound A2A envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AEnvelopeView {
    pub method: String,
    pub from_pubkey: Option<String>,
    pub task_id: Option<String>,
    pub raw: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("hook handler {handler} failed in phase {phase:?}: {source}")]
    Handler {
        handler: String,
        phase: Phase,
        #[source]
        source: anyhow::Error,
    },
    #[error("cancellation requested")]
    Cancelled,
}

#[derive(Debug, Clone, Copy)]
pub enum ErrorAction {
    Retry(u8),
    Fail,
    Swallow,
}
