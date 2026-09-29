use super::*;
use mur_common::a2a::MessagePart;

// Helper: build LlmResponse that signals tool_use stop with one call
fn tool_call_response(call_id: &str, command: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: call_id.into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": command}),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    }
}

fn end_turn_response(text: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: text.into(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: crate::llm::StopReason::EndTurn,
    }
}

/// A response truncated mid-tool_use: `stop_reason == MaxTokens` while a
/// tool_call is present. The `input` is the empty `{}` that
/// `parse_response_body` yields when the assistant turn was cut off before
/// the tool_use JSON finished — i.e. the malformed call the loop must NOT
/// execute blindly.
fn truncated_tool_call_response(call_id: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: call_id.into(),
            tool_name: "bash".into(),
            // Empty input — the hallmark of a truncated tool_use.
            input: serde_json::json!({}),
        }],
        stop_reason: crate::llm::StopReason::MaxTokens,
    }
}

/// A response truncated while still inside a thinking block: `stop_reason
/// == MaxTokens` but NEITHER text NOR a tool_call was ever produced. This
/// is what the Anthropic client now returns (instead of erroring) when
/// the whole `max_tokens` budget goes to reasoning before any visible
/// output starts.
fn truncated_thinking_only_response() -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: crate::llm::StopReason::MaxTokens,
    }
}

/// A response truncated in the middle of the FINAL answer: `stop_reason ==
/// MaxTokens` with usable text and no tool_calls — the silent-corruption
/// case from #715 (a delegated spec cut mid-word at exactly the cap).
fn truncated_text_response(text: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: text.into(),
        input_tokens: 5,
        output_tokens: 16384,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: crate::llm::StopReason::MaxTokens,
    }
}

/// Counting `bash` tool: records how many times it executes so a test can
/// assert a (truncated) tool call was NOT run.
#[derive(Default)]
struct CountingBashTool {
    calls: Arc<AtomicU64>,
    /// D8 regression cover: the task id `execute` observed via the
    /// task-local, if the scope reached it. `None` on a stub that never
    /// sets this field — struct-update syntax at every call site keeps
    /// this optional.
    seen_task: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for CountingBashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: "bash".into(),
            description: "test bash tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        *self.seen_task.lock().unwrap_or_else(|e| e.into_inner()) =
            crate::tools::bash_jobs::current_task_id();
        Ok("ran".to_string().into())
    }
}

fn ping_spec() -> TaskSpec {
    TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: "ping".into(),
            }],
        },
        context_task_id: None,
        task_id: None,
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        active_fleet: None,
        active_team: None,
        attended: true,
        deadline_secs: None,
    }
}

fn user_turn(text: &str, task_id: &str, ctx: Option<&str>) -> TaskSpec {
    TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: text.into() }],
        },
        context_task_id: ctx.map(str::to_string),
        task_id: Some(task_id.to_string()),
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        active_fleet: None,
        active_team: None,
        attended: true,
        deadline_secs: None,
    }
}

fn build_tool_call_response(call_id: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: call_id.into(),
            tool_name: "build".into(),
            // IDENTICAL args every turn: only the result varies.
            input: serde_json::json!({}),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    }
}

fn loop_spec(text: &str) -> TaskSpec {
    TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![mur_common::a2a::MessagePart::Text { text: text.into() }],
        },
        context_task_id: None,
        task_id: None,
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        active_fleet: None,
        active_team: None,
        attended: true,
        deadline_secs: None,
    }
}

fn empty_pending_approvals() -> HitlApprovals {
    Arc::new(tokio::sync::Mutex::new(HashMap::new()))
}

mod approval;
mod basics;
mod continuation;
mod conversation;
mod doom_loop;
mod drain_and_retry;
mod limits;
mod system_prompt;
mod truncation;

/// Lives in `task_runner/tests/` so this file stops growing.
mod sandbox_gate;

/// D2b probe: forged remembered allow, real store + key.
mod d2b_attack_path;
