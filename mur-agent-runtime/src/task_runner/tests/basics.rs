use super::*;

#[test]
fn ctx_fill_measures_one_call_not_runner_lifetime() {
    // A long session: 1M tokens sent over its life, but the latest call
    // carried only 20k. Fill must reflect the 20k, or the adaptive cutoff
    // fires forever once the lifetime total passes the threshold.
    let fill = context_fill_ratio(20_000, 200_000);
    assert!((fill - 0.1).abs() < 1e-9, "fill = {fill}");
    // First turn: no call yet → empty context, never a cutoff.
    assert_eq!(context_fill_ratio(0, 200_000), 0.0);
    // Degenerate config and overflow both stay in [0, 1].
    assert_eq!(context_fill_ratio(5_000, 0), 0.0);
    assert_eq!(context_fill_ratio(900_000, 200_000), 1.0);
}

/// Stub hook: replaces any tool output longer than 10 chars with
/// "OFFLOADED" (stands in for CompressHook's size-gated offload). Proves
/// the turn loop consumes `replace_output` rather than discarding it.
struct ReplaceBigHook;
#[async_trait::async_trait]
impl crate::hooks::Hook for ReplaceBigHook {
    fn name(&self) -> &str {
        "ReplaceBigHook"
    }
    async fn post_tool_use(
        &self,
        _ctx: &HookCtx,
        _call: &ToolCall,
        result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<crate::hooks::PostToolUsePatch, crate::hooks::HookError> {
        let big = result
            .output
            .as_str()
            .map(|s| s.len() > 10)
            .unwrap_or(false);
        Ok(crate::hooks::PostToolUsePatch {
            replace_output: big.then(|| serde_json::Value::String("OFFLOADED".into())),
        })
    }
}

/// Records the `duration_ms` every hook was handed, in call order.
struct RecordDurationHook(std::sync::Mutex<Vec<u64>>);
#[async_trait::async_trait]
impl crate::hooks::Hook for RecordDurationHook {
    fn name(&self) -> &str {
        "RecordDurationHook"
    }
    async fn post_tool_use(
        &self,
        _ctx: &HookCtx,
        _call: &ToolCall,
        result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<crate::hooks::PostToolUsePatch, crate::hooks::HookError> {
        self.0.lock().unwrap().push(result.duration_ms);
        Ok(crate::hooks::PostToolUsePatch {
            replace_output: None,
        })
    }
}

/// Every `execute_tool` telemetry record on this machine carried
/// `duration_ms: 0` — 8177 of them, no other value — because the
/// `ToolResult` handed to the hook chain was built with a literal zero
/// (issue #1197). Telemetry serialised the zero faithfully, so tool
/// latency read as measured-and-instant rather than never-measured.
#[tokio::test]
async fn post_tool_use_hooks_receive_the_measured_duration() {
    use crate::llm::{ToolCallResult, ToolResultEntry};
    let hook = Arc::new(RecordDurationHook(std::sync::Mutex::new(Vec::new())));
    let chain = Arc::new(HookChain::new(vec![hook.clone()]));
    let runner = TaskRunner::new_stub_echo().with_hook_chain(
        chain,
        HookCtx::for_test_with_home(std::path::PathBuf::from("."), 0),
        CancellationToken::new(),
    );
    let calls = vec![
        ToolCallResult {
            call_id: "c1".into(),
            tool_name: "slow".into(),
            input: serde_json::json!({}),
        },
        ToolCallResult {
            call_id: "c2".into(),
            tool_name: "fast".into(),
            input: serde_json::json!({}),
        },
    ];
    let mut results = vec![
        ToolResultEntry {
            call_id: "c1".into(),
            content: "a".into(),
            is_error: false,
            status: crate::tools::ToolStatus::Ok,
            images: Vec::new(),
        },
        ToolResultEntry {
            call_id: "c2".into(),
            content: "b".into(),
            is_error: false,
            status: crate::tools::ToolStatus::Ok,
            images: Vec::new(),
        },
    ];

    runner
        .apply_post_tool_use(&calls, &mut results, &[42, 7])
        .await;

    assert_eq!(
        *hook.0.lock().unwrap(),
        vec![42, 7],
        "each call's own measured duration must reach its hook, in order"
    );
}

#[tokio::test]
async fn apply_post_tool_use_rewrites_oversized_output() {
    use crate::llm::{ToolCallResult, ToolResultEntry};
    let chain = Arc::new(HookChain::new(vec![Arc::new(ReplaceBigHook)]));
    let runner = TaskRunner::new_stub_echo().with_hook_chain(
        chain,
        HookCtx::for_test_with_home(std::path::PathBuf::from("."), 0),
        CancellationToken::new(),
    );
    let calls = vec![
        ToolCallResult {
            call_id: "c1".into(),
            tool_name: "big".into(),
            input: serde_json::json!({}),
        },
        ToolCallResult {
            call_id: "c2".into(),
            tool_name: "small".into(),
            input: serde_json::json!({}),
        },
    ];
    let mut results = vec![
        ToolResultEntry {
            call_id: "c1".into(),
            content: "this is a large tool output".into(),
            is_error: false,
            status: crate::tools::ToolStatus::Ok,
            images: Vec::new(),
        },
        ToolResultEntry {
            call_id: "c2".into(),
            content: "ok".into(),
            is_error: false,
            status: crate::tools::ToolStatus::Ok,
            images: Vec::new(),
        },
    ];
    runner
        .apply_post_tool_use(&calls, &mut results, &[7, 9])
        .await;
    assert_eq!(
        results[0].content, "OFFLOADED",
        "oversized output rewritten"
    );
    assert_eq!(results[1].content, "ok", "small output untouched");
}

#[tokio::test]
async fn last_activity_starts_at_zero() {
    let runner = TaskRunner::new_stub_echo();
    assert_eq!(runner.last_activity_at(), 0);
}

#[tokio::test]
async fn start_async_bumps_last_activity() {
    let runner = TaskRunner::new_stub_echo();
    let before = chrono::Utc::now().timestamp();
    let _handle = runner.start_async(ping_spec());
    let activity = runner.last_activity_at();
    let after = chrono::Utc::now().timestamp();
    assert!(
        activity >= before && activity <= after,
        "activity={activity} not in [{before},{after}]"
    );
}

#[tokio::test]
async fn run_sync_bumps_last_activity() {
    // Regression: the production inbound path must record activity so idle
    // triggers measure real quiescence (previously only start_async did).
    let runner = TaskRunner::new_stub_echo();
    let before = chrono::Utc::now().timestamp();
    let _ = runner.run_sync(ping_spec()).await;
    let activity = runner.last_activity_at();
    let after = chrono::Utc::now().timestamp();
    assert!(
        activity >= before && activity <= after,
        "activity={activity} not in [{before},{after}]"
    );
}

#[test]
fn task_spec_accepts_optional_task_id() {
    let spec = TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "hi".into() }],
        },
        context_task_id: None,
        task_id: Some("task-fixed-1".to_string()),
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        active_fleet: None,
        active_team: None,
        attended: true,
        deadline_secs: None,
    };
    assert_eq!(spec.task_id.as_deref(), Some("task-fixed-1"));
}

#[tokio::test]
async fn run_sync_uses_supplied_task_id() {
    let runner = TaskRunner::new_stub_echo();
    let spec = TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "hi".into() }],
        },
        context_task_id: None,
        task_id: Some("task-supplied-9".to_string()),
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        active_fleet: None,
        active_team: None,
        attended: true,
        deadline_secs: None,
    };
    let outcome = runner.run_sync(spec).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed")
    };
    assert_eq!(task.id, "task-supplied-9");
}

#[tokio::test]
async fn run_sync_streaming_is_cancellable_by_id() {
    use std::sync::Arc;
    let runner = Arc::new(TaskRunner::new_stub_slow());
    let (tx, _rx) = tokio::sync::mpsc::channel(8); // streaming sink, unused here
    let spec = TaskSpec {
        cwd: None,
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: "slow".into(),
            }],
        },
        context_task_id: None,
        task_id: Some("task-cancelme".to_string()),
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        active_fleet: None,
        active_team: None,
        attended: true,
        deadline_secs: None,
    };
    let r2 = runner.clone();
    let handle = tokio::spawn(async move { r2.run_sync_streaming(spec, tx, None).await });

    // Let the task register its cancel signal, then cancel by the known id.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    runner
        .cancel("task-cancelme")
        .await
        .expect("cancel should succeed");

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("must finish promptly, not wait 60s")
        .expect("join");
    let TaskOutcome::Cancelled(task) = outcome else {
        panic!("expected Cancelled, got {outcome:?}")
    };
    assert_eq!(task.id, "task-cancelme");
    assert_eq!(task.state, TaskState::Cancelled);
}

#[tokio::test]
async fn run_sync_llm_error_yields_failed() {
    // Regression: a provider failure must surface as Failed with a
    // populated error, not a Completed task whose body says "llm error:".
    use crate::llm::stub::StubLlm;
    let yaml = r#"
- match: { contains: "ping" }
  fault: rate_limit
"#;
    let client = std::sync::Arc::new(StubLlm::from_yaml(yaml).unwrap());
    let runner = TaskRunner::with_llm(client);
    let outcome = runner.run_sync(ping_spec()).await;
    match outcome {
        TaskOutcome::Failed(task) => {
            assert_eq!(task.state, TaskState::Failed);
            let err = task.error.expect("Failed task must carry an error");
            assert_eq!(err.code, "llm_error");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// A provider that reports ToolUse but hands back no calls must fail the
/// turn rather than deliver the model's narration as though the tool had
/// run (#938). The text here is the exact fabrication shape observed in
/// the wild: the model states a command's output when no command ran.
#[tokio::test]
async fn tool_use_stop_without_calls_fails_instead_of_fabricating() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> = vec![crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: "The exact output of git rev-list --count HEAD is: FABRICATED-2469".into(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: crate::llm::StopReason::ToolUse,
    }];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("fabricate")).await;
    match outcome {
        TaskOutcome::Failed(task) => {
            // Search the MESSAGES, not the whole Debug rendering: that
            // includes the task id, and a v7 UUID ending in the sentinel
            // digits failed this on CI at roughly 1-in-65k per run. The
            // sentinel is now prefixed for the same reason — four bare
            // digits are something a UUID can produce by chance, and a
            // test that fails on a coin flip teaches people to re-run
            // rather than to read.
            let delivered: String = task
                .messages
                .iter()
                .flat_map(|m| m.parts.iter())
                .filter_map(|p| match p {
                    mur_common::a2a::MessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let err = task.error.expect("Failed task must carry an error");
            assert_eq!(err.code, "llm_error");
            assert!(
                !err.recoverable,
                "a call dropped in the provider client reproduces on retry"
            );
            assert!(
                !delivered.contains("FABRICATED-2469"),
                "the model's fabricated tool output must never reach the user, got: {delivered}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
