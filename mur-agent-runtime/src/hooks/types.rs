//! Hook-trait shared value types. All `Send + Sync + 'static` so they can
//! cross `Arc<dyn Hook>` boundaries.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::companion::clock::Clock;

pub type RunId = String; // ULID or task UUID

/// Context passed to every hook invocation.
///
/// `agent_home` and `turn_id` were added in M3.8.1 so that
/// `B0SafetyHook` can locate `<agent_home>/telemetry/inputs.jsonl` and
/// the per-artifact `.txt` sidecars and select the entries for the
/// current turn. `turn_flags` carries flags emitted by mutate hooks
/// (e.g. `after_untrusted_input` from `on_prompt_submit`) down to
/// `pre_tool_use` on the same turn.
#[derive(Clone)]
pub struct HookCtx {
    pub agent_name: String,
    pub agent_uuid: String,
    pub run_id: RunId,
    pub clock: Arc<dyn Clock>,
    pub telemetry: Arc<dyn TelemetryEmitter>,
    /// Per-agent home dir, e.g. `~/.mur/agents/<name>`. Empty path until
    /// the supervisor populates it.
    pub agent_home: PathBuf,
    /// Monotonic turn counter inside this run. Resets to 0 each restart.
    pub turn_id: u64,
    /// Turn-scoped flags raised by mutate hooks (e.g.
    /// `"after_untrusted_input"`) and consumed by gate hooks.
    pub turn_flags: Vec<String>,
}

impl HookCtx {
    pub fn agent_home(&self) -> &std::path::Path {
        &self.agent_home
    }

    pub fn turn_id(&self) -> u64 {
        self.turn_id
    }

    pub fn turn_flags(&self) -> &[String] {
        &self.turn_flags
    }

    /// Test helper: build a HookCtx pinned to an `agent_home` + `turn_id`.
    /// Used by integration tests that need a real on-disk ledger to read.
    pub fn for_test_with_home(home: PathBuf, turn_id: u64) -> Self {
        Self {
            agent_name: "test".into(),
            agent_uuid: "00000000-0000-0000-0000-000000000000".into(),
            run_id: "test-run".into(),
            clock: Arc::new(crate::companion::clock::SystemClock),
            telemetry: Arc::new(NoopTelemetryEmitter),
            agent_home: home,
            turn_id,
            turn_flags: Vec::new(),
        }
    }

    /// Test helper: build a HookCtx with a populated `turn_flags` vec
    /// (no on-disk state). Used by gate-hook tests.
    pub fn for_test_with_turn_flags(turn_flags: Vec<String>) -> Self {
        Self {
            agent_name: "test".into(),
            agent_uuid: "00000000-0000-0000-0000-000000000000".into(),
            run_id: "test-run".into(),
            clock: Arc::new(crate::companion::clock::SystemClock),
            telemetry: Arc::new(NoopTelemetryEmitter),
            agent_home: PathBuf::new(),
            turn_id: 0,
            turn_flags,
        }
    }
}

/// `TelemetryEmitter` impl that drops every event. Used by the
/// `for_test_*` HookCtx constructors so callers don't need to wire up
/// a real emitter just to invoke a hook.
struct NoopTelemetryEmitter;

#[async_trait::async_trait]
impl TelemetryEmitter for NoopTelemetryEmitter {
    async fn emit_span_event(&self, _name: &str, _attrs: serde_json::Value) {}
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

impl ToolCall {
    /// Resolved tool name. Mirrors `tool_name` for now; reserved so future
    /// MCP-prefixed renames can flow through one accessor instead of every
    /// gate-hook checking both fields.
    pub fn name(&self) -> &str {
        &self.tool_name
    }

    /// Test helper: build a `ToolCall` with a stable `call_id` and no
    /// `mcp_server`. Only intended for unit/integration tests.
    pub fn test(name: &str, input: serde_json::Value) -> Self {
        Self {
            tool_name: name.into(),
            mcp_server: None,
            call_id: "test-call".into(),
            input,
        }
    }
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

impl PromptView {
    /// Empty view with no system prompt and no messages. Useful for tests
    /// that exercise `on_prompt_submit` without building a real prompt.
    pub fn empty() -> Self {
        Self {
            system: None,
            messages: Vec::new(),
        }
    }
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
    /// Runtime error inside a hook (unwrapped: no handler/phase tag).
    /// Used by hooks that fail before they have phase context (e.g.
    /// I/O failure reading the ledger inside `B0SafetyHook`).
    #[error("hook runtime error: {0}")]
    Runtime(String),
}

#[derive(Debug, Clone, Copy)]
pub enum ErrorAction {
    Retry(u8),
    Fail,
    Swallow,
}
