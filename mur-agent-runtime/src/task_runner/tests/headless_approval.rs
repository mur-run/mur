//! #2085: a caller that cannot approve (`mur agent send`, `can_approve:
//! false`) must still honour a decision the human already made, and must be
//! told "needs a human" apart from "denied".

use super::*;
use crate::hitl::store::{DecisionStore, Settled};

struct Fixed(Option<Settled>);

#[async_trait::async_trait]
impl DecisionStore for Fixed {
    async fn lookup(&self, _h: &str) -> Option<Settled> {
        self.0
    }
    async fn record(&self, _r: mur_common::hitl::HitlResponse) {}
}

struct Run {
    outcome: TaskOutcome,
    executed: u64,
    asked: bool,
}

async fn run_headless(settled: Option<Settled>) -> Run {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![
        tool_call_response("c-1", "echo hi"),
        end_turn_response("OK"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
        .with_tools(vec![Arc::new(CountingBashTool {
            calls: calls.clone(),
            seen_task: Default::default(),
        })])
        .with_tools_policy(vec![]) // default Ask
        .with_pending_approvals(empty_pending_approvals())
        .with_sandbox_enforcing(true)
        .with_notifier(ntx.clone())
        .with_decision_store(Arc::new(Fixed(settled)))
        .with_hitl_timeout_secs(30)
        .with_iteration_ceiling(3);
    runner
        .register_client_notifier("t-headless", ntx, false)
        .await;
    let mut spec = loop_spec("headless");
    spec.task_id = Some("t-headless".into());
    let runner = Arc::new(runner);
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), runner.run_sync(spec))
        .await
        .expect("a headless caller must never wait on the HITL timeout");
    let mut asked = false;
    while let Ok(n) = nrx.try_recv() {
        asked |= n["method"] == "tool/approval_needed";
    }
    Run {
        outcome,
        executed: calls.load(Ordering::Relaxed),
        asked,
    }
}

fn error_of(o: &TaskOutcome) -> mur_common::a2a::TaskError {
    match o {
        TaskOutcome::Failed(t) => t.error.clone().expect("a failed task carries its error"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// The bug: an allow the human already gave (e.g. from `mur agent cli
/// --ask`) was ignored because the headless refusal ran before the store
/// was consulted.
#[tokio::test]
async fn a_remembered_allow_runs_for_a_caller_that_cannot_approve() {
    let r = run_headless(Some(Settled::Allow)).await;
    assert!(
        matches!(r.outcome, TaskOutcome::Completed(_)),
        "{:?}",
        r.outcome
    );
    assert_eq!(r.executed, 1);
    assert!(!r.asked, "a settled call must not prompt");
}

/// Acceptance 3: an explicit denial is unchanged — same code, not recoverable.
#[tokio::test]
async fn a_remembered_deny_stays_hitl_denied() {
    let r = run_headless(Some(Settled::Deny)).await;
    let e = error_of(&r.outcome);
    assert_eq!(e.code, "hitl_denied", "{}", e.message);
    assert!(!e.recoverable);
    assert_eq!(r.executed, 0);
    assert!(!r.asked);
}

/// Acceptance 2: nobody has decided yet. Still fail-closed (nothing runs,
/// nothing is asked), but the caller can tell it needs a human, and the
/// message says how to give the approval and come back.
#[tokio::test]
async fn an_undecided_call_reports_approval_required_not_denied() {
    let r = run_headless(None).await;
    let e = error_of(&r.outcome);
    assert_eq!(e.code, "hitl_approval_required", "{}", e.message);
    assert!(e.recoverable, "needs a human, not a dead end");
    assert!(e.message.contains("--ask"), "{}", e.message);
    assert!(e.message.contains("resend"), "{}", e.message);
    assert_eq!(r.executed, 0, "fail-closed: never runs unapproved");
    assert!(!r.asked, "nobody is there to answer");
}

/// #2085 option C: a headless turn that opened an approval window.
/// `answer` = what a human sends to the parked call (`None` = nobody does).
async fn run_parked(answer: Option<bool>, wait: std::time::Duration) -> (Run, bool) {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![
        tool_call_response("c-1", "echo hi"),
        end_turn_response("OK"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let pa = empty_pending_approvals();
    let (ntx, _nrx) = tokio::sync::mpsc::channel(16);
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
        .with_tools(vec![Arc::new(CountingBashTool {
            calls: calls.clone(),
            seen_task: Default::default(),
        })])
        .with_tools_policy(vec![])
        .with_pending_approvals(pa.clone())
        .with_sandbox_enforcing(true)
        .with_notifier(ntx.clone())
        .with_decision_store(Arc::new(Fixed(None)))
        .with_hitl_timeout_secs(30)
        .with_iteration_ceiling(3);
    runner.register_client_notifier("t-park", ntx, false).await;
    runner.open_approval_window("t-park", wait);
    let parking = runner.parking();
    let runner = Arc::new(runner);
    let run = tokio::spawn({
        let runner = runner.clone();
        async move {
            let mut spec = loop_spec("headless");
            spec.task_id = Some("t-park".into());
            runner.run_sync(spec).await
        }
    });
    // Wait until the call is listed, as `mur agent hitl pending` would see it.
    let mut listed = false;
    for _ in 0..200 {
        if let Some(c) = parking.list().into_iter().next() {
            listed = true;
            if let Some(allow) = answer
                && let Some(tx) = pa.lock().await.remove(&c.hitl_id)
            {
                let _ = tx.send(crate::hitl::HitlDecision {
                    allow,
                    reason: None,
                    surface: Some("cli".into()),
                    awaiting_human: false,
                });
            }
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), run)
        .await
        .expect("a parked turn must end once answered or out of time")
        .unwrap();
    assert!(
        parking.list().is_empty(),
        "nothing stays listed after the turn"
    );
    (
        Run {
            outcome,
            executed: calls.load(Ordering::Relaxed),
            asked: false,
        },
        listed,
    )
}

#[tokio::test]
async fn a_parked_call_runs_once_a_human_approves_it() {
    let (r, listed) = run_parked(Some(true), std::time::Duration::from_secs(30)).await;
    assert!(listed, "the waiting call must be visible to `hitl pending`");
    assert!(
        matches!(r.outcome, TaskOutcome::Completed(_)),
        "{:?}",
        r.outcome
    );
    assert_eq!(r.executed, 1);
}

#[tokio::test]
async fn a_parked_call_a_human_denies_is_hitl_denied() {
    let (r, _) = run_parked(Some(false), std::time::Duration::from_secs(30)).await;
    let e = error_of(&r.outcome);
    assert_eq!(e.code, "hitl_denied", "{}", e.message);
    assert_eq!(r.executed, 0);
}

/// Nobody answers: still fail-closed, still "needs a human", and the message
/// says the wait ran out rather than pretending nobody was asked.
#[tokio::test]
async fn a_parked_call_nobody_answers_is_approval_required() {
    let (r, listed) = run_parked(None, std::time::Duration::from_millis(200)).await;
    assert!(listed);
    let e = error_of(&r.outcome);
    assert_eq!(e.code, "hitl_approval_required", "{}", e.message);
    assert!(e.recoverable);
    assert!(e.message.contains("nobody answered"), "{}", e.message);
    assert_eq!(r.executed, 0, "fail-closed: never runs unapproved");
}
