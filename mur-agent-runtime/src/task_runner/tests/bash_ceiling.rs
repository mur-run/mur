//! #1814: the bash tier ceiling ENFORCES. A `bash: allow` call above the
//! ceiling stops for an approval, the prompt carries the command's tier, and
//! one telemetry record (`enforced: true`) names agent, command, tier and
//! ceiling — what `mur agent doctor` counts.

use super::*;
use crate::telemetry_writer::Event;
use mur_common::hitl::RiskTier;
use mur_common::hitl::floor_waiver::FloorWaiver;

struct CountingBash {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for CountingBash {
    fn name(&self) -> &str {
        "bash"
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: "bash".into(),
            description: "stub bash".into(),
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

fn bash_call(cmd: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![crate::llm::ToolCallResult {
            call_id: "b-0".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({ "command": cmd }),
        }],
        stop_reason: crate::llm::StopReason::ToolUse,
        registry_key: None,
    }
}

struct Run {
    executed: u64,
    /// `risk` on each asked call, in order (`None` = field absent).
    asked: Vec<Option<String>>,
    records: Vec<serde_json::Value>,
    /// `code: message` of a failed task, empty otherwise.
    error: String,
}

/// Run one `bash` call under an exact `bash: allow` rule carrying `waiver`.
/// Every approval is DENIED, so `executed` is 1 only for a call that never
/// asked.
async fn run_one(cmd: &str, waiver: FloorWaiver) -> Run {
    run_with(cmd, waiver, None, true).await
}

/// A decision store that already holds a settled answer for every call, as a
/// `git push` approved earlier in the TTL would.
struct Remembered(crate::hitl::store::Settled);

#[async_trait::async_trait]
impl crate::hitl::store::DecisionStore for Remembered {
    async fn lookup(&self, _h: &str) -> Option<crate::hitl::store::Settled> {
        Some(self.0)
    }
    async fn record(&self, _r: mur_common::hitl::HitlResponse) {}
}

async fn run_with(
    cmd: &str,
    waiver: FloorWaiver,
    store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    sandbox_enforcing: bool,
) -> Run {
    run_wired(cmd, waiver, store, sandbox_enforcing, Wiring::default()).await
}

/// The two ways a call can be refused without a human ever being asked,
/// besides the sandbox not enforcing.
#[derive(Default)]
struct Wiring {
    /// The turn's sink says the caller cannot approve (a one-shot CLI).
    caller_cannot_approve: bool,
    /// No notifier is wired, so a prompt has nowhere to go. (Dropping the
    /// pending-approvals table instead would skip the agentic loop entirely.)
    no_approval_channel: bool,
}

async fn run_wired(
    cmd: &str,
    waiver: FloorWaiver,
    store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    sandbox_enforcing: bool,
    wiring: Wiring,
) -> Run {
    use crate::llm::stub::SequenceLlm;
    let calls = Arc::new(AtomicU64::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(64);
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let pa = empty_pending_approvals();
    let mut runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
        bash_call(cmd),
        end_turn_response("done"),
    ])))
    .with_tools(vec![Arc::new(CountingBash {
        calls: calls.clone(),
    })])
    .with_tools_policy(vec![mur_common::agent::ToolRule {
        pattern: "bash".into(),
        policy: mur_common::agent::ToolPolicy::Allow,
        risk: None,
        bypass_builtin_tier_floor: waiver,
    }])
    .with_telemetry(tx)
    .with_pending_approvals(pa.clone())
    .with_sandbox_enforcing(sandbox_enforcing)
    .with_hitl_timeout_secs(5)
    .with_iteration_ceiling(5);
    if !wiring.no_approval_channel {
        runner = runner.with_notifier(ntx);
    }
    if let Some(s) = store {
        runner = runner.with_decision_store(s);
    }
    let mut spec = loop_spec("bash-ceiling");
    if wiring.caller_cannot_approve {
        let (stx, _srx) = tokio::sync::mpsc::channel(16);
        runner
            .register_client_notifier("t-bash-ceiling", stx, false)
            .await;
        spec.task_id = Some("t-bash-ceiling".to_string());
    }
    let runner = Arc::new(runner);
    let denier = tokio::spawn(async move {
        loop {
            let senders: Vec<_> = {
                let mut g = pa.lock().await;
                let keys: Vec<String> = g.keys().cloned().collect();
                keys.into_iter().filter_map(|k| g.remove(&k)).collect()
            };
            for s in senders {
                let _ = s.send(crate::hitl::HitlDecision {
                    allow: false,
                    reason: Some("test denies".into()),
                    surface: None,
                    awaiting_human: false,
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    let outcome = runner.run_sync(spec).await;
    let error = match &outcome {
        TaskOutcome::Failed(t) => t
            .error
            .as_ref()
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_default(),
        _ => String::new(),
    };
    denier.abort();
    drop(runner);
    let mut asked = Vec::new();
    while let Ok(n) = nrx.try_recv() {
        if n["method"] == "tool/approval_needed" {
            for c in n["params"]["calls"].as_array().into_iter().flatten() {
                asked.push(c.get("risk").and_then(|r| r.as_str()).map(String::from));
            }
        }
    }
    let mut records = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let Event::HookFired { method, attrs } = ev
            && method == mur_common::telemetry::METHOD_BASH_CEILING_EXCEEDED
        {
            records.push(attrs);
        }
    }
    Run {
        executed: calls.load(Ordering::Relaxed),
        asked,
        records,
        error,
    }
}

#[tokio::test]
async fn above_the_ceiling_asks_and_is_recorded_as_enforced() {
    let r = run_one("git push origin main", FloorWaiver::Off).await;
    assert_eq!(
        r.executed, 0,
        "a denied call above the ceiling must not run"
    );
    assert_eq!(
        r.asked,
        vec![Some("network-egress".to_string())],
        "the prompt must carry the command tier, not look like a plain write"
    );
    assert_eq!(r.records.len(), 1, "exactly one record per asked call");
    let h = &r.records[0];
    assert_eq!(h["tool"], "bash");
    assert_eq!(h["command"], "git push origin main");
    assert_eq!(h["tier"], "network-egress");
    assert_eq!(h["ceiling"], "write");
    assert_eq!(h["rule"], "bash");
    assert_eq!(h["enforced"], true);
    assert_eq!(
        h[mur_common::telemetry::MUR_EVENT_TYPE],
        mur_common::telemetry::METHOD_BASH_CEILING_EXCEEDED,
        "the jsonl line must be findable without the JSON-RPC method"
    );
}

#[tokio::test]
async fn at_or_under_the_ceiling_runs_unasked_and_unrecorded() {
    let r = run_one("ls -la", FloorWaiver::Off).await;
    assert_eq!(r.executed, 1);
    assert!(r.asked.is_empty(), "{:?}", r.asked);
    assert!(r.records.is_empty(), "{:?}", r.records);
}

/// BREAKING vs #1833: `true` on `bash` no longer waives anything.
#[tokio::test]
async fn true_on_bash_is_ignored_end_to_end() {
    let r = run_one("sudo launchctl list", FloorWaiver::All).await;
    assert_eq!(r.executed, 0, "`true` must not let sudo through");
    assert_eq!(r.asked, vec![Some("privileged".to_string())]);
    assert_eq!(r.records.len(), 1);
}

/// The highest waiver lifts destructive, never privileged.
#[tokio::test]
async fn a_named_ceiling_runs_up_to_it_and_privileged_still_asks() {
    let max = FloorWaiver::UpTo(RiskTier::Destructive);
    let rm = run_one("rm -rf target", max).await;
    assert_eq!(rm.executed, 1, "destructive is under the named ceiling");
    assert!(rm.asked.is_empty() && rm.records.is_empty());

    let chmod = run_one("chmod 777 target", max).await;
    assert_eq!(chmod.executed, 0);
    assert_eq!(chmod.asked, vec![Some("privileged".to_string())]);
    assert_eq!(chmod.records[0]["ceiling"], "destructive");
}

/// A remembered decision answers without asking anyone, so it is not a stop:
/// counting it would have doctor report a `git push` approved yesterday as
/// asked again on every run.
#[tokio::test]
async fn a_remembered_decision_is_not_recorded() {
    use crate::hitl::store::Settled;
    for (settled, ran) in [(Settled::Allow, 1), (Settled::Deny, 0)] {
        let r = run_with(
            "git push origin main",
            FloorWaiver::Off,
            Some(Arc::new(Remembered(settled))),
            true,
        )
        .await;
        assert_eq!(r.executed, ran, "{settled:?}");
        assert!(r.asked.is_empty(), "{settled:?} asked: {:?}", r.asked);
        assert!(
            r.records.is_empty(),
            "{settled:?} recorded: {:?}",
            r.records
        );
    }
}

/// A refusal is a stop too: with the sandbox not enforcing, the gate refuses
/// without asking, and doctor must still see the ceiling stopped the call.
#[tokio::test]
async fn a_refusal_without_asking_is_recorded() {
    let r = run_with("git push origin main", FloorWaiver::Off, None, false).await;
    assert_eq!(r.executed, 0);
    assert!(r.asked.is_empty(), "{:?}", r.asked);
    assert_eq!(r.records.len(), 1, "{:?}", r.records);
    assert_eq!(r.records[0]["tier"], "network-egress");
}

/// Refused because the caller cannot approve (`decide_without_asking`): no
/// prompt, but still a stop doctor must count.
#[tokio::test]
async fn a_refusal_because_the_caller_cannot_approve_is_recorded() {
    let r = run_wired(
        "git push origin main",
        FloorWaiver::Off,
        None,
        true,
        Wiring {
            caller_cannot_approve: true,
            ..Wiring::default()
        },
    )
    .await;
    assert_eq!(r.executed, 0);
    assert!(r.asked.is_empty(), "{:?}", r.asked);
    assert_eq!(r.records.len(), 1, "{:?}", r.records);
    assert_eq!(r.records[0]["tier"], "network-egress");
}

/// #1997: the same refusal must say the ceiling is why, not `tool-allow` —
/// the `bash: allow` rule is already there.
#[tokio::test]
async fn a_ceiling_refusal_names_the_tier_not_tool_allow() {
    let r = run_wired(
        "git push origin main",
        FloorWaiver::Off,
        None,
        true,
        Wiring {
            caller_cannot_approve: true,
            ..Wiring::default()
        },
    )
    .await;
    // #2085: nobody decided yet, so "needs a human", not "denied".
    assert!(r.error.starts_with("hitl_approval_required"), "{}", r.error);
    assert!(r.error.contains("tier `network-egress`"), "{}", r.error);
    assert!(r.error.contains("bash ceiling `write`"), "{}", r.error);
    assert!(!r.error.contains("tool-allow"), "{}", r.error);
}

/// Refused fail-closed because no approval channel exists: the call never
/// reached a human, and doctor must still count it.
#[tokio::test]
async fn a_refusal_with_no_approval_channel_is_recorded() {
    let r = run_wired(
        "git push origin main",
        FloorWaiver::Off,
        None,
        true,
        Wiring {
            no_approval_channel: true,
            ..Wiring::default()
        },
    )
    .await;
    assert_eq!(r.executed, 0);
    assert!(r.asked.is_empty(), "{:?}", r.asked);
    assert_eq!(r.records.len(), 1, "{:?}", r.records);
    assert_eq!(r.records[0]["tier"], "network-egress");
}
