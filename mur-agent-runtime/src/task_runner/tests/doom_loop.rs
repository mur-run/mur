use super::*;

/// A tool whose output CHANGES on every call even when the args are
/// identical — models e.g. `cargo build` returning new diagnostics after
/// each intervening edit. Used to prove the doom-loop guard keys on the
/// (tool, args, result) triple, not (tool, args) alone.
struct VaryingResultTool {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for VaryingResultTool {
    fn name(&self) -> &str {
        "build"
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: "build".into(),
            description: "test build tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        // Distinct content each call -> distinct result fingerprint.
        Ok(format!("build output #{n}").into())
    }
}

/// A tool whose output is CONSTANT on every call (same args -> same
/// result), modelling a genuinely stuck retry. Trips the doom-loop guard.
struct ConstantResultTool;

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for ConstantResultTool {
    fn name(&self) -> &str {
        "build"
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: "build".into(),
            description: "test build tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        Ok("identical build output".to_string().into())
    }
}

/// A tool whose output contains a credential — the shape a real `curl -v`
/// or `git remote -v` produces once a secret is in the environment.
struct LeakyTool;

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for LeakyTool {
    fn name(&self) -> &str {
        "build"
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: "build".into(),
            description: "test tool that echoes a credential".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        Ok(
            "remote: https://x:d8b04a3cc632a5c8026cf5a810d36e292c603f99@git.local"
                .to_string()
                .into(),
        )
    }
}

/// Records every request it is handed, so a test can assert on what the
/// MODEL actually received — the only place the masking guarantee is
/// observable end to end. Asserting on `masked()` alone would pass even if
/// neither execute site called it.
struct RecordingLlm {
    responses: Vec<crate::llm::LlmResponse>,
    index: std::sync::atomic::AtomicUsize,
    seen: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for RecordingLlm {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{:?}", req.messages));
        let idx = self
            .index
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.responses[idx % self.responses.len()].clone())
    }
    fn model_name(&self) -> &str {
        "recording-stub"
    }
}

/// Fix #1a — progress is NOT a loop: the SAME tool call (identical args)
/// every turn, but whose execution returns a DIFFERENT result each time,
/// must NOT trip the doom-loop guard. Under the old (tool, args)-only
/// fingerprint this aborts with "loop_detected"; under the (tool, args,
/// result) fingerprint it runs to the iteration cap instead.
#[tokio::test]
async fn varying_tool_results_are_not_a_doom_loop() {
    use crate::llm::stub::SequenceLlm;
    // Always emits the same call; the loop only ever stops on a budget.
    let responses: Vec<crate::llm::LlmResponse> = vec![
        build_tool_call_response("c-0"),
        build_tool_call_response("c-1"),
        build_tool_call_response("c-2"),
        build_tool_call_response("c-3"),
        build_tool_call_response("c-4"),
        build_tool_call_response("c-5"),
        end_turn_response("ITER SUMMARY: capped by iteration budget."),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(VaryingResultTool {
                calls: calls.clone(),
            })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "build".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            // Small cap so the test terminates fast; doom-loop must NOT
            // fire before the cap is reached.
            .with_iteration_ceiling(5),
    );
    let outcome = runner.run_sync(loop_spec("progress")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let usage = task.usage.expect("budget exit must populate usage");
    assert_ne!(
        usage["stop_reason"], "loop_detected",
        "changing results must NOT be a doom loop; usage={usage}"
    );
    assert_eq!(
        usage["stop_reason"], "iteration_ceiling",
        "expected the iteration ceiling to be the terminus; usage={usage}"
    );
}

/// Fix #1b — genuine stuck IS a loop: the SAME tool call AND identical
/// result each turn still aborts with stop_reason "loop_detected" within
/// ~3 iterations, well below the iteration cap.
#[tokio::test]
async fn an_approved_ask_tool_result_is_masked_too() {
    // The Allow arm and the Ask arm execute the tool at two separate call
    // sites. A test that only drives Allow passes with the Ask site
    // unmasked — which is the arm that matters most, since `Ask` is what a
    // credential-touching tool is set to. Mutation-checked: reverting
    // either site fails one of these two tests.
    let vault = Arc::new(crate::secrets::SecretVault::new());
    vault
        .set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
        .unwrap();
    let llm = Arc::new(RecordingLlm {
        responses: vec![build_tool_call_response("s-0"), end_turn_response("done")],
        index: std::sync::atomic::AtomicUsize::new(0),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let approvals = empty_pending_approvals();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(16);

    // Stand in for the human: approve whatever is asked.
    let approver = approvals.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if msg["method"] == "tool/approval_needed"
                && let Some(id) = msg["params"]["hitl_id"].as_str()
                && let Some(sender) = approver.lock().await.remove(id)
            {
                let _ = sender.send(crate::hitl::HitlDecision {
                    allow: true,
                    reason: None,
                    // The test stands in for a surface that did not name
                    // itself; recorded as unknown, never guessed.
                    surface: None,
                });
            }
        }
    });

    let runner = Arc::new(
        TaskRunner::with_llm(llm.clone())
            .with_secrets(vault)
            .with_tools(vec![Arc::new(LeakyTool)])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "build".into(),
                policy: mur_common::agent::ToolPolicy::Ask,
                risk: None,
            }])
            .with_pending_approvals(approvals)
            .with_sandbox_enforcing(true)
            .with_notifier(tx)
            .with_hitl_timeout_secs(5)
            .with_iteration_ceiling(5),
    );
    let _ = runner.run_sync(loop_spec("push it")).await;

    let seen = llm.seen.lock().unwrap().join("\n");
    assert!(
        seen.contains("[SECRET:GITEA_TOKEN]"),
        "the approved tool's output reached the model unmasked; got:\n{seen}"
    );
    assert!(
        !seen.contains("d8b04a3cc632a5c8026cf5a810d36e292c603f99"),
        "the raw credential reached the model; got:\n{seen}"
    );
}

#[tokio::test]
async fn the_model_never_receives_a_tool_result_containing_a_secret() {
    // End to end through `run_sync`: a tool emits the credential, and what
    // the MODEL is handed on the next call must carry the tag, not the
    // value. This is the assertion the whole design rests on — a unit test
    // of `masked()` would still pass if neither execute site called it.
    let vault = Arc::new(crate::secrets::SecretVault::new());
    vault
        .set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
        .unwrap();
    let llm = Arc::new(RecordingLlm {
        responses: vec![build_tool_call_response("s-0"), end_turn_response("done")],
        index: std::sync::atomic::AtomicUsize::new(0),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let runner = Arc::new(
        TaskRunner::with_llm(llm.clone())
            .with_secrets(vault)
            .with_tools(vec![Arc::new(LeakyTool)])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "build".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let _ = runner.run_sync(loop_spec("push it")).await;

    let seen = llm.seen.lock().unwrap().join("\n");
    assert!(
        seen.contains("[SECRET:GITEA_TOKEN]"),
        "the masked tag must be what the model saw; got:\n{seen}"
    );
    assert!(
        !seen.contains("d8b04a3cc632a5c8026cf5a810d36e292c603f99"),
        "the raw credential reached the model; got:\n{seen}"
    );
}

#[tokio::test]
async fn identical_tool_results_still_trip_doom_loop() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> = vec![
        build_tool_call_response("s-0"),
        build_tool_call_response("s-1"),
        build_tool_call_response("s-2"),
        end_turn_response("LOOP SUMMARY: stuck; identical output."),
    ];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(ConstantResultTool)])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "build".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("stuck")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (doom-loop graceful exit), got {outcome:?}");
    };
    let usage = task.usage.expect("doom-loop exit must populate usage");
    assert_eq!(usage["stop_reason"], "loop_detected", "usage={usage}");
    let iters = usage["iterations"]
        .as_u64()
        .expect("iterations is a number");
    assert!(iters < 5, "expected early abort, got {iters} iterations");
}

/// The production shape, which the guard could not catch: the command is
/// identical every time and only the model's narration changes. Captured
/// from a live agent on 2026-09-14 — it even numbered them "(1 of 6)".
/// Before the narration fields were dropped from the fingerprint, all six
/// `args` hashes were distinct, `repeats` never left 1, and the turn ran
/// to completion with nothing to show.
#[tokio::test]
async fn doom_loop_fires_when_only_the_description_varies() {
    use crate::llm::stub::SequenceLlm;
    let dir = tempfile::tempdir().unwrap();
    let call = |n: u32| crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: format!("d-{n}"),
            tool_name: "bash".into(),
            input: serde_json::json!({
                "command": "echo LOOPTEST",
                // Identical work, fresh narration — exactly what a model
                // produces, and what the guard used to hash.
                "description": format!("Running echo LOOPTEST ({n} of 6)"),
            }),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    };
    let responses = vec![
        call(0),
        call(1),
        call(2),
        call(3),
        call(4),
        call(5),
        end_turn_response("done"),
    ];
    let bash = crate::tools::bash::BashTool::new(
        dir.path().to_path_buf(),
        crate::tools::fs_policy::SessionCwd::new(dir.path().to_path_buf()),
    );
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(bash)])
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
    let outcome = runner.run_sync(loop_spec("narration")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let usage = task.usage.expect("usage");
    assert_eq!(
        usage["stop_reason"], "loop_detected",
        "narration must not hide a repeated action; usage={usage}"
    );
}

/// Negative control: dropping narration must not make DIFFERENT work look
/// the same. Same tool, same narration, genuinely different commands —
/// the guard must stay quiet and the turn must reach its own end.
#[tokio::test]
async fn different_commands_sharing_a_description_do_not_trip_the_guard() {
    use crate::llm::stub::SequenceLlm;
    let dir = tempfile::tempdir().unwrap();
    let call = |n: u32| crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: format!("v-{n}"),
            tool_name: "bash".into(),
            input: serde_json::json!({
                "command": format!("echo DIFFERENT-{n}"),
                "description": "Probing the tree",
            }),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    };
    let responses = vec![
        call(0),
        call(1),
        call(2),
        call(3),
        end_turn_response("all four ran"),
    ];
    let bash = crate::tools::bash::BashTool::new(
        dir.path().to_path_buf(),
        crate::tools::fs_policy::SessionCwd::new(dir.path().to_path_buf()),
    );
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(bash)])
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
    let outcome = runner.run_sync(loop_spec("varied")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let reply = task.messages.last().map(text_of).unwrap_or_default();
    assert!(reply.contains("all four ran"), "reply={reply}");
}

/// REPRO PROBE: the doom-loop guard passes with a stub tool but was
/// observed NOT firing in production against the REAL bash tool — six
/// identical `echo` calls, six separate job logs, clean completion
/// (2026-09-14, agent `rustsmith`). Same command, same output, same
/// arguments: the fingerprint should repeat and abort at the third.
#[tokio::test]
async fn doom_loop_fires_against_the_real_bash_tool() {
    use crate::llm::stub::SequenceLlm;
    let dir = tempfile::tempdir().unwrap();
    let call = |n: u32| crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: format!("c-{n}"),
            tool_name: "bash".into(),
            // IDENTICAL arguments every time.
            input: serde_json::json!({"command": "echo LOOPTEST"}),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    };
    let responses = vec![
        call(0),
        call(1),
        call(2),
        call(3),
        call(4),
        call(5),
        end_turn_response("done"),
    ];
    let bash = crate::tools::bash::BashTool::new(
        dir.path().to_path_buf(),
        crate::tools::fs_policy::SessionCwd::new(dir.path().to_path_buf()),
    );
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(bash)])
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
    let outcome = runner.run_sync(loop_spec("real-bash")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let usage = task.usage.expect("usage");
    assert_eq!(
        usage["stop_reason"], "loop_detected",
        "six identical bash calls must trip the guard; usage={usage}"
    );
}

/// Fix #2 — graceful_exit must sanitize a dangling tool_use before the
/// final summary turn. We build a `history` ending in a `ToolUse` whose
/// call has NO following `ToolResults` (mid-iteration abort), then run
/// `graceful_exit`. A recording LLM captures the request it receives; we
/// assert every tool_use call_id in that request has a matching
/// tool_result, so the Anthropic API would not 400 on it.
#[tokio::test]
async fn graceful_exit_sanitizes_dangling_tool_use() {
    use crate::llm::{RichMessage, ToolCallResult};

    /// Captures the messages of the request passed to it and replies with
    /// a benign end-turn summary.
    struct RecordingLlm {
        seen: Arc<Mutex<Vec<RichMessage>>>,
    }
    #[async_trait::async_trait]
    impl crate::llm::LlmClient for RecordingLlm {
        async fn generate(
            &self,
            req: crate::llm::LlmRequest,
        ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
            *self.seen.lock().unwrap() = req.messages.clone();
            Ok(end_turn_response("SUMMARY: done."))
        }
        fn model_name(&self) -> &str {
            "recording"
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let client: Arc<dyn crate::llm::LlmClient> = Arc::new(RecordingLlm { seen: seen.clone() });
    let runner = TaskRunner::with_llm(client.clone());

    // History ends with a tool_use that has no following tool_result.
    let history = vec![
        RichMessage::Text {
            role: "system".into(),
            content: "sys".into(),
        },
        RichMessage::Text {
            role: "user".into(),
            content: "do the thing".into(),
        },
        RichMessage::ToolUse {
            text: Some("calling build".into()),
            calls: vec![ToolCallResult {
                call_id: "dangling-1".into(),
                tool_name: "build".into(),
                input: serde_json::json!({}),
            }],
        },
    ];

    let msg = runner
        .graceful_exit(
            client.as_ref(),
            &history,
            LoopStop::LoopDetected,
            &crate::turn_ledger::TurnLedger::default(),
            2,
            &crate::bounds::Progress::start(std::time::Instant::now()),
        )
        .await;
    // Summary turn succeeded (not the fallback path).
    // The settlement follows the model's text now; this test is about the
    // dangling tool_use being sanitised, not the exact reply bytes.
    assert!(
        text_of(&msg).starts_with("SUMMARY: done."),
        "{}",
        text_of(&msg)
    );

    // Inspect what the LLM actually received: collect every tool_use id and
    // every tool_result id, then assert no tool_use id is unmatched.
    let messages = seen.lock().unwrap().clone();
    let mut use_ids: Vec<String> = Vec::new();
    let mut result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in &messages {
        match m {
            RichMessage::ToolUse { calls, .. } => {
                for c in calls {
                    use_ids.push(c.call_id.clone());
                }
            }
            RichMessage::ToolResults { results } => {
                for r in results {
                    result_ids.insert(r.call_id.clone());
                }
            }
            RichMessage::Text { .. }
            | RichMessage::ImageText { .. }
            | RichMessage::TurnLedger { .. } => {}
        }
    }
    assert!(!use_ids.is_empty(), "expected at least one tool_use");
    for id in &use_ids {
        assert!(
            result_ids.contains(id),
            "tool_use id {id} has no matching tool_result; request would 400. \
             result_ids={result_ids:?}"
        );
    }
}

/// #595 — graceful_exit must append the iteration-cap marker to the
/// output text ONLY when the stop reason is `MaxIterations`, so partial
/// execution at the cap is visible instead of looking like a clean
/// completion.
///
/// The premise widened when the notice moved into the settlement: it used
/// to be an iteration-cap-only string appended after whatever the model
/// had just claimed, so a budget stop looked clean. Every non-clean stop
/// now names itself, and each names the RIGHT one — a card that said
/// "iteration cap" for a token-budget stop would be worse than silence.
#[tokio::test]
async fn graceful_exit_names_the_stop_reason_in_the_settlement() {
    use crate::llm::stub::SequenceLlm;
    let client: Arc<dyn crate::llm::LlmClient> = Arc::new(SequenceLlm::new(vec![
        end_turn_response("partial work done"),
        end_turn_response("partial work done"),
    ]));
    let runner = TaskRunner::with_llm(client.clone());
    let history = vec![crate::llm::RichMessage::Text {
        role: "user".into(),
        content: "do the thing".into(),
    }];

    let capped = runner
        .graceful_exit(
            client.as_ref(),
            &history,
            LoopStop::IterationCeiling,
            &crate::turn_ledger::TurnLedger::default(),
            3,
            &crate::bounds::Progress::start(std::time::Instant::now()),
        )
        .await;
    let capped_text = text_of(&capped);
    assert!(
        capped_text.contains("iteration ceiling")
            && capped_text.contains("output may be incomplete"),
        "IterationCeiling exit must name the ceiling: {capped_text}"
    );

    let other = runner
        .graceful_exit(
            client.as_ref(),
            &history,
            LoopStop::LoopDetected,
            &crate::turn_ledger::TurnLedger::default(),
            3,
            &crate::bounds::Progress::start(std::time::Instant::now()),
        )
        .await;
    let other_text = text_of(&other);
    assert!(
        !other_text.contains("iteration cap"),
        "LoopDetected must not be reported as an iteration cap: {other_text}"
    );
    assert!(
        other_text.contains("loop detected"),
        "LoopDetected must name its own reason: {other_text}"
    );
}
