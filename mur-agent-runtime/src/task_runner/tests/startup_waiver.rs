//! `--no-hitl` end to end at the runner seam: which asks a startup waiver
//! settles, which still reach a human, and that nothing it settles is
//! remembered past this boot.

use super::*;
use crate::hitl::store::{DecisionStore, Settled};
use mur_common::agent::{OverrideFlags, OverrideRecord, ToolPolicy, ToolRule};
use mur_common::hitl::RiskTier;
use mur_common::hitl::floor_waiver::FloorWaiver;
use mur_common::hitl::startup_waiver::StartupWaiver;
use mur_common::startup_override::ConfirmedBy;

const NONCE: &str = "nonce-1";

fn waiver() -> StartupWaiver {
    StartupWaiver::from_record(&OverrideRecord {
        nonce: NONCE.into(),
        flags: OverrideFlags {
            no_hitl: true,
            ..OverrideFlags::default()
        },
        confirmed_by: ConfirmedBy::Tty,
    })
    .expect("no_hitl builds a waiver")
}

/// A store answering `settled` for every hash and counting writes.
struct Store {
    settled: Option<Settled>,
    records: AtomicU64,
}

#[async_trait::async_trait]
impl DecisionStore for Store {
    async fn lookup(&self, _h: &str) -> Option<Settled> {
        self.settled
    }
    async fn record(&self, _r: mur_common::hitl::HitlResponse) {
        self.records.fetch_add(1, Ordering::Relaxed);
    }
}

/// A counting tool under any wire name.
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
            description: "stub".into(),
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

fn rule(pattern: &str, policy: ToolPolicy, risk: Option<RiskTier>) -> ToolRule {
    ToolRule {
        pattern: pattern.into(),
        policy,
        risk,
        bypass_builtin_tier_floor: FloorWaiver::Off,
    }
}

fn calls_response(tool: &str, input: serde_json::Value, n: usize) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: (0..n)
            .map(|i| crate::llm::ToolCallResult {
                call_id: format!("c-{i}"),
                tool_name: tool.into(),
                input: input.clone(),
            })
            .collect(),
        stop_reason: crate::llm::StopReason::ToolUse,
        registry_key: None,
    }
}

struct Setup {
    tool: &'static str,
    input: serde_json::Value,
    n: usize,
    rules: Vec<ToolRule>,
    waiver: bool,
    sandbox: bool,
    settled: Option<Settled>,
    one_shot: bool,
    /// `false` = a `mur agent send` caller that cannot approve.
    can_approve: bool,
}

impl Setup {
    fn new(tool: &'static str, input: serde_json::Value) -> Self {
        Self {
            tool,
            input,
            n: 1,
            rules: vec![],
            waiver: true,
            sandbox: true,
            settled: None,
            one_shot: false,
            can_approve: true,
        }
    }
}

struct Run {
    executed: u64,
    /// Calls named in `tool/approval_needed` requests.
    asked: usize,
    records: u64,
}

async fn run(s: Setup) -> Run {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![
        calls_response(s.tool, s.input.clone(), s.n),
        end_turn_response("OK"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let store = Arc::new(Store {
        settled: s.settled,
        records: AtomicU64::new(0),
    });
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(256);
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
        .with_tools(vec![Arc::new(Named {
            name: s.tool,
            calls: calls.clone(),
        })])
        .with_tools_policy(s.rules)
        .with_pending_approvals(empty_pending_approvals())
        .with_sandbox_enforcing(s.sandbox)
        .with_notifier(ntx.clone())
        .with_decision_store(store.clone())
        .with_hitl_timeout_secs(1)
        .with_iteration_ceiling(3);
    if s.waiver {
        runner.attach_startup_waiver(waiver());
    }
    let task = "t-waiver";
    runner
        .register_client_notifier(task, ntx, s.can_approve)
        .await;
    if s.one_shot {
        runner.mark_one_shot_approvals(task).await;
    }
    let mut spec = loop_spec("waiver");
    spec.task_id = Some(task.into());
    let runner = Arc::new(runner);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), runner.run_sync(spec))
        .await
        .expect("turn finishes");
    let mut asked = 0;
    while let Ok(n) = nrx.try_recv() {
        if n["method"] == "tool/approval_needed" {
            asked += n["params"]["calls"]
                .as_array()
                .map_or(1, |a| a.len().max(1));
        }
    }
    Run {
        executed: calls.load(Ordering::Relaxed),
        asked,
        records: store.records.load(Ordering::Relaxed),
    }
}

fn bash(cmd: &str) -> serde_json::Value {
    serde_json::json!({ "command": cmd })
}

fn waived(r: &Run) {
    assert_eq!(r.executed, 1, "a waived call runs");
    assert_eq!(r.asked, 0, "nobody is asked");
    assert_eq!(
        r.records, 0,
        "a waiver writes nothing to the decision store"
    );
}

fn still_asks(r: &Run) {
    assert_eq!(r.executed, 0, "nobody answered, so it must not run");
}

/// No rule: a default ask at `Destructive` (bash's own classifier) is waived.
#[tokio::test]
async fn a_default_destructive_ask_is_waived() {
    waived(&run(Setup::new("bash", bash("rm -rf build"))).await);
}

/// A declared `risk: destructive` under `allow` is a clamp, and waivable.
#[tokio::test]
async fn destructive_tool_runs_without_a_hitl_request() {
    let mut s = Setup::new("mcp__db__drop", serde_json::json!({}));
    s.rules = vec![rule(
        "mcp__db__drop",
        ToolPolicy::Allow,
        Some(RiskTier::Destructive),
    )];
    waived(&run(s).await);
}

/// Same, for a caller that cannot approve: settled, never parked.
#[tokio::test]
async fn destructive_tool_runs_headless_without_parking() {
    let mut s = Setup::new("mcp__db__drop", serde_json::json!({}));
    s.rules = vec![rule(
        "mcp__db__drop",
        ToolPolicy::Allow,
        Some(RiskTier::Destructive),
    )];
    s.can_approve = false;
    waived(&run(s).await);
}

/// With no waiver attached, the same call behaves exactly as on `main`.
#[tokio::test]
async fn without_a_waiver_nothing_changes() {
    let mut s = Setup::new("bash", bash("rm -rf build"));
    s.waiver = false;
    still_asks(&run(s).await);
}

/// The Spend floor, not the declared tier, decides. No rule reaches the gate
/// as `None`; `allow` + `risk: destructive` is a clamp at a waivable tier that
/// only the floor lifts to `Spend`. (`allow` + `risk: write` never asks, on
/// `main` or here, so it is not a waiver case.)
#[tokio::test]
async fn spend_tools_always_ask() {
    for tool in ["fleet_run", "parallel_jobs", "delegate_to"] {
        for risk in [None, Some(RiskTier::Destructive), Some(RiskTier::Spend)] {
            let mut s = Setup::new(tool, serde_json::json!({}));
            if risk.is_some() {
                s.rules = vec![rule(tool, ToolPolicy::Allow, risk)];
            }
            let r = run(s).await;
            assert_eq!(r.executed, 0, "{tool} at {risk:?} must not run unasked");
        }
    }
}

#[tokio::test]
async fn twenty_fleet_runs_all_ask() {
    let mut s = Setup::new("fleet_run", serde_json::json!({"fleet": "deep-research"}));
    s.n = 20;
    let r = run(s).await;
    assert_eq!(r.executed, 0);
    assert_eq!(r.asked, 20, "every fleet_run reaches the human");
}

#[tokio::test]
async fn an_explicit_ask_is_kept() {
    let mut s = Setup::new("bash", bash("rm -rf build"));
    s.rules = vec![rule("bash", ToolPolicy::Ask, None)];
    still_asks(&run(s).await);
}

/// `sudo` under `bash: allow` is a clamp at `Privileged`: never waived.
#[tokio::test]
async fn a_privileged_command_still_asks() {
    let mut s = Setup::new("bash", bash("sudo ls"));
    s.rules = vec![rule("bash", ToolPolicy::Allow, None)];
    still_asks(&run(s).await);
}

/// An MCP tool with no `risk:` reaches the gate unclassified: not waived.
#[tokio::test]
async fn an_undeclared_mcp_tool_still_asks() {
    still_asks(&run(Setup::new("mcp__x__do", serde_json::json!({}))).await);
}

#[tokio::test]
async fn an_explicit_deny_is_denied() {
    let mut s = Setup::new("bash", bash("rm -rf build"));
    s.rules = vec![rule("bash", ToolPolicy::Deny, None)];
    let r = run(s).await;
    assert_eq!((r.executed, r.asked), (0, 0));
}

#[tokio::test]
async fn remembered_deny_survives_waiver() {
    for one_shot in [false, true] {
        let mut s = Setup::new("bash", bash("rm -rf build"));
        s.settled = Some(Settled::Deny);
        s.one_shot = one_shot;
        let r = run(s).await;
        assert_eq!((r.executed, r.asked), (0, 0), "one_shot={one_shot}");
    }
}

#[tokio::test]
async fn remembered_deny_survives_waiver_headless() {
    for one_shot in [false, true] {
        let mut s = Setup::new("bash", bash("rm -rf build"));
        s.settled = Some(Settled::Deny);
        s.one_shot = one_shot;
        s.can_approve = false;
        let r = run(s).await;
        assert_eq!(r.executed, 0, "one_shot={one_shot}");
    }
}

/// One-shot keeps its meaning for approvals: a remembered `Allow` is not
/// consulted, so an explicit `ask` reaches the human.
#[tokio::test]
async fn one_shot_ignores_remembered_allow() {
    let mut s = Setup::new("bash", bash("rm -rf build"));
    s.rules = vec![rule("bash", ToolPolicy::Ask, None)];
    s.settled = Some(Settled::Allow);
    s.one_shot = true;
    let r = run(s).await;
    assert_eq!(r.executed, 0);
    assert_eq!(r.asked, 1);
}

#[tokio::test]
async fn unsealed_boot_still_refuses() {
    let mut s = Setup::new("bash", bash("rm -rf build"));
    s.sandbox = false;
    let r = run(s).await;
    assert_eq!((r.executed, r.asked), (0, 0));
}
