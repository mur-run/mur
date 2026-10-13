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
    }
}

struct Run {
    executed: u64,
    /// `risk` on each asked call, in order (`None` = field absent).
    asked: Vec<Option<String>>,
    records: Vec<serde_json::Value>,
}

/// Run one `bash` call under an exact `bash: allow` rule carrying `waiver`.
/// Every approval is DENIED, so `executed` is 1 only for a call that never
/// asked.
async fn run_one(cmd: &str, waiver: FloorWaiver) -> Run {
    use crate::llm::stub::SequenceLlm;
    let calls = Arc::new(AtomicU64::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(64);
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let pa = empty_pending_approvals();
    let runner = Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
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
        .with_sandbox_enforcing(true)
        .with_notifier(ntx)
        .with_hitl_timeout_secs(5)
        .with_iteration_ceiling(5),
    );
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
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    let _ = runner.run_sync(loop_spec("bash-ceiling")).await;
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
