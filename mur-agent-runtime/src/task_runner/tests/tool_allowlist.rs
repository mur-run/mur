//! Channel consult C1: a `TaskSpec` tool allowlist binds both what the loop
//! offers and what it dispatches, and only the built-in loop may run one (R9).

use super::*;

/// Records the tool names offered on every request.
struct OfferLog {
    inner: crate::llm::stub::SequenceLlm,
    offered: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for OfferLog {
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
        "offer-log"
    }
}

/// A tool that only counts its runs, under any name.
struct Named {
    name: &'static str,
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for Named {
    fn name(&self) -> &str {
        self.name
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: self.name.into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok("ran".to_string().into())
    }
}

fn allow_all() -> Vec<mur_common::agent::ToolRule> {
    vec![mur_common::agent::ToolRule {
        pattern: "*".into(),
        policy: mur_common::agent::ToolPolicy::Allow,
        risk: None,
        bypass_builtin_tier_floor: Default::default(),
    }]
}

fn confined(text: &str) -> TaskSpec {
    TaskSpec {
        tool_allowlist: Some(vec!["read_file".into()]),
        ..loop_spec(text)
    }
}

/// The model is shown only `read_file`, then calls `bash` anyway: the call is
/// refused without running, and `read_file` still works.
#[tokio::test]
async fn allowlist_binds_both_offer_and_dispatch() {
    let offered = Arc::new(Mutex::new(Vec::new()));
    let bash = Arc::new(AtomicU64::new(0));
    let read = Arc::new(AtomicU64::new(0));
    let mut read_call = tool_call_response("r-0", "x");
    read_call.tool_calls[0].tool_name = "read_file".into();
    let responses = vec![
        tool_call_response("b-0", "cat ~/secret"),
        read_call,
        end_turn_response("done"),
    ];
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(OfferLog {
            inner: crate::llm::stub::SequenceLlm::new(responses),
            offered: offered.clone(),
        }))
        .with_tools(vec![
            Arc::new(Named {
                name: "bash",
                calls: bash.clone(),
            }),
            Arc::new(Named {
                name: "read_file",
                calls: read.clone(),
            }),
        ])
        .with_tools_policy(allow_all())
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_hitl_timeout_secs(1)
        .with_iteration_ceiling(6),
    );
    let outcome = runner.run_sync(confined("consult")).await;
    assert!(matches!(outcome, TaskOutcome::Completed(_)), "{outcome:?}");
    assert_eq!(bash.load(Ordering::Relaxed), 0, "bash must never run");
    assert_eq!(read.load(Ordering::Relaxed), 1, "read_file still runs");
    let offered = offered.lock().unwrap();
    assert!(
        offered
            .iter()
            .all(|names| names == &["read_file".to_string()]),
        "only read_file is ever offered: {offered:?}"
    );
}

/// No allowlist = every tool, as before.
#[tokio::test]
async fn no_allowlist_offers_every_tool() {
    let offered = Arc::new(Mutex::new(Vec::new()));
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(OfferLog {
            inner: crate::llm::stub::SequenceLlm::new(vec![end_turn_response("hi")]),
            offered: offered.clone(),
        }))
        .with_tools(vec![
            Arc::new(Named {
                name: "bash",
                calls: Arc::default(),
            }),
            Arc::new(Named {
                name: "read_file",
                calls: Arc::default(),
            }),
        ])
        .with_tools_policy(allow_all())
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0),
    );
    let _ = runner.run_sync(loop_spec("plain")).await;
    let first = &offered.lock().unwrap()[0];
    assert!(first.iter().any(|n| n == "bash") && first.iter().any(|n| n == "read_file"));
}

/// R9 "Scope reaches the runner by another path": a confined spec on a CLI
/// backend is refused by the runner itself, naming the backend.
#[tokio::test]
async fn cli_backend_refuses_a_confined_turn() {
    let runner = TaskRunner::with_cli_spawn(&mur_common::cli_backend::CLAUDE);
    let TaskOutcome::Failed(task) = runner.run_sync(confined("consult")).await else {
        panic!("a confined turn must not run on a CLI backend");
    };
    let err = task.error.expect("error");
    assert_eq!(err.code, "backend_cannot_confine");
    assert!(
        err.message.contains("claude"),
        "names the backend: {}",
        err.message
    );
}

/// Any non-loop backend refuses too, not just CLI spawn.
#[tokio::test]
async fn stub_backend_refuses_a_confined_turn() {
    let runner = TaskRunner::with_backend(RunnerBackend::StubEcho);
    assert!(matches!(
        runner.run_sync(confined("consult")).await,
        TaskOutcome::Failed(_)
    ));
    // Unconfined, the same backend still answers.
    assert!(matches!(
        runner.run_sync(loop_spec("plain")).await,
        TaskOutcome::Completed(_)
    ));
}
