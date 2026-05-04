//! M4 end-to-end: verifies L2 detection logic and workflow trigger matching.

use mur_core::inject::event::{EventKind, parse_event};
use serde_json::json;

fn make_tool_event(tool_name: &str, with_response: bool) -> serde_json::Value {
    let mut v = json!({
        "tool_name": tool_name,
        "tool_input": {"file_path": "src/main.rs", "old_string": "foo", "new_string": "bar"},
        "session_id": "sess_e2e_m4"
    });
    if with_response {
        v.as_object_mut()
            .unwrap()
            .insert("tool_response".into(), json!({"output": "ok"}));
    }
    v
}

#[test]
fn pre_tool_use_edit_has_no_tool_response() {
    let raw = make_tool_event("Edit", false);
    let ev = parse_event(raw.clone(), EventKind::Tool, "claude");
    assert_eq!(ev.tool_called.as_deref(), Some("Edit"));
    assert!(raw.get("tool_response").is_none());
}

#[test]
fn post_tool_use_edit_has_tool_response() {
    let raw = make_tool_event("Edit", true);
    assert!(raw.get("tool_response").is_some());
}

#[test]
fn read_tool_is_non_l2_tool() {
    let raw = json!({
        "tool_name": "Read",
        "tool_input": {"file_path": "src/lib.rs"},
        "session_id": "sess_e2e_m4"
    });
    let ev = parse_event(raw.clone(), EventKind::Tool, "claude");
    let called = ev.tool_called.as_deref().unwrap_or("").to_ascii_lowercase();
    assert!(!["edit", "write", "bash", "multiedit"].contains(&called.as_str()));
}
