//! Who may answer a HITL prompt: a caller that cannot approve is denied at
//! once, everyone else is asked. Moved out of `task_runner/mod.rs` verbatim.

use super::TaskRunner;
use std::sync::Arc;

fn runner() -> Arc<TaskRunner> {
    Arc::new(TaskRunner::new_stub_echo())
}

const NO_RULE: super::deny_hint::AskCause = super::deny_hint::AskCause::NoAllowRule;

/// The fix: a caller that cannot approve gets the denial immediately,
/// instead of after `hitl.timeout_secs` of silence.
#[test]
fn a_caller_that_cannot_approve_is_denied_at_once() {
    let d = super::decide_without_asking(Some(false), "a", "remember", &NO_RULE)
        .expect("must short-circuit");
    assert!(!d.allow);
    let why = d.reason.unwrap_or_default();
    assert!(why.contains("remember"), "must name the tool: {why}");
    assert!(
        why.contains("own terminal"),
        "must name where it can be approved: {why}"
    );
    assert!(
        why.contains("tool-allow"),
        "must name the other way out: {why}"
    );
}

/// Controls. An interactive caller must still be asked, and no routed entry
/// must still reach the existing no-sink handling — this shortcut is only
/// for a caller that told us it cannot answer.
#[test]
fn everyone_else_is_still_asked() {
    assert!(super::decide_without_asking(Some(true), "a", "remember", &NO_RULE).is_none());
    assert!(super::decide_without_asking(None, "a", "remember", &NO_RULE).is_none());
}

/// A caller that declared it cannot approve must be distinguishable from
/// one that can, at the map the gate reads. Before this the two were the
/// same entry and the gate waited on both.
#[tokio::test]
async fn the_gate_can_tell_the_two_callers_apart() {
    let r = runner();
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    r.register_client_notifier("t-interactive", tx.clone(), true)
        .await;
    r.register_client_notifier("t-oneshot", tx, false).await;

    let map = r.client_notifiers.lock().await;
    assert_eq!(map.get("t-interactive").map(|(_, ok)| *ok), Some(true));
    assert_eq!(map.get("t-oneshot").map(|(_, ok)| *ok), Some(false));
}

/// Step events still reach a caller that cannot approve — it is not a
/// second-class client, it just has nobody to answer a prompt. If this
/// breaks, `mur agent send` goes silent about tool progress too.
#[tokio::test]
async fn a_non_approving_caller_still_receives_notifications() {
    let r = runner();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    r.register_client_notifier("t1", tx, false).await;

    let sink = {
        let map = r.client_notifiers.lock().await;
        map.get("t1").map(|(tx, _)| tx.clone())
    };
    sink.expect("sink must still be routed")
        .send(serde_json::json!({"method": "step/started"}))
        .await
        .expect("send must reach the client");
    assert!(rx.recv().await.is_some());
}

/// `step/tokens` counts the result as it enters history (post-hook), goes
/// to the watching client, and skips a withdrawn call that never got a
/// step card.
#[tokio::test]
async fn step_tokens_counts_the_post_hook_result_per_started_step() {
    let r = runner();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    r.register_client_notifier("t1", tx, true).await;
    let entry = |content: &str| crate::llm::ToolResultEntry {
        call_id: "c".into(),
        content: content.into(),
        is_error: false,
        status: Default::default(),
        images: Vec::new(),
    };
    let note = "[Large output compressed; original stored.]";
    r.emit_step_tokens(
        "t1",
        &[Some("s1".into()), None],
        &[entry(note), entry("withdrawn")],
    )
    .await;

    let n = rx.recv().await.expect("one frame for the started step");
    assert_eq!(n["method"], "step/tokens");
    assert_eq!(n["params"]["step_id"], "s1");
    assert_eq!(n["params"]["task_id"], "t1");
    let tokens = n["params"]["tokens"].as_u64().expect("a count");
    assert!(tokens > 0 && tokens < note.len() as u64, "{tokens}");
    assert!(rx.try_recv().is_err(), "no frame for the withdrawn call");
}

/// Unregistering clears both halves.
#[tokio::test]
async fn unregister_removes_the_entry() {
    let r = runner();
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    r.register_client_notifier("t1", tx, false).await;
    r.unregister_client_notifier("t1").await;
    assert!(r.client_notifiers.lock().await.get("t1").is_none());
}
