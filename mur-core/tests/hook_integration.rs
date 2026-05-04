//! Cross-tool hook integration: fixture stdin → expected NormalizedEvent.

use mur_core::inject::event::{EventKind, parse_event};
use serde_json::Value;

fn load_fixture(name: &str) -> Value {
    let path = format!("tests/fixtures/hook_inputs/{name}");
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"));
    serde_json::from_str(&content).unwrap_or_else(|e| panic!("bad JSON in {path}: {e}"))
}

#[test]
fn claude_coding_prompt_has_query() {
    let raw = load_fixture("claude_prompt_coding.json");
    let ev = parse_event(raw, EventKind::Prompt, "claude");
    assert_eq!(ev.kind, EventKind::Prompt);
    assert_eq!(ev.tool_provider, "claude");
    let q = ev.query.expect("coding prompt must have query");
    assert!(q.len() > 10, "query should be non-trivial");
}

#[test]
fn claude_ack_query_in_event() {
    let raw = load_fixture("claude_prompt_ack.json");
    let ev = parse_event(raw, EventKind::Prompt, "claude");
    assert_eq!(ev.query.as_deref(), Some("ok"));
}

#[test]
fn claude_stop_has_stop_reason() {
    let raw = load_fixture("claude_stop.json");
    let ev = parse_event(raw, EventKind::Stop, "claude");
    assert_eq!(ev.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(ev.kind, EventKind::Stop);
}

#[test]
fn gemini_prompt_extracted() {
    let raw = load_fixture("gemini_before_agent.json");
    let ev = parse_event(raw, EventKind::Prompt, "gemini");
    assert_eq!(ev.tool_provider, "gemini");
    assert!(ev.query.is_some(), "gemini BeforeAgent must have query");
}

#[test]
fn cursor_prompt_extracted() {
    let raw = load_fixture("cursor_prompt.json");
    let ev = parse_event(raw, EventKind::Prompt, "cursor");
    assert_eq!(ev.tool_provider, "cursor");
    assert_eq!(
        ev.query.as_deref(),
        Some("implement retry logic with exponential backoff")
    );
}

#[test]
fn copilot_prompt_extracted() {
    let raw = load_fixture("copilot_prompt.json");
    let ev = parse_event(raw, EventKind::Prompt, "copilot");
    assert_eq!(ev.tool_provider, "copilot");
    assert!(ev.query.is_some(), "copilot prompt must have query");
}

#[test]
fn opencode_session_created_has_session_id() {
    let raw = load_fixture("opencode_session_created.json");
    let ev = parse_event(raw, EventKind::SessionStart, "opencode");
    assert_eq!(ev.session_id.as_deref(), Some("sess_abc"));
    assert_eq!(ev.kind, EventKind::SessionStart);
}

#[test]
fn all_tools_parse_empty_json_without_panic() {
    let providers = [
        "claude", "gemini", "cursor", "copilot", "opencode", "amp", "auggie",
    ];
    for p in &providers {
        let ev = parse_event(serde_json::json!({}), EventKind::Prompt, p);
        assert!(
            ev.query.is_none(),
            "empty JSON should produce no query for provider {p}"
        );
    }
}
