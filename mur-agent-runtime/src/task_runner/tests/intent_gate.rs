//! Layer 3 end to end on gate B: an intent raise turns a riskless
//! `bash: allow` call into an ask, and the human's deny is honoured.

use super::*;
use crate::hitl::intent::{IntentChecker, tests::Fixed};
use std::time::Duration;

fn runner(reply: &'static str, calls: Arc<AtomicU64>) -> TaskRunner {
    TaskRunner::new_stub_echo()
        .with_sandbox_enforcing(true)
        .with_tools(vec![Arc::new(CountingBashTool {
            calls,
            ..Default::default()
        })])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: "bash".into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }])
        .with_intent_checker(Arc::new(IntentChecker::new(
            Arc::new(Fixed(Ok(reply), Duration::ZERO)),
            Duration::from_secs(1),
        )))
}

fn bash(cmd: &str) -> crate::llm::ToolCallResult {
    crate::llm::ToolCallResult {
        call_id: "c-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({ "command": cmd }),
    }
}

#[tokio::test]
async fn a_raised_allow_call_asks_and_a_deny_stops_it() {
    let calls = Arc::new(AtomicU64::new(0));
    let r = Arc::new(runner("network-egress", calls.clone()));
    let call = bash("python3 sync.py");
    r.mark_unattended("t").await;
    let (out, _) = r.gate_response("t", std::slice::from_ref(&call)).await;
    let d = out
        .get("c-1")
        .cloned()
        .expect("raised call must reach the gate");
    assert!(!d.allow);
    let res = r.handle_tool_call("t", &call, Some(d), None).await;
    assert!(res.is_err(), "a denied raised call must not run");
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

/// Negative control: the model answers `read` (the injection outcome) or
/// `write` — nothing is raised, so the `Allow` call never meets the gate.
#[tokio::test]
async fn a_read_or_write_answer_leaves_an_allow_call_alone() {
    for reply in ["read", "write"] {
        let calls = Arc::new(AtomicU64::new(0));
        let r = Arc::new(runner(reply, calls.clone()));
        let call = bash("python3 sync.py");
        r.mark_unattended("t").await;
        let (out, _) = r.gate_response("t", std::slice::from_ref(&call)).await;
        assert!(out.is_empty(), "{reply}: nothing to decide");
        r.handle_tool_call("t", &call, None, None).await.unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1, "{reply}");
    }
}
