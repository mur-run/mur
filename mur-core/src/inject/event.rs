use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    #[default]
    Prompt,
    Tool,
    Stop,
    SessionStart,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub kind: EventKind,
    pub tool_provider: String,
    pub query: Option<String>,
    pub tool_called: Option<String>,
    pub tool_input: Option<Value>,
    pub stop_reason: Option<String>,
    pub session_id: Option<String>,
    /// Claude Code Stop payloads carry the transcript path; used to recover the
    /// last assistant message for ambient capture (spec §3.1).
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// PostToolUse result payload (claude); exit_code is extracted from it.
    #[serde(default)]
    pub tool_response: Option<Value>,
    /// Hook-reported working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Wall-clock duration of the hook invocation, in milliseconds.
    /// None for events written at entry (before processing completes).
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// True for synthetic timing records written after hook processing completes.
    /// Excluded from event counts in compute() but included in latency percentiles.
    #[serde(default)]
    pub is_duration_record: bool,
}

#[allow(dead_code)]
pub fn parse_event(raw: Value, kind: EventKind, provider: &str) -> NormalizedEvent {
    match provider {
        "gemini" => parse_gemini(raw, kind),
        "cursor" => parse_cursor(raw, kind),
        "copilot" => parse_copilot(raw, kind),
        "opencode" => parse_opencode(raw, kind),
        _ => parse_claude(raw, kind),
    }
}

fn parse_claude(raw: Value, kind: EventKind) -> NormalizedEvent {
    NormalizedEvent {
        kind,
        tool_provider: "claude".into(),
        query: raw
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_called: raw
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_input: raw.get("tool_input").cloned(),
        stop_reason: raw
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        session_id: raw
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        transcript_path: raw
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_response: raw.get("tool_response").cloned(),
        cwd: raw.get("cwd").and_then(|v| v.as_str()).map(str::to_owned),
        duration_ms: None,
        is_duration_record: false,
    }
}

fn parse_gemini(raw: Value, kind: EventKind) -> NormalizedEvent {
    NormalizedEvent {
        kind,
        tool_provider: "gemini".into(),
        query: raw
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_called: raw.get("tool").and_then(|v| v.as_str()).map(str::to_owned),
        // Gemini's `result` is the post-tool output, not input args.
        // We store it in tool_input for queue serialisation; daemon should check tool_provider
        // when interpreting this field.
        tool_input: raw.get("result").cloned(),
        stop_reason: raw
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        session_id: raw
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        transcript_path: None,
        tool_response: None,
        cwd: None,
        duration_ms: None,
        is_duration_record: false,
    }
}

fn parse_cursor(raw: Value, kind: EventKind) -> NormalizedEvent {
    NormalizedEvent {
        kind,
        tool_provider: "cursor".into(),
        query: raw
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        // Prefix shell commands with "shell:" to distinguish from named tool functions
        tool_called: raw
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| format!("shell:{s}")),
        tool_input: None, // Cursor hook schema carries no structured tool args
        stop_reason: raw
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        session_id: None, // Cursor hooks do not expose a session identifier
        transcript_path: None,
        tool_response: None,
        cwd: None,
        duration_ms: None,
        is_duration_record: false,
    }
}

fn parse_copilot(raw: Value, kind: EventKind) -> NormalizedEvent {
    let tool_input = raw
        .get("input")
        .cloned()
        .or_else(|| raw.get("tool_input").cloned());
    NormalizedEvent {
        kind,
        tool_provider: "copilot".into(),
        query: raw
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_called: raw.get("tool").and_then(|v| v.as_str()).map(str::to_owned),
        tool_input,
        stop_reason: None, // Copilot stop events arrive as separate sessionEnd hooks
        session_id: None,  // Copilot does not surface session ID in hook payloads
        transcript_path: None,
        tool_response: None,
        cwd: None,
        duration_ms: None,
        is_duration_record: false,
    }
}

fn parse_opencode(raw: Value, kind: EventKind) -> NormalizedEvent {
    let session_obj = raw.get("session");
    let tool_obj = raw.get("tool");
    NormalizedEvent {
        kind,
        tool_provider: "opencode".into(),
        query: None,
        tool_called: tool_obj
            .and_then(|t| t.get("name"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_input: tool_obj.and_then(|t| t.get("input")).cloned(),
        stop_reason: session_obj
            .and_then(|s| s.get("status"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        session_id: session_obj
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        transcript_path: None,
        tool_response: None,
        cwd: None,
        duration_ms: None,
        is_duration_record: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_claude_captures_enrichment_fields() {
        let raw = json!({
            "session_id": "s1",
            "transcript_path": "/tmp/t.jsonl",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo build"},
            "tool_response": {"stdout": "ok", "exit_code": 0}
        });
        let ev = parse_event(raw, EventKind::Tool, "claude");
        assert_eq!(ev.transcript_path.as_deref(), Some("/tmp/t.jsonl"));
        assert_eq!(ev.cwd.as_deref(), Some("/repo"));
        assert_eq!(ev.tool_response.as_ref().unwrap()["exit_code"], 0);
    }

    #[test]
    fn old_queue_lines_still_deserialize() {
        // Queue lines written before this change lack the new fields.
        let line = r#"{"kind":"tool","tool_provider":"claude","query":null,"tool_called":"Bash",
            "tool_input":null,"stop_reason":null,"session_id":"s1"}"#;
        let ev: NormalizedEvent = serde_json::from_str(line).unwrap();
        assert!(ev.transcript_path.is_none());
        assert!(ev.tool_response.is_none());
        assert!(ev.cwd.is_none());
    }

    #[test]
    fn parse_claude_prompt() {
        let raw =
            json!({"prompt": "how do I implement tokio retry logic?", "session_id": "abc123"});
        let ev = parse_event(raw, EventKind::Prompt, "claude");
        assert_eq!(ev.kind, EventKind::Prompt);
        assert_eq!(
            ev.query.as_deref(),
            Some("how do I implement tokio retry logic?")
        );
        assert_eq!(ev.session_id.as_deref(), Some("abc123"));
        assert!(ev.tool_called.is_none());
    }

    #[test]
    fn parse_claude_pre_tool_use() {
        let raw = json!({"tool_name": "Edit", "tool_input": {"command": "str_replace", "path": "src/main.rs"}});
        let ev = parse_event(raw, EventKind::Tool, "claude");
        assert_eq!(ev.kind, EventKind::Tool);
        assert_eq!(ev.tool_called.as_deref(), Some("Edit"));
        assert!(ev.tool_input.is_some());
    }

    #[test]
    fn parse_claude_stop() {
        let raw = json!({"stop_reason": "end_turn"});
        let ev = parse_event(raw, EventKind::Stop, "claude");
        assert_eq!(ev.kind, EventKind::Stop);
        assert_eq!(ev.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn parse_gemini_before_agent() {
        let raw = json!({"prompt": "explain async/await in Rust"});
        let ev = parse_event(raw, EventKind::Prompt, "gemini");
        assert_eq!(ev.tool_provider, "gemini");
        assert_eq!(ev.query.as_deref(), Some("explain async/await in Rust"));
    }

    #[test]
    fn parse_gemini_after_tool() {
        let raw = json!({"tool": "bash", "result": "cargo build succeeded"});
        let ev = parse_event(raw, EventKind::Tool, "gemini");
        assert_eq!(ev.tool_called.as_deref(), Some("bash"));
        assert!(
            ev.tool_input.is_some(),
            "gemini result should map to tool_input"
        );
    }

    #[test]
    fn parse_cursor_prompt() {
        let raw = json!({"prompt": "refactor this function", "language": "rust"});
        let ev = parse_event(raw, EventKind::Prompt, "cursor");
        assert_eq!(ev.query.as_deref(), Some("refactor this function"));
    }

    #[test]
    fn parse_copilot_prompt() {
        let raw = json!({"prompt": "add error handling", "timeoutSec": 30});
        let ev = parse_event(raw, EventKind::Prompt, "copilot");
        assert_eq!(ev.query.as_deref(), Some("add error handling"));
    }

    #[test]
    fn parse_copilot_pre_tool_use() {
        let raw = json!({"tool": "bash", "input": {"command": "npm test"}});
        let ev = parse_event(raw, EventKind::Tool, "copilot");
        assert_eq!(ev.tool_called.as_deref(), Some("bash"));
        assert!(ev.tool_input.is_some());
    }

    #[test]
    fn parse_copilot_tool_input_fallback() {
        // When "input" is absent, falls back to "tool_input" key
        let raw = json!({"tool": "edit", "tool_input": {"path": "src/lib.rs"}});
        let ev = parse_event(raw, EventKind::Tool, "copilot");
        assert_eq!(ev.tool_called.as_deref(), Some("edit"));
        assert!(ev.tool_input.is_some());
    }

    #[test]
    fn parse_opencode_session_created() {
        let raw = json!({"session": {"id": "sess_123", "project": "/Users/user/app"}});
        let ev = parse_event(raw, EventKind::SessionStart, "opencode");
        assert_eq!(ev.session_id.as_deref(), Some("sess_123"));
    }

    #[test]
    fn parse_opencode_tool_execute() {
        let raw = json!({"tool": {"name": "bash", "input": {"command": "cargo run"}}});
        let ev = parse_event(raw, EventKind::Tool, "opencode");
        assert_eq!(ev.tool_called.as_deref(), Some("bash"));
    }

    #[test]
    fn parse_empty_stdin_does_not_panic() {
        let raw = serde_json::json!({});
        let ev = parse_event(raw, EventKind::Prompt, "claude");
        assert_eq!(ev.kind, EventKind::Prompt);
        assert!(ev.query.is_none());
    }

    #[test]
    fn amp_uses_claude_parser() {
        let raw = json!({"prompt": "fix the build", "session_id": "s1"});
        let ev = parse_event(raw, EventKind::Prompt, "amp");
        assert_eq!(ev.tool_provider, "claude");
        assert_eq!(ev.query.as_deref(), Some("fix the build"));
    }

    #[test]
    fn auggie_uses_claude_parser() {
        let raw = json!({"prompt": "help me debug this", "session_id": "s2"});
        let ev = parse_event(raw, EventKind::Prompt, "auggie");
        assert_eq!(ev.tool_provider, "claude");
    }
}
