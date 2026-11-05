//! `hitl_step_tests`, moved out of `stream.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented by one level, nothing else.

use super::HitlRequest;

#[test]
fn parses_step_id_when_present() {
    let v = serde_json::json!({
        "step_id": "s-1", "hitl_id": "h-1", "tool_name": "edit",
        "tool_input": { "file_path": "a.rs" }, "prompt": "Run `edit`?"
    });
    let req = HitlRequest::from_value(v, std::time::Instant::now());
    assert_eq!(req.step_id.as_deref(), Some("s-1"));
    assert_eq!(req.hitl_id, "h-1");
}

#[test]
fn step_id_none_on_old_runtime() {
    let v = serde_json::json!({ "hitl_id": "h-1", "tool_name": "bash" });
    let req = HitlRequest::from_value(v, std::time::Instant::now());
    assert!(req.step_id.is_none());
}

#[test]
fn from_params_expands_calls_and_falls_back_to_single() {
    let batch = serde_json::json!({
        "hitl_id": "h1", "tool_name": "bash", "tool_input": {"command": "a"},
        "calls": [
            {"hitl_id": "h1", "step_id": "s1", "tool_name": "bash", "tool_input": {"command": "a"}},
            {"hitl_id": "h2", "step_id": "s2", "tool_name": "write_file", "tool_input": {"path": "x"}}
        ]
    });
    let reqs = HitlRequest::from_params(batch);
    assert_eq!(
        reqs.iter().map(|r| r.hitl_id.as_str()).collect::<Vec<_>>(),
        ["h1", "h2"]
    );
    assert_eq!(reqs[1].step_id.as_deref(), Some("s2"));
    let single =
        HitlRequest::from_params(serde_json::json!({"hitl_id": "h9", "tool_name": "bash"}));
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].hitl_id, "h9");
}
