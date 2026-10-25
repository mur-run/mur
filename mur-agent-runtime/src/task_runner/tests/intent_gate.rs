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

/// Review on #2115: a checker call is a paid LLM call of the owning turn. It
/// lands in that turn's per-model ledger under the checker's own model —
/// even when its reply is junk the gate ignores — and it is NOT the main
/// model's context size.
#[tokio::test]
async fn checker_calls_are_charged_to_the_turn_under_their_own_model() {
    for reply in ["network-egress", "¯\\_(ツ)_/¯"] {
        let r = Arc::new(runner(reply, Arc::new(AtomicU64::new(0))));
        r.mark_unattended("t").await;
        let ledger = Arc::new(super::super::turn_models::TurnModels::default());
        let call = bash("python3 sync.py");
        super::super::turn_models::scope(ledger.clone(), async {
            r.gate_response("t", std::slice::from_ref(&call)).await;
        })
        .await;
        let by_model = ledger.to_json();
        assert_eq!(by_model.as_array().unwrap().len(), 1, "{reply}: {by_model}");
        assert_eq!(by_model[0]["model"], "fixed", "{reply}");
        assert_eq!(by_model[0]["input_tokens"], 40, "{reply}");
        assert_eq!(by_model[0]["output_tokens"], 2, "{reply}");
        assert_eq!(
            r.last_input_tokens.load(Ordering::Relaxed),
            0,
            "{reply}: a side call is not the conversation's context size"
        );
    }
}

/// …and it reaches telemetry as its own `LlmCall`, tagged with the task and
/// the checker's provider, not the main model's.
#[tokio::test]
async fn checker_calls_emit_their_own_llm_call_event() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let r = TaskRunner::new_stub_echo()
        .with_sandbox_enforcing(true)
        .with_tools(vec![Arc::new(CountingBashTool::default())])
        .with_intent_checker(Arc::new(
            IntentChecker::new(
                Arc::new(Fixed(Ok("write"), Duration::ZERO)),
                Duration::from_secs(1),
            )
            .with_telemetry(Some(tx), "ollama"),
        ));
    r.gate_response("t-9", std::slice::from_ref(&bash("python3 sync.py")))
        .await;
    let ev = rx.try_recv().expect("one LlmCall for the checker");
    match ev {
        crate::telemetry_writer::Event::LlmCall {
            task_id,
            model,
            input_tokens,
            output_tokens,
            provider,
            ..
        } => {
            assert_eq!(task_id, "t-9");
            assert_eq!(model, "fixed");
            assert_eq!((input_tokens, output_tokens), (40, 2));
            assert_eq!(provider, "ollama");
        }
        other => panic!("expected LlmCall, got {other:?}"),
    }
}
