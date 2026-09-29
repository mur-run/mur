use super::*;

#[test]
fn user_message_carries_pasted_image() {
    let msg = Message {
        role: "user".into(),
        parts: vec![
            MessagePart::Text {
                text: "what is this?".into(),
            },
            MessagePart::Data {
                mime_type: "image/png".into(),
                data: serde_json::json!({ "base64": "QkFTRTY0" }),
            },
        ],
    };
    match user_message(&msg) {
        crate::llm::RichMessage::ImageText {
            media_type,
            data,
            text,
            ..
        } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "QkFTRTY0");
            assert_eq!(text, "what is this?");
        }
        other => panic!("expected ImageText, got {other:?}"),
    }
}

#[test]
fn user_message_text_only_when_no_image() {
    let msg = Message {
        role: "user".into(),
        parts: vec![MessagePart::Text { text: "hi".into() }],
    };
    assert!(matches!(
        user_message(&msg),
        crate::llm::RichMessage::Text { .. }
    ));
}

// ── Drain tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn drain_idle_runner_returns_true_immediately() {
    let runner = TaskRunner::new_stub_echo();
    // An idle runner (no in-flight tasks) must return true within the timeout.
    let ok = runner
        .await_idle(std::time::Duration::from_millis(200))
        .await;
    assert!(ok, "idle runner should drain immediately");
}

#[tokio::test]
async fn drain_rejects_new_turns_after_begin_drain() {
    let runner = TaskRunner::new_stub_echo();
    runner.begin_drain();
    // New turns must be rejected with a transient Failed outcome.
    let outcome = runner.run_sync(ping_spec()).await;
    match outcome {
        TaskOutcome::Failed(task) => {
            let err = task.error.expect("drained turn must have an error");
            assert!(
                err.recoverable,
                "drain rejection must be marked recoverable"
            );
            assert!(
                err.message.contains("draining"),
                "error message must mention draining, got: {}",
                err.message
            );
        }
        other => panic!("expected Failed after drain, got {other:?}"),
    }
}

#[tokio::test]
async fn drain_still_idle_after_rejected_turn() {
    // A rejected turn must NOT register in the registry, so await_idle stays true.
    let runner = TaskRunner::new_stub_echo();
    runner.begin_drain();
    let _ = runner.run_sync(ping_spec()).await;
    let ok = runner
        .await_idle(std::time::Duration::from_millis(100))
        .await;
    assert!(
        ok,
        "registry must be clean after a rejected (draining) turn"
    );
}

#[tokio::test]
async fn drain_start_async_does_not_register_working_entry() {
    // After begin_drain(), start_async must NOT leave a Working entry, so
    // await_idle returns true immediately (no phantom task blocks shutdown).
    let runner = TaskRunner::new_stub_echo();
    runner.begin_drain();
    let handle = runner.start_async(ping_spec());
    // The handle resolves to Failed (transient rejection).
    let outcome = handle.await_completion().await;
    match outcome {
        TaskOutcome::Failed(task) => {
            let err = task.error.expect("drained async turn must have an error");
            assert!(err.recoverable, "async drain rejection must be recoverable");
            assert!(
                err.message.contains("draining"),
                "error message must mention draining, got: {}",
                err.message
            );
        }
        other => panic!("expected Failed from start_async after drain, got {other:?}"),
    }
    // await_idle must not hang — no Working entry was registered.
    let ok = runner
        .await_idle(std::time::Duration::from_millis(100))
        .await;
    assert!(
        ok,
        "registry must have no Working entries after start_async drain rejection"
    );
}

#[tokio::test]
async fn steering_register_inject_unregister() {
    let runner = TaskRunner::new_stub_echo();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    runner.register_steering("t1", tx).await;
    runner
        .inject_steering("t1", "use ripgrep".into())
        .await
        .unwrap();
    assert_eq!(rx.recv().await.as_deref(), Some("use ripgrep"));
    // unknown task → error
    assert!(runner.inject_steering("nope", "x".into()).await.is_err());
    runner.unregister_steering("t1").await;
    assert!(runner.inject_steering("t1", "y".into()).await.is_err());
}

/// A client that fails the first `fails` calls with `LlmError::RateLimit`
/// carrying `retry_after`, then succeeds. Records the virtual-time instant
/// of every call so a test can assert on the gaps between them.
struct RateLimitedLlm {
    fails: usize,
    retry_after: Option<std::time::Duration>,
    calls: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for RateLimitedLlm {
    async fn generate(
        &self,
        _req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, LlmError> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(tokio::time::Instant::now());
            calls.len()
        };
        if n <= self.fails {
            Err(LlmError::RateLimit(self.retry_after))
        } else {
            Ok(end_turn_response("recovered"))
        }
    }
    fn model_name(&self) -> &str {
        "rate-limited-stub"
    }
}

/// Drive one turn against `RateLimitedLlm` under paused time and return the
/// gaps between consecutive LLM calls — i.e. how long the loop actually
/// slept before each retry.
async fn rate_limit_retry_gaps(
    fails: usize,
    retry_after: Option<std::time::Duration>,
) -> Vec<std::time::Duration> {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(RateLimitedLlm {
            fails,
            retry_after,
            calls: calls.clone(),
        }))
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0),
    );
    let _ = runner.run_sync(loop_spec("rate limited")).await;
    let calls = calls.lock().unwrap();
    calls.windows(2).map(|w| w[1] - w[0]).collect()
}

/// §4: when the server says how long to wait, the loop waits THAT long —
/// not its own 2s guess. This is the whole point of the passthrough: MUR's
/// own gateway hands out a 5s permit window, and retrying at 2s burns an
/// attempt against a permit that cannot possibly be free yet.
#[tokio::test(start_paused = true)]
async fn rate_limit_retry_honours_retry_after() {
    let gaps = rate_limit_retry_gaps(1, Some(std::time::Duration::from_secs(45))).await;
    assert_eq!(gaps.len(), 1, "one retry: {gaps:?}");
    assert_eq!(
        gaps[0],
        std::time::Duration::from_secs(45),
        "slept the server's retry-after, not the backoff guess"
    );
}

/// A retry-after past the clamp is capped: a live turn must not park for
/// an hour on a header. Anything longer belongs to the durable path.
#[tokio::test(start_paused = true)]
async fn rate_limit_retry_after_is_clamped() {
    let gaps = rate_limit_retry_gaps(1, Some(std::time::Duration::from_secs(3600))).await;
    assert_eq!(gaps.len(), 1, "one retry: {gaps:?}");
    assert_eq!(
        gaps[0],
        crate::llm::RETRY_AFTER_MAX,
        "clamped to RETRY_AFTER_MAX"
    );
}

/// The regression guard for §4: with no header, the exponential schedule
/// is byte-for-byte what it was before this change — 2s, 4s, 8s.
#[tokio::test(start_paused = true)]
async fn rate_limit_retry_falls_back_to_backoff() {
    let gaps = rate_limit_retry_gaps(3, None).await;
    assert_eq!(
        gaps,
        vec![
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(4),
            std::time::Duration::from_secs(8),
        ],
        "unchanged fallback schedule"
    );
}

#[test]
fn rate_limit_backoff_delay_matches_spec() {
    assert_eq!(
        rate_limit_backoff_delay(1),
        std::time::Duration::from_secs(2)
    );
    assert_eq!(
        rate_limit_backoff_delay(2),
        std::time::Duration::from_secs(4)
    );
    assert_eq!(
        rate_limit_backoff_delay(3),
        std::time::Duration::from_secs(8)
    );
}

#[test]
fn rate_limit_retry_constants_are_sane() {
    // Must retry at least once for the backoff to matter, and stay small
    // enough that a still-limited account fails a turn in a bounded time
    // (2s + 4s + 8s = 14s at the current base) rather than hanging.
    // Const items, so editing a constant out of range fails the BUILD
    // rather than only this test.
    const _: () = assert!(MAX_RATE_LIMIT_RETRIES >= 1);
    const _: () = assert!(MAX_RATE_LIMIT_RETRIES <= 5);
    const _: () = assert!(RATE_LIMIT_BACKOFF_BASE.as_millis() >= 1);
    let max_delay = rate_limit_backoff_delay(MAX_RATE_LIMIT_RETRIES);
    assert!(max_delay <= std::time::Duration::from_secs(60));
}
