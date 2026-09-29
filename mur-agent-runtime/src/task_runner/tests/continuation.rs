use super::*;

/// Build the standard TaskSpec used by the #001 continuation tests.
fn continuation_spec(text: &str, attended: bool) -> TaskSpec {
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
        attended,
        deadline_secs: None,
    }
}

fn continuation_runner(
    responses: Vec<crate::llm::LlmResponse>,
    autonomy: mur_common::hitl::Autonomy,
) -> Arc<TaskRunner> {
    use crate::llm::stub::SequenceLlm;
    let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
    let pa: Arc<
        tokio::sync::Mutex<
            HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(pa)
            .with_notifier(notif_tx)
            .with_hitl_timeout_secs(1)
            .with_autonomy(autonomy)
            .with_iteration_ceiling(10),
    )
}

/// ISSUE #001, the bug itself. Under `autonomy: continue` a model that
/// ends the turn early is nudged back into the loop exactly once, and the
/// reply is the SECOND turn's text. Before the seam at the termination
/// branch existed, "已授權工作持續推進" lived only in the prompt and this
/// returned "Stopping here" — the runtime had no way to re-enter.
#[tokio::test]
async fn continue_autonomy_resumes_a_turn_that_ended_early() {
    let runner = continuation_runner(
        vec![
            end_turn_response("Stopping here to check with you."),
            end_turn_response("RESUMED: finished the remaining work."),
        ],
        mur_common::hitl::Autonomy::Continue,
    );
    let outcome = runner
        .run_sync(continuation_spec("do the thing", false))
        .await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply.contains("RESUMED"),
        "turn should have been carried onward; got: {reply}"
    );
}

/// The bound. A model that keeps ending the turn is nudged `MAX_CONTINUATIONS`
/// times and then settles — the continuation must never become a second,
/// unbounded loop running beside the iteration ceiling.
#[tokio::test]
async fn continuation_is_bounded_and_then_settles() {
    let runner = continuation_runner(
        vec![
            end_turn_response("first stop"),
            end_turn_response("second stop"),
            end_turn_response("third stop"),
            end_turn_response("fourth stop"),
        ],
        mur_common::hitl::Autonomy::Continue,
    );
    let outcome = runner
        .run_sync(continuation_spec("do the thing", false))
        .await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply = task.messages.last().map(text_of).unwrap_or_default();
    // One nudge = the SECOND response settles, never the third.
    assert!(
        reply.contains("second stop"),
        "expected settle after exactly {} nudge(s); got: {reply}",
        mur_common::hitl::MAX_CONTINUATIONS
    );
}

/// The default must be the handback. An agent whose profile says nothing
/// about autonomy behaves exactly as it did before this feature existed.
#[tokio::test]
async fn default_autonomy_hands_back_unchanged() {
    let runner = continuation_runner(
        vec![
            end_turn_response("Stopping here to check with you."),
            end_turn_response("RESUMED: should never be reached."),
        ],
        mur_common::hitl::Autonomy::default(),
    );
    let outcome = runner
        .run_sync(continuation_spec("do the thing", false))
        .await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply.contains("Stopping here"),
        "default autonomy must hand back; got: {reply}"
    );
    assert!(!reply.contains("RESUMED"), "default must not continue");
}

/// `Review` is a handback too: it changes what the agent is asked to
/// produce, never whether the runtime re-enters the loop.
#[tokio::test]
async fn review_autonomy_hands_back() {
    let runner = continuation_runner(
        vec![
            end_turn_response("Done, please review."),
            end_turn_response("RESUMED: should never be reached."),
        ],
        mur_common::hitl::Autonomy::Review,
    );
    let outcome = runner
        .run_sync(continuation_spec("do the thing", false))
        .await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        !reply.contains("RESUMED"),
        "review must not continue: {reply}"
    );
}

/// #001 §6 A1 at the runtime seam: a turn stopped by the iteration ceiling
/// takes its graceful exit and is NOT nudged, even under `continue`. The
/// two mechanisms must not fight over the same turn.
#[tokio::test]
async fn continuation_does_not_fire_on_a_budget_stop() {
    use crate::llm::stub::SequenceLlm;
    let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
    let pa: Arc<
        tokio::sync::Mutex<
            HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
            tool_call_response("id-0", "echo step-0"),
            tool_call_response("id-1", "echo step-1"),
            tool_call_response("id-2", "echo step-2"),
            end_turn_response("SUMMARY: ceiling reached."),
        ])))
        .with_pending_approvals(pa)
        .with_notifier(notif_tx)
        .with_hitl_timeout_secs(1)
        .with_autonomy(mur_common::hitl::Autonomy::Continue)
        .with_iteration_ceiling(3),
    );
    let outcome = runner.run_sync(continuation_spec("loop", false)).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let usage = task.usage.expect("graceful exit must populate usage");
    assert_eq!(
        usage["stop_reason"], "iteration_ceiling",
        "budget stop must keep its own exit, not be nudged: usage={usage}"
    );
}

#[tokio::test]
async fn max_iterations_exceeded_yields_completed_with_summary() {
    use crate::llm::stub::SequenceLlm;
    // Three tool_use turns fill the cap; the fourth call is the graceful,
    // tools-disabled summary turn. SequenceLlm wraps modulo len, so a
    // 4-element vector maps loop turns to indices 0,1,2 and the summary
    // turn to index 3 deterministically.
    // Distinct commands per turn so the doom-loop guard (identical-call
    // detection) does NOT fire first — this test must exercise the
    // iteration cap specifically.
    let responses: Vec<crate::llm::LlmResponse> = vec![
        tool_call_response("id-0", "echo step-0"),
        tool_call_response("id-1", "echo step-1"),
        tool_call_response("id-2", "echo step-2"),
        end_turn_response("SUMMARY: completed nothing; build untouched; remaining: all."),
    ];
    let llm = SequenceLlm::new(responses);
    let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
    let pa: Arc<
        tokio::sync::Mutex<
            HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(llm))
            .with_pending_approvals(pa)
            .with_notifier(notif_tx)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(3),
    );
    let spec = TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![mur_common::a2a::MessagePart::Text {
                text: "loop".into(),
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
    };
    let outcome = runner.run_sync(spec).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (graceful exit), got {outcome:?}");
    };
    // The returned reply carries the summarizing turn's text.
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.contains("SUMMARY:"),
        "expected summary in reply, got: {reply_text}"
    );
    // The stop reason is surfaced in usage for callers to inspect.
    let usage = task.usage.expect("graceful exit must populate usage");
    assert_eq!(usage["stop_reason"], "iteration_ceiling", "usage={usage}");
    assert_eq!(usage["iterations"], 3, "usage={usage}");
}

/// Step 4 (doom-loop detection): an LLM that emits the SAME tool call every
/// turn must be aborted after ~3 identical calls with stop_reason
/// "loop_detected" — well before the iteration cap (50 here). This catches
/// blind identical retries quickly regardless of how high the cap is.
#[tokio::test]
async fn doom_loop_detected_yields_completed_with_summary() {
    use crate::llm::stub::SequenceLlm;
    // Identical args every turn. With no tool registered, every call
    // resolves to the same "unknown tool" result, so the full
    // (tool, args, RESULT) fingerprint is identical each turn. The 3rd
    // identical fingerprint trips the guard on iteration index 2; the 4th
    // call is the graceful summary.
    let responses: Vec<crate::llm::LlmResponse> = vec![
        tool_call_response("same-0", "echo identical"),
        tool_call_response("same-1", "echo identical"),
        tool_call_response("same-2", "echo identical"),
        end_turn_response("LOOP SUMMARY: stuck retrying; build untouched; need new approach."),
    ];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            // High cap so only doom-loop detection can stop us this fast.
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("doom")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (doom-loop graceful exit), got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.contains("LOOP SUMMARY"),
        "expected summary in reply, got: {reply_text}"
    );
    let usage = task.usage.expect("doom-loop exit must populate usage");
    assert_eq!(usage["stop_reason"], "loop_detected", "usage={usage}");
    // Aborted within ~3 iterations, far below the cap of 50.
    let iters = usage["iterations"]
        .as_u64()
        .expect("iterations is a number");
    assert!(iters < 5, "expected early abort, got {iters} iterations");
}
