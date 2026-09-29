use super::*;

/// Fix B — truncation is self-correcting, not a silent loop. When a turn
/// stops with `MaxTokens` AND carries tool_calls (cut off mid-tool_use),
/// the loop must NOT execute the malformed call. Instead it appends a
/// truncation-guidance user message and continues, letting the model
/// recover with a shorter, well-formed turn.
#[tokio::test]
async fn truncated_tool_use_injects_guidance_and_recovers() {
    use crate::llm::stub::SequenceLlm;
    // Turn 0: truncated mid-tool_use (MaxTokens + a tool_call).
    // Turn 1: a clean end-turn — the recovery the model produces after the
    // guidance nudge.
    let responses: Vec<crate::llm::LlmResponse> = vec![
        truncated_tool_call_response("trunc-0"),
        end_turn_response("RECOVERED: produced a shorter response."),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingBashTool {
                calls: calls.clone(),
                ..Default::default()
            })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "bash".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("truncate")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (recovered turn), got {outcome:?}");
    };
    // The malformed (truncated) tool call must NOT have been executed.
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "truncated tool_use must not be executed"
    );
    // The following well-formed turn proceeds and is the natural terminus —
    // no budget tripped, so usage carries token counts but NO stop_reason.
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.contains("RECOVERED"),
        "expected the recovery turn's reply, got: {reply_text}"
    );
    let usage = task
        .usage
        .expect("usage is always populated with token counts");
    assert!(
        usage.get("stop_reason").is_none(),
        "natural end_turn after recovery must not populate a budget stop_reason; usage={usage:?}",
    );
    assert!(
        usage.get("input_tokens").is_some() && usage.get("output_tokens").is_some(),
        "usage must report real token counts; usage={usage:?}",
    );
}

/// Regression: a turn that burns its whole `max_tokens` budget inside a
/// thinking block — no text, no tool_use, just `stop_reason: MaxTokens` —
/// must recover the same way a truncated-mid-tool_use turn does, not
/// surface a hard error to the user (this was the "invalid response:
/// empty streamed response" crash reported from `murmur`).
#[tokio::test]
async fn truncated_thinking_only_injects_guidance_and_recovers() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> = vec![
        truncated_thinking_only_response(),
        end_turn_response("RECOVERED: produced a shorter response."),
    ];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("truncate-thinking")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (recovered turn), got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.contains("RECOVERED"),
        "expected the recovery turn's reply, got: {reply_text}"
    );
}

/// Fix A (#715): a turn whose FINAL answer stops at `MaxTokens` (text
/// present, no tool_calls) must not be passed off as complete — the reply
/// gets the visible truncation marker appended and `Task.usage` carries
/// `"truncated": true`.
#[tokio::test]
async fn max_tokens_final_answer_gets_marker_and_usage_flag() {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![truncated_text_response("A long spec cut mid-wo")];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let outcome = runner.run_sync(loop_spec("truncate-final-answer")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.starts_with("A long spec cut mid-wo"),
        "truncated text must be preserved, got: {reply_text}"
    );
    assert!(
        // The marker stays inline where the truncation happened; the
        // settlement now follows it, so it is no longer the last thing in
        // the reply.
        reply_text.contains(crate::llm::MAX_TOKENS_TRUNCATION_MARKER),
        "reply must carry the visible truncation marker, got: {reply_text}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert_eq!(
        usage["truncated"], true,
        "usage must flag the truncation; usage={usage:?}"
    );
}

/// Counterpart to the marker test: a clean end_turn must carry neither the
/// marker nor the `truncated` usage key (the flag is additive-only).
#[tokio::test]
async fn clean_end_turn_has_no_truncation_marker_or_flag() {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![end_turn_response("complete answer")];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let outcome = runner.run_sync(loop_spec("clean-end-turn")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        !reply_text.contains(crate::llm::MAX_TOKENS_TRUNCATION_MARKER),
        "clean turn must not carry the marker, got: {reply_text}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert!(
        usage.get("truncated").is_none(),
        "clean turn must not populate the truncated flag; usage={usage:?}"
    );
}

/// Same marker + flag behavior on the non-agentic `run_llm` path (runner
/// built without pending approvals — e.g. companion / plain generate).
#[tokio::test]
async fn run_llm_path_marks_max_tokens_truncation() {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![truncated_text_response("plain reply cut mid-wo")];
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)));
    let outcome = runner.run_sync(loop_spec("truncate-run-llm")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.ends_with(crate::llm::MAX_TOKENS_TRUNCATION_MARKER),
        "run_llm reply must end with the truncation marker, got: {reply_text}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert_eq!(
        usage["truncated"], true,
        "usage must flag the truncation; usage={usage:?}"
    );
}

fn interrupted_text_response(text: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: text.into(),
        input_tokens: 5,
        // 0 on purpose: usage arrives in the final frame, which never
        // came. See `mark_stream_interruption`.
        output_tokens: 0,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: crate::llm::StopReason::Interrupted,
    }
}

/// #1287, the agentic path (the deeper of the two response-handling sites).
/// A reply whose stream stopped sending must reach the user marked, and the
/// usage must flag it — the same three destinations the `max_tokens` marker
/// already has.
#[tokio::test]
async fn agentic_path_marks_a_stream_interruption() {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![interrupted_text_response("half an ans")];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let outcome = runner.run_sync(loop_spec("interrupted-agentic")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    // `contains`, not `ends_with`: this path appends a settlement card
    // after the reply. The marker must sit with the answer, before it.
    let (answer, settlement) = reply_text
        .split_once("─ settlement ─")
        .expect("the agentic path always settles");
    // The settlement card opens with a code fence, so trim that too: what
    // must come last is the marker, not the fence.
    let answer_body = answer.trim_end().trim_end_matches('`').trim_end();
    assert!(
        answer_body.ends_with(crate::llm::STREAM_IDLE_TRUNCATION_MARKER.trim_end()),
        "an interrupted reply must not look complete, got: {answer_body}"
    );
    // The settlement names the cause and the knob, because `StopKind` keeps
    // them apart. Without the added variant this said "end_turn".
    assert!(
        settlement.contains("stream interrupted"),
        "the card must say what stopped the turn: {settlement}"
    );
    assert!(
        settlement.contains("MUR_LLM_IDLE_TIMEOUT_SECS"),
        "and name the knob that changes it: {settlement}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert_eq!(
        usage["truncated"], true,
        "an interrupted reply is truncated; usage={usage:?}"
    );
}

/// The SECOND site, `run_llm` (companion / plain generate). Both sites are
/// asserted because this repo has already shipped a bug where one of a pair
/// of identical response-handling sites was updated and the other was not.
#[tokio::test]
async fn run_llm_path_marks_a_stream_interruption() {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![interrupted_text_response("plain reply cut off")];
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)));
    let outcome = runner.run_sync(loop_spec("interrupted-run-llm")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.ends_with(crate::llm::STREAM_IDLE_TRUNCATION_MARKER),
        "run_llm reply must end with the interruption marker, got: {reply_text}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert_eq!(usage["truncated"], true, "usage={usage:?}");
}

/// The ledger tells the two truncations apart even though the usage flag
/// does not: `end_turn` for an interrupted turn would be a falsehood in a
/// durable audit record.
#[test]
fn the_ledger_distinguishes_an_interruption_from_a_clean_end() {
    use crate::turn_ledger::StopKind;
    assert_eq!(StopKind::StreamInterrupted.as_str(), "stream interrupted");
    assert!(!StopKind::StreamInterrupted.is_clean());
    assert!(
        StopKind::StreamInterrupted
            .remedy("a1")
            .is_some_and(|r| r.contains("MUR_LLM_IDLE_TIMEOUT_SECS")),
        "the remedy must name the knob that changes this"
    );
}

/// Returns `InvalidResponse("empty streamed response")` on its first call,
/// then delegates to `inner` — used to test the bounded retry for a
/// transient empty-stream hiccup (task_runner's LLM call site).
struct EmptyStreamOnceThenLlm {
    inner: crate::llm::stub::SequenceLlm,
    failed_once: std::sync::atomic::AtomicBool,
}

impl EmptyStreamOnceThenLlm {
    fn new(responses: Vec<crate::llm::LlmResponse>) -> Self {
        Self {
            inner: crate::llm::stub::SequenceLlm::new(responses),
            failed_once: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for EmptyStreamOnceThenLlm {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        if !self.failed_once.swap(true, Ordering::Relaxed) {
            return Err(crate::llm::LlmError::InvalidResponse(
                "empty streamed response".into(),
            ));
        }
        self.inner.generate(req).await
    }
    fn model_name(&self) -> &str {
        "empty-stream-once-then-stub"
    }
}

/// Regression: a transient empty-stream error (a momentary network/proxy
/// hiccup) must be retried once and recover silently, not surface as a
/// hard task error or a blank agent reply.
#[tokio::test]
async fn empty_stream_error_retries_once_and_recovers() {
    let responses = vec![end_turn_response("RECOVERED: retried after empty stream.")];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(EmptyStreamOnceThenLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("empty-stream-retry")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (recovered turn), got {outcome:?}");
    };
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(
        reply_text.contains("RECOVERED"),
        "expected the recovery turn's reply, got: {reply_text}"
    );
}

/// First call: streams visible text AND calls a tool (the shape of a reply
/// that ends with `suggest_replies`), so the loop goes back to the model.
/// Every later call: `empty streamed response` — outlasting the one retry.
struct TextThenEmptyStreamLlm {
    calls: std::sync::atomic::AtomicUsize,
    first_text: String,
    /// Tool the first reply calls. `suggest_replies` exercises the
    /// silence-ends-the-turn rule; anything else is a real failure.
    tool: &'static str,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for TextThenEmptyStreamLlm {
    async fn generate(
        &self,
        _req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) > 0 {
            return Err(crate::llm::LlmError::InvalidResponse(
                "empty streamed response".into(),
            ));
        }
        Ok(crate::llm::LlmResponse {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            text: self.first_text.clone(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: "s1".into(),
                tool_name: self.tool.into(),
                input: serde_json::json!({"replies": ["照 A 做"]}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        })
    }
    fn model_name(&self) -> &str {
        "text-then-empty-stream"
    }
}

/// Regression (turn 243, "照 A 修"): the A/B/C table was streamed to the
/// user, then the follow-up LLM call hit two empty streams and the turn
/// failed. Only successful turns were remembered, and the CLI threads the
/// next turn only on a reply, so the next turn had never heard of option
/// A. Text the user has already seen must settle the turn — as a
/// truncation, the same way a stream that went quiet does (#1287) — so
/// memory, the reply, and context threading all carry it.
#[tokio::test]
async fn streamed_text_survives_a_later_llm_failure_in_the_same_turn() {
    const TABLE: &str = "| A | 先寫 regression test |\n| B | 直接修 |\n| C | 先不動 |";
    let runner = TaskRunner::with_llm(Arc::new(TextThenEmptyStreamLlm {
        calls: std::sync::atomic::AtomicUsize::new(0),
        first_text: TABLE.into(),
        tool: "read_file",
    }))
    .with_pending_approvals(empty_pending_approvals())
    .with_notifier(tokio::sync::mpsc::channel(16).0)
    .with_hitl_timeout_secs(1)
    .with_iteration_ceiling(50);
    let (sink, mut seen) = tokio::sync::mpsc::channel(64);

    let outcome = runner
        .run_sync_streaming(user_turn("列出選項", "t-table", None), sink, None)
        .await;

    let mut streamed = String::new();
    while let Ok(d) = seen.try_recv() {
        streamed.push_str(&d.text);
    }
    assert!(streamed.contains(TABLE), "table must have reached the user");

    // The turn settles on what the user saw, marked as cut short.
    let TaskOutcome::Completed(task) = outcome else {
        panic!("seen text must settle the turn, got {outcome:?}");
    };
    let reply = task.messages.last().expect("reply");
    let reply_text = text_of(reply);
    assert!(
        reply_text.contains(TABLE),
        "reply must carry the table: {reply_text}"
    );
    assert!(
        reply_text.contains(crate::llm::LLM_FAILED_TRUNCATION_MARKER),
        "reply must say it was cut short: {reply_text}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert_eq!(usage["truncated"], true, "usage={usage:?}");
    // The ledger tells the truth: not end_turn, and the error survives.
    let ledger = ledger_of(reply).expect("ledger part");
    let crate::turn_ledger::StopKind::LlmFailedAfterOutput { error } = &ledger.stop else {
        panic!("stop must record the failure, got {:?}", ledger.stop);
    };
    assert!(error.contains("empty streamed response"), "error: {error}");

    // The symptom: the next turn, threaded on this turn's id, recalls
    // what the user saw.
    let prior = runner.conversations.lock().unwrap().prior(Some("t-table"));
    let recalled = prior.iter().any(|m| {
        matches!(m, crate::llm::RichMessage::Text { role, content }
            if role == "agent" && content.contains("| A |"))
    });
    assert!(
        recalled,
        "streamed-then-failed turn left no trace in memory: {prior:?}"
    );
}

/// Root cause of the turn-243 "two empty streams": after `suggest_replies`
/// (a no-op) the model has nothing left to say and ends with no text. The
/// provider reports that as `empty streamed response`, the one retry asks
/// the same question and gets the same silence, and the turn looked failed.
/// Silence right after offering replies is the model ending its turn, so
/// it settles cleanly: the shown text is the reply, with no truncation
/// marker, and the ledger says `end_turn`.
#[tokio::test]
async fn silence_after_suggest_replies_ends_the_turn_cleanly() {
    const TABLE: &str = "| A | 先寫 regression test |\n| B | 直接修 |";
    let llm = Arc::new(TextThenEmptyStreamLlm {
        calls: std::sync::atomic::AtomicUsize::new(0),
        first_text: TABLE.into(),
        tool: "suggest_replies",
    });
    let runner = TaskRunner::with_llm(llm.clone())
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_hitl_timeout_secs(1)
        .with_iteration_ceiling(50);
    let (sink, _seen) = tokio::sync::mpsc::channel(64);

    let outcome = runner
        .run_sync_streaming(user_turn("列出選項", "t-silence", None), sink, None)
        .await;

    let TaskOutcome::Completed(task) = outcome else {
        panic!("silence after suggest_replies must complete, got {outcome:?}");
    };
    let reply = task.messages.last().expect("reply");
    let reply_text = text_of(reply);
    assert!(
        reply_text.starts_with(TABLE),
        "reply is what was shown: {reply_text}"
    );
    assert!(
        !reply_text.contains(crate::llm::LLM_FAILED_TRUNCATION_MARKER),
        "silence is not a failure: {reply_text}"
    );
    let usage = task.usage.expect("usage is always populated");
    assert_ne!(
        usage["truncated"], true,
        "not a truncation: usage={usage:?}"
    );
    let ledger = ledger_of(reply).expect("ledger part");
    assert!(
        matches!(ledger.stop, crate::turn_ledger::StopKind::EndTurn),
        "stop must be end_turn, got {:?}",
        ledger.stop
    );
    // Silence is an answer, not a blip: no retry of the same question.
    assert_eq!(
        llm.calls.load(Ordering::Relaxed),
        2,
        "one call for the table, one that came back silent"
    );
}

/// First call: the answer AND a tool call. Second call: one closing line,
/// no tools. The shape of turn 274, where the model offered replies and
/// then asked "下一步你想怎麼做？" instead of going silent.
struct AnswerThenFollowUpLlm {
    calls: std::sync::atomic::AtomicUsize,
    tool: &'static str,
}

const ANSWER: &str = "| D2b | 用 deps installer 裝 Lightpanda |\n\n`provision.rs:324-326` 的註解寫著：\n\n```rust\n// prefer aura/lightpanda\n```";
const FOLLOW_UP: &str = "下一步你想怎麼做？";

#[async_trait::async_trait]
impl crate::llm::LlmClient for AnswerThenFollowUpLlm {
    async fn generate(
        &self,
        _req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        let first = self.calls.fetch_add(1, Ordering::Relaxed) == 0;
        Ok(crate::llm::LlmResponse {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            text: if first { ANSWER } else { FOLLOW_UP }.into(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: if first {
                vec![crate::llm::ToolCallResult {
                    call_id: "s1".into(),
                    tool_name: self.tool.into(),
                    input: serde_json::json!({"replies": ["照 A 做"], "path": "x"}),
                }]
            } else {
                Vec::new()
            },
            stop_reason: if first {
                crate::llm::StopReason::ToolUse
            } else {
                crate::llm::StopReason::EndTurn
            },
        })
    }
    fn model_name(&self) -> &str {
        "answer-then-follow-up"
    }
}

async fn reply_of_answer_then_follow_up(tool: &'static str) -> String {
    let runner = TaskRunner::with_llm(Arc::new(AnswerThenFollowUpLlm {
        calls: std::sync::atomic::AtomicUsize::new(0),
        tool,
    }))
    .with_pending_approvals(empty_pending_approvals())
    .with_notifier(tokio::sync::mpsc::channel(16).0)
    .with_hitl_timeout_secs(1)
    .with_iteration_ceiling(50);
    let (sink, _seen) = tokio::sync::mpsc::channel(64);
    let outcome = runner
        .run_sync_streaming(
            user_turn("Lightpanda 也一起裝", "t-follow", None),
            sink,
            None,
        )
        .await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("turn must complete, got {outcome:?}");
    };
    text_of(task.messages.last().expect("reply"))
}

/// Regression (turn 274): the answer streamed, `suggest_replies` ran, and
/// the model added one closing line. The reply was that line alone. The
/// CLI puts the reply in place of the streaming text, and
/// `suggest_replies` draws no step card, so nothing on screen marked a
/// boundary between the two calls. The part of the answer still in the
/// band vanished the moment the chooser opened, and only the closing line
/// reached the channel log and memory. `suggest_replies` is a no-op: the
/// text before it is part of the reply.
#[tokio::test]
async fn text_before_suggest_replies_stays_in_the_reply() {
    let reply = reply_of_answer_then_follow_up("suggest_replies").await;
    assert!(
        reply.starts_with(ANSWER),
        "the answer before suggest_replies was dropped: {reply}"
    );
    assert!(
        reply.contains(FOLLOW_UP),
        "the closing line is part of the reply too: {reply}"
    );
}

/// Counterpart: a real tool draws a card, and the card freezes the text
/// above it on screen. The reply is only what came after it, as before.
#[tokio::test]
async fn text_before_a_real_tool_stays_out_of_the_reply() {
    let reply = reply_of_answer_then_follow_up("read_file").await;
    assert!(
        !reply.contains("Lightpanda"),
        "text above a step card belongs to the frozen segment: {reply}"
    );
    assert!(reply.starts_with(FOLLOW_UP), "{reply}");
}

/// Counterpart: a failure before the user saw anything is still a plain
/// failure — nothing to keep, nothing to remember.
#[tokio::test]
async fn llm_failure_before_any_output_still_fails_and_forgets() {
    struct AlwaysEmpty;
    #[async_trait::async_trait]
    impl crate::llm::LlmClient for AlwaysEmpty {
        async fn generate(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
            Err(crate::llm::LlmError::InvalidResponse(
                "empty streamed response".into(),
            ))
        }
        fn model_name(&self) -> &str {
            "always-empty"
        }
    }
    let runner = TaskRunner::with_llm(Arc::new(AlwaysEmpty))
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_hitl_timeout_secs(1)
        .with_iteration_ceiling(50);
    let (sink, _seen) = tokio::sync::mpsc::channel(64);

    let outcome = runner
        .run_sync_streaming(user_turn("hi", "t-nothing", None), sink, None)
        .await;

    assert!(
        matches!(outcome, TaskOutcome::Failed(_)),
        "no visible output → Failed, got {outcome:?}"
    );
    let prior = runner
        .conversations
        .lock()
        .unwrap()
        .prior(Some("t-nothing"));
    assert!(
        prior.is_empty(),
        "failed turn must not be remembered: {prior:?}"
    );
}

#[tokio::test]
async fn loop_ends_on_end_turn_no_tools() {
    use crate::llm::stub::SequenceLlm;
    let llm = SequenceLlm::new(vec![end_turn_response("Completed.")]);
    let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
    let pa: Arc<
        tokio::sync::Mutex<
            HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(llm))
            .with_pending_approvals(pa)
            .with_notifier(notif_tx),
    );
    let spec = TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![mur_common::a2a::MessagePart::Text {
                text: "hello".into(),
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
    assert!(matches!(outcome, TaskOutcome::Completed(_)));
}
