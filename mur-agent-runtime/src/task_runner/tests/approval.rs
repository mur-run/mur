use super::*;

/// A counting stub registered under the `fleet_run` wire name, for the
/// default-Allow policy-gate tests below.
struct CountingFleetRunTool {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for CountingFleetRunTool {
    fn name(&self) -> &str {
        crate::tools::fleet_run::FLEET_RUN
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: crate::tools::fleet_run::FLEET_RUN.into(),
            description: "stub fleet_run".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok("fleet ran".to_string().into())
    }
}

fn fleet_run_call_response(call_id: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: call_id.into(),
            tool_name: crate::tools::fleet_run::FLEET_RUN.into(),
            input: serde_json::json!({"fleet": "deep-research"}),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    }
}

/// issue #3: fleet_run with NO explicit rule now defaults to `Ask` (the
/// `None => Allow` special case is gone). With an approval sink present but
/// no responder, the 1s HITL timeout auto-denies PRE-execution — the spy's
/// execute count MUST stay 0. This is the core issue #3 regression guard:
/// dispatch/spend tools never run before approval.
#[tokio::test]
async fn fleet_run_without_rule_defaults_to_ask_and_denies_before_exec() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> = vec![
        fleet_run_call_response("fr-0"),
        end_turn_response("SHOULD NOT REACH"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingFleetRunTool {
                calls: calls.clone(),
            })])
            .with_tools_policy(vec![]) // no rules => default Ask
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let _ = runner.run_sync(loop_spec("fleet-run-default-ask")).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "unapproved fleet_run must NOT execute (pre-exec deny)"
    );
}

/// issue #3: fail-closed. With NO approval sink wired at all
/// (`pending_approvals`/`notifier` absent), an `Ask` tool must be DENIED
/// pre-execution, never silently allowed. Spy execute count stays 0.
#[tokio::test]
async fn ask_tool_denies_when_no_approval_sink() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> =
        vec![fleet_run_call_response("fr-0"), end_turn_response("NOPE")];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingFleetRunTool {
                calls: calls.clone(),
            })])
            .with_tools_policy(vec![]) // default Ask
            // NB: no with_pending_approvals / no with_notifier => no sink
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let _ = runner.run_sync(loop_spec("fleet-run-no-sink")).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "with no approval sink, Ask must fail-closed (deny), never execute"
    );
}

/// issue #3: happy path — an explicit approval arriving on the pending
/// channel lets the tool execute exactly once. A background poller pulls
/// the sender out of `pending_approvals` and answers `allow: true`.
#[tokio::test]
async fn ask_tool_executes_after_approval() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> = vec![
        fleet_run_call_response("fr-0"),
        end_turn_response("REPORT DELIVERED"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let pa = empty_pending_approvals();
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingFleetRunTool {
                calls: calls.clone(),
            })])
            .with_tools_policy(vec![]) // default Ask
            .with_pending_approvals(pa.clone())
            .with_sandbox_enforcing(true)
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(5)
            .with_iteration_ceiling(5),
    );
    // Background approver: as soon as a pending approval appears, answer allow.
    let pa2 = pa.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..200 {
            let sender = {
                let mut guard = pa2.lock().await;
                guard.keys().next().cloned().and_then(|k| guard.remove(&k))
            };
            if let Some(tx) = sender {
                let _ = tx.send(crate::hitl::HitlDecision {
                    allow: true,
                    reason: None,
                    surface: None,
                });
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    let outcome = runner.run_sync(loop_spec("fleet-run-approved")).await;
    let _ = approver.await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "approved fleet_run must execute exactly once"
    );
    let reply_text = task.messages.last().map(text_of).unwrap_or_default();
    assert!(reply_text.contains("REPORT DELIVERED"), "{reply_text}");
}

/// P3 §3.1: two `Ask` calls in one response → ONE `tool/approval_needed`
/// carrying both, two pending oneshots, and both execute after two allows.
#[tokio::test]
async fn two_ask_calls_in_one_response_emit_one_notification() {
    use crate::llm::stub::SequenceLlm;
    let mut two = tool_call_response("c-1", "echo one");
    two.tool_calls.push(crate::llm::ToolCallResult {
        call_id: "c-2".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "echo two"}),
    });
    let responses = vec![two, end_turn_response("DONE")];
    let calls = Arc::new(AtomicU64::new(0));
    let pa = empty_pending_approvals();
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingBashTool {
                calls: calls.clone(),
                ..Default::default()
            })])
            .with_tools_policy(vec![])
            .with_pending_approvals(pa.clone())
            .with_sandbox_enforcing(true)
            .with_notifier(ntx)
            .with_hitl_timeout_secs(5)
            .with_iteration_ceiling(5),
    );
    let pa2 = pa.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..500 {
            let senders: Vec<_> = {
                let mut g = pa2.lock().await;
                let keys: Vec<String> = g.keys().cloned().collect();
                keys.into_iter().filter_map(|k| g.remove(&k)).collect()
            };
            for tx in senders {
                let _ = tx.send(crate::hitl::HitlDecision {
                    allow: true,
                    reason: None,
                    surface: None,
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    let outcome = runner.run_sync(loop_spec("batch")).await;
    approver.abort();
    assert!(matches!(outcome, TaskOutcome::Completed(_)), "{outcome:?}");
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    let mut approvals = 0;
    let mut batch_len = 0;
    while let Ok(n) = nrx.try_recv() {
        if n["method"] == "tool/approval_needed" {
            approvals += 1;
            batch_len = n["params"]["calls"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            assert_eq!(
                n["params"]["hitl_id"], n["params"]["calls"][0]["hitl_id"],
                "legacy fields = calls[0]"
            );
            assert_eq!(
                n["params"]["calls"][0]["action_hash"]
                    .as_str()
                    .map(str::len),
                Some(64)
            );
        }
    }
    assert_eq!(approvals, 1, "one notification for the whole response");
    assert_eq!(batch_len, 2);
}

/// P3 §3.2: a remembered allow executes without asking; a remembered deny
/// denies without asking; nothing is asked in either case.
#[tokio::test]
async fn remembered_decisions_are_not_asked_again() {
    use crate::hitl::store::{DecisionStore, Settled};
    struct Fixed(Settled);
    #[async_trait::async_trait]
    impl DecisionStore for Fixed {
        async fn lookup(&self, _h: &str) -> Option<Settled> {
            Some(self.0)
        }
        async fn record(&self, _r: mur_common::hitl::HitlResponse) {}
    }
    for (settled, expect_calls, expect_completed) in
        [(Settled::Allow, 1u64, true), (Settled::Deny, 0u64, false)]
    {
        use crate::llm::stub::SequenceLlm;
        let responses = vec![
            tool_call_response("c-1", "echo hi"),
            end_turn_response("OK"),
        ];
        let calls = Arc::new(AtomicU64::new(0));
        let seen_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(CountingBashTool {
                    calls: calls.clone(),
                    seen_task: seen_task.clone(),
                })])
                .with_tools_policy(vec![])
                .with_pending_approvals(empty_pending_approvals())
                .with_sandbox_enforcing(true)
                .with_notifier(ntx)
                .with_decision_store(Arc::new(Fixed(settled)))
                .with_hitl_timeout_secs(1)
                .with_iteration_ceiling(3),
        );
        let outcome = runner.run_sync(loop_spec("remembered")).await;
        assert_eq!(
            matches!(outcome, TaskOutcome::Completed(_)),
            expect_completed,
            "{settled:?}: {outcome:?}"
        );
        assert_eq!(calls.load(Ordering::Relaxed), expect_calls, "{settled:?}");
        if matches!(settled, Settled::Allow) {
            // D8: the owner scope reaches the Ask site (this policy is
            // the default, `Ask`, resolved by a remembered decision) —
            // the tool observed a task id, not `None`.
            assert!(
                seen_task
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some(),
                "the Ask execute site never scoped CURRENT_TASK_ID"
            );
        }
        while let Ok(n) = nrx.try_recv() {
            assert_ne!(
                n["method"], "tool/approval_needed",
                "{settled:?} must not ask"
            );
        }
    }
}

/// An EXPLICIT Deny rule on fleet_run still wins over the built-in
/// Allow default — the call is refused without executing.
#[tokio::test]
async fn fleet_run_explicit_deny_still_wins() {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> =
        vec![fleet_run_call_response("fr-0"), end_turn_response("OK")];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingFleetRunTool {
                calls: calls.clone(),
            })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: crate::tools::fleet_run::FLEET_RUN.into(),
                policy: mur_common::agent::ToolPolicy::Deny,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(5),
    );
    let _ = runner.run_sync(loop_spec("fleet-run-deny")).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "explicitly denied fleet_run must never execute"
    );
}

/// A stub LLM that records the tool names offered on every request and
/// otherwise answers from a fixed sequence.
struct OfferRecordingLlm {
    inner: crate::llm::stub::SequenceLlm,
    offered: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for OfferRecordingLlm {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        self.offered
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|d| d.name.clone()).collect());
        self.inner.generate(req).await
    }
    fn model_name(&self) -> &str {
        "recording"
    }
}

/// A fleet_run stand-in whose gate always says no.
struct RefusingFleetRunTool {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for RefusingFleetRunTool {
    fn name(&self) -> &str {
        crate::tools::fleet_run::FLEET_RUN
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: crate::tools::fleet_run::FLEET_RUN.into(),
            description: "refusing fleet_run".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(crate::tools::ToolError::NotAuthorized(
            mur_common::authz::not_authorized("fleet_run: test refusal"),
        ))
    }
}

/// §3.8: a refusal is told once and the tool leaves the list. The stub
/// asks for fleet_run on three consecutive turns; the tool runs once, the
/// second and third requests do not offer it.
#[tokio::test]
async fn a_refused_tool_is_offered_once_and_then_withdrawn() {
    use crate::llm::stub::SequenceLlm;
    let offered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let responses = vec![
        fleet_run_call_response("fr-0"),
        fleet_run_call_response("fr-1"),
        fleet_run_call_response("fr-2"),
        end_turn_response("gave up"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(OfferRecordingLlm {
            inner: SequenceLlm::new(responses),
            offered: offered.clone(),
        }))
        .with_tools(vec![Arc::new(RefusingFleetRunTool {
            calls: calls.clone(),
        })])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: crate::tools::fleet_run::FLEET_RUN.into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
        }])
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_hitl_timeout_secs(1)
        .with_iteration_ceiling(6),
    );
    let _ = runner.run_sync(loop_spec("refused")).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the refused tool ran exactly once"
    );
    let offered = offered.lock().unwrap();
    let fr = crate::tools::fleet_run::FLEET_RUN;
    assert!(
        offered[0].iter().any(|n| n == fr),
        "offered on the first request: {offered:?}"
    );
    assert!(
        offered.len() >= 2
            && offered[1..]
                .iter()
                .all(|names| !names.iter().any(|n| n == fr)),
        "withdrawn afterwards: {offered:?}"
    );
}

/// A sandbox-denying stand-in: returns the same `Denied { Action }` shape
/// `tools::bash` returns for a kernel EPERM on a path or a binary.
struct SandboxDenyingTool {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for SandboxDenyingTool {
    fn name(&self) -> &str {
        crate::tools::fleet_run::FLEET_RUN
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: crate::tools::fleet_run::FLEET_RUN.into(),
            description: "sandbox-denying".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(crate::tools::ToolOutput {
            text: "[sandbox] ./cmdtest: Operation not permitted".into(),
            status: crate::tools::ToolStatus::Denied {
                detail: "not in the spawn allowlist".into(),
                scope: crate::tools::DenialScope::Action,
            },
            images: Vec::new(),
        })
    }
}

/// B: a sandbox denial must NOT withdraw the tool. The kernel refused one
/// path; the tool works for everything else. Withdrawing it is what turned
/// a denied `./cmdtest` into a whole lost turn on 2026-09-13 — every later
/// `bash` call came back refused and the agent flailed until the doom-loop
/// detector stopped it 54 iterations in, with nothing written.
#[tokio::test]
async fn a_sandbox_denial_does_not_withdraw_the_tool() {
    use crate::llm::stub::SequenceLlm;
    let offered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let responses = vec![
        fleet_run_call_response("sd-0"),
        fleet_run_call_response("sd-1"),
        fleet_run_call_response("sd-2"),
        end_turn_response("done"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(OfferRecordingLlm {
            inner: SequenceLlm::new(responses),
            offered: offered.clone(),
        }))
        .with_tools(vec![Arc::new(SandboxDenyingTool {
            calls: calls.clone(),
        })])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: crate::tools::fleet_run::FLEET_RUN.into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
        }])
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_hitl_timeout_secs(1)
        .with_iteration_ceiling(6),
    );
    let _ = runner.run_sync(loop_spec("sandbox")).await;

    // "I actually reached it": the tool really executed every time, so the
    // assertion below is about withdrawal and not about a stub that was
    // never called.
    assert_eq!(
        calls.load(Ordering::Relaxed),
        3,
        "the tool must keep running after a sandbox denial"
    );
    let offered = offered.lock().unwrap();
    let fr = crate::tools::fleet_run::FLEET_RUN;
    // Every request that carried tools at all offered it. The trailing
    // tools-less request is `graceful_exit`'s summary turn (it passes
    // `tools: vec![]` on purpose), not a withdrawal — the doom-loop guard
    // fires here because this stub repeats one command with one identical
    // result, which is exactly what that guard is for.
    let with_tools: Vec<_> = offered.iter().filter(|n| !n.is_empty()).collect();
    assert_eq!(with_tools.len(), 3, "offered={offered:?}");
    assert!(
        with_tools.iter().all(|names| names.iter().any(|n| n == fr)),
        "the tool must stay on the list after an Action-scoped denial: {offered:?}"
    );
}

/// D: once a tool IS withdrawn (a real authorization refusal), calling it
/// again can only be refused again — so the turn settles instead of paying
/// for more. The doom-loop detector does not cover this: it fingerprints
/// (tool, ARGS, result), and a model that varies its arguments defeats it,
/// which is how a withdrawn `bash` still burned 54 iterations.
#[tokio::test]
async fn repeated_calls_to_a_withdrawn_tool_settle_the_turn() {
    use crate::llm::stub::SequenceLlm;
    // Varying arguments on purpose: identical ones are the doom-loop
    // detector's job (it fingerprints tool + ARGS + result). The real
    // agent cycled `pwd` / `true` / `echo hello` / `echo probe`, minting a
    // fresh fingerprint every time, and that is how it reached 54
    // iterations against a tool that could never run again.
    let varied = |n: u32| crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: format!("w-{n}"),
            tool_name: crate::tools::fleet_run::FLEET_RUN.into(),
            input: serde_json::json!({ "fleet": format!("probe-{n}") }),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
    };
    let responses = vec![
        varied(0),
        varied(1),
        varied(2),
        varied(3),
        varied(4),
        end_turn_response("summary after the withdrawal"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(RefusingFleetRunTool {
                calls: calls.clone(),
            })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: crate::tools::fleet_run::FLEET_RUN.into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(50),
    );
    let outcome = runner.run_sync(loop_spec("withdrawn")).await;
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed (graceful exit), got {outcome:?}");
    };
    let usage = task.usage.expect("a guard exit must populate usage");
    assert_eq!(usage["stop_reason"], "tool_withdrawn", "usage={usage}");
    // The refused tool ran once; everything after was a synthetic refusal.
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let iters = usage["iterations"]
        .as_u64()
        .expect("iterations is a number");
    assert!(iters < 6, "expected an early settle, got {iters}");
}

#[test]
fn only_a_gates_refusal_withdraws_a_tool() {
    use crate::llm::ToolResultEntry;
    use crate::tools::ToolStatus;
    let mk = |content: &str, status: ToolStatus| ToolResultEntry {
        call_id: "c".into(),
        content: content.into(),
        is_error: true,
        status,
        images: Vec::new(),
    };
    let denied = |scope| ToolStatus::Denied {
        detail: "d".into(),
        scope,
    };
    // Withdrawn: the TOOL is gone for the rest of the turn.
    assert!(withdraws(&mk(
        "not authorized: x",
        denied(crate::tools::DenialScope::Tool)
    )));
    assert!(withdraws(&mk(
        "Tool `bash` is denied by policy",
        denied(crate::tools::DenialScope::Tool)
    )));
    // NOT withdrawn: only this ACTION was refused. The regression this
    // guards is the whole point of `DenialScope` — a sandbox EPERM on one
    // path used to take `bash` away for the turn, and the agent then
    // flailed through `pwd`/`true`/`echo` until the doom-loop detector
    // stopped it 54 iterations later with nothing written (2026-09-13).
    assert!(!withdraws(&mk(
        "[sandbox] ./cmdtest is not in the spawn allowlist",
        denied(crate::tools::DenialScope::Action)
    )));
    // An unknown name was never in the request list; nothing to withdraw.
    // Structural now — this used to be decided by sniffing the content
    // string for "unknown tool".
    assert!(!withdraws(&mk(
        "unknown tool: made_up",
        denied(crate::tools::DenialScope::Action)
    )));
    assert!(!withdraws(&mk(
        "tool error: boom",
        ToolStatus::Failed { exit_code: -1 }
    )));
}

struct NoopNamedTool(&'static str);

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for NoopNamedTool {
    fn name(&self) -> &str {
        self.0
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: self.0.into(),
            description: "noop".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        Ok("ok".to_string().into())
    }
}

/// The inventory the preflight consults is the loop's own: a tool is
/// missing when it is not registered or its policy is Deny.
#[test]
fn missing_tools_reads_the_same_inventory_the_gate_reads() {
    let runner = TaskRunner::with_llm(Arc::new(crate::llm::stub::SequenceLlm::new(vec![])))
        .with_tools(vec![
            Arc::new(NoopNamedTool("write_file")),
            Arc::new(NoopNamedTool("bash")),
        ])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: "bash".into(),
            policy: mur_common::agent::ToolPolicy::Deny,
            risk: None,
        }]);
    assert_eq!(
        runner.missing_tools(&["write_file".into()]),
        Vec::<String>::new()
    );
    assert_eq!(
        runner.missing_tools(&["bash".into(), "edit_file".into(), "write_file".into()]),
        vec!["bash".to_string(), "edit_file".to_string()]
    );
}
