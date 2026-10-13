use super::*;
use serde_json::json;

/// `write_file` is keyed on its name by `session_grant`; `rm -rf` has no key.
fn write_call(id: &str) -> Value {
    json!({ "hitl_id": id, "tool_name": "write_file", "tool_input": { "path": "/x/y" } })
}

fn batch(calls: Vec<Value>) -> Value {
    json!({ "batch_id": "b", "calls": calls })
}

#[test]
fn an_ungranted_batch_asks_and_offers_the_key() {
    let s = SessionAllow::default();
    let Plan::Ask(calls) = plan(&s, "a", Some("c1"), &batch(vec![write_call("h1")])) else {
        panic!("nothing granted yet: must ask");
    };
    assert_eq!(calls[0].grant.as_ref().unwrap().0, "write_file");
}

#[test]
fn a_granted_key_auto_allows_in_its_own_channel_only() {
    let s = SessionAllow::default();
    s.grant("a", "c1", "write_file");
    let b = batch(vec![write_call("h1")]);
    assert!(matches!(plan(&s, "a", Some("c1"), &b), Plan::AutoAllow(_)));
    assert!(matches!(plan(&s, "a", Some("c2"), &b), Plan::Ask(_)));
    assert!(matches!(plan(&s, "other", Some("c1"), &b), Plan::Ask(_)));
}

/// The "latest channel" fallback has no defined scope: never auto, never offered.
#[test]
fn no_channel_never_auto_allows_or_offers() {
    let s = SessionAllow::default();
    s.grant("a", "c1", "write_file");
    let Plan::Ask(calls) = plan(&s, "a", None, &batch(vec![write_call("h1")])) else {
        panic!("no channel: must ask");
    };
    assert!(calls[0].grant.is_none());
}

/// All-or-nothing: one uncovered call sends the whole batch to the operator.
#[test]
fn a_partly_covered_batch_asks_for_every_call() {
    let s = SessionAllow::default();
    s.grant("a", "c1", "write_file");
    let rm = json!({ "hitl_id": "h2", "tool_name": "bash", "tool_input": { "command": "rm -rf build" } });
    let Plan::Ask(calls) = plan(&s, "a", Some("c1"), &batch(vec![write_call("h1"), rm])) else {
        panic!("rm -rf is not covered: must ask");
    };
    assert_eq!(calls.len(), 2);
    assert!(calls[1].grant.is_none());
}

/// The declared `risk` is forwarded to `session_grant`, so a profile marking the
/// tool destructive removes a grant made earlier by name.
#[test]
fn a_declared_risk_overrides_a_remembered_key() {
    let s = SessionAllow::default();
    s.grant("a", "c1", "write_file");
    let mut call = write_call("h1");
    call["risk"] = json!("destructive");
    assert!(matches!(plan(&s, "a", Some("c1"), &batch(vec![call])), Plan::Ask(_)));
}

#[test]
fn reset_forgets_every_channel_of_that_agent_only() {
    let s = SessionAllow::default();
    s.grant("a", "c1", "write_file");
    s.grant("a", "c2", "write_file");
    s.grant("b", "c1", "write_file");
    s.reset("a");
    assert!(!s.contains("a", "c1", "write_file"));
    assert!(!s.contains("a", "c2", "write_file"));
    assert!(s.contains("b", "c1", "write_file"));
}

/// A legacy payload (no `calls`) is one call and gets the same treatment.
#[test]
fn a_legacy_single_call_payload_is_planned() {
    let s = SessionAllow::default();
    s.grant("a", "c1", "write_file");
    let legacy = json!({ "hitl_id": "h1", "tool_name": "write_file", "tool_input": { "path": "/x/y" } });
    assert!(matches!(plan(&s, "a", Some("c1"), &legacy), Plan::AutoAllow(_)));
}

#[test]
fn the_event_carries_the_grant_and_the_auto_flag() {
    let s = SessionAllow::default();
    let p = batch(vec![write_call("h1")]);
    let Plan::Ask(calls) = plan(&s, "a", Some("c1"), &p) else {
        panic!()
    };
    let ev = event_payload("a", Some("c1"), &p, &calls, false);
    assert_eq!(ev["auto_allowed"], json!(false));
    assert_eq!(ev["channel_id"], json!("c1"));
    assert_eq!(ev["calls"][0]["session_grant"]["key"], json!("write_file"));
}
