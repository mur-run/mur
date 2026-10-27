//! #2175: the `pre_tool_use` gate hooks run on the guarded tool-call path —
//! the one both the agentic loop and MCP `tools/call` go through.

use super::*;
use crate::hooks::{B0SafetyHook, Decision, HookChain, HookCtx, HookError};
use mur_common::agent::SpawnMode;

fn bash_allowed(calls: Arc<AtomicU64>) -> TaskRunner {
    TaskRunner::new_stub_echo()
        .with_sandbox_enforcing(true)
        .with_tools(vec![Arc::new(CountingBashTool {
            calls,
            ..Default::default()
        })])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: "bash".into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }])
}

fn ctx_with_spawn(mode: SpawnMode) -> HookCtx {
    let mut ctx = HookCtx::for_test_with_turn_flags(Vec::new());
    ctx.entitlements.processes.spawn.mode = mode;
    ctx
}

fn bash(cmd: &str) -> crate::llm::ToolCallResult {
    crate::llm::ToolCallResult {
        call_id: "c-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({ "command": cmd }),
    }
}

async fn run_one(
    r: &TaskRunner,
    call: &crate::llm::ToolCallResult,
) -> Result<crate::llm::ToolResultEntry, mur_common::a2a::TaskError> {
    let (mut d, mut s) = r.gate_response("t", std::slice::from_ref(call)).await;
    r.handle_tool_call("t", call, d.remove(&call.call_id), s.remove(&call.call_id))
        .await
}

#[tokio::test]
async fn b0_denies_bash_when_spawn_mode_is_none() {
    let calls = Arc::new(AtomicU64::new(0));
    let r = bash_allowed(calls.clone()).with_hook_chain(
        Arc::new(HookChain::new(vec![Arc::new(B0SafetyHook::new())])),
        ctx_with_spawn(SpawnMode::None),
        CancellationToken::new(),
    );
    let err = run_one(&r, &bash("echo hi"))
        .await
        .expect_err("a hook deny must stop the call");
    assert_eq!(err.code, "hook_deny");
    assert!(err.message.contains("mode=none"), "{}", err.message);
    assert_eq!(calls.load(Ordering::Relaxed), 0, "bash must not run");
}

/// No hook chain wired = no rules, not "refuse everything".
#[tokio::test]
async fn no_hook_chain_leaves_an_allow_call_alone() {
    let calls = Arc::new(AtomicU64::new(0));
    let r = bash_allowed(calls.clone());
    run_one(&r, &bash("echo hi")).await.expect("runs");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

type GateFn = fn() -> Result<Decision, HookError>;

/// Returns one fixed result from `pre_tool_use`.
struct FixedGate(GateFn);
#[async_trait::async_trait]
impl crate::hooks::Hook for FixedGate {
    fn name(&self) -> &str {
        "FixedGate"
    }
    async fn pre_tool_use(
        &self,
        _ctx: &HookCtx,
        _call: &crate::hooks::ToolCall,
        _tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        (self.0)()
    }
}

fn with_gate(r: TaskRunner, f: GateFn) -> TaskRunner {
    r.with_hook_chain(
        Arc::new(HookChain::new(vec![Arc::new(FixedGate(f))])),
        HookCtx::for_test_with_turn_flags(Vec::new()),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn a_rewrite_and_a_hook_failure_refuse_with_their_own_codes() {
    let cases: [(GateFn, &str); 2] = [
        (
            || Ok(Decision::Rewrite(serde_json::json!({}))),
            "hook_rewrite_unsupported",
        ),
        (|| Err(HookError::Runtime("boom".into())), "hook_error"),
    ];
    for (f, code) in cases {
        let calls = Arc::new(AtomicU64::new(0));
        let r = with_gate(bash_allowed(calls.clone()), f);
        let err = run_one(&r, &bash("echo hi")).await.expect_err(code);
        assert_eq!(err.code, code);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}

/// An `AskUser` lifts an `Allow`-policy call into the approval gate: with
/// nobody able to approve, it is held for a human, never run.
#[tokio::test]
async fn an_ask_lifts_an_allow_call_into_the_approval_gate() {
    let calls = Arc::new(AtomicU64::new(0));
    let r = with_gate(bash_allowed(calls.clone()), || ask(false));
    r.mark_unattended("t").await;
    let err = run_one(&r, &bash("echo hi")).await.expect_err("held");
    assert_eq!(err.code, crate::task_runner::HITL_APPROVAL_REQUIRED);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

/// Remembers an allow for every call and counts what is written back.
struct AllowsEverything(Arc<AtomicU64>);
#[async_trait::async_trait]
impl crate::hitl::store::DecisionStore for AllowsEverything {
    async fn lookup(&self, _h: &str) -> Option<crate::hitl::store::Settled> {
        Some(crate::hitl::store::Settled::Allow)
    }
    async fn record(&self, _r: mur_common::hitl::HitlResponse) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn ask(one_shot: bool) -> Result<Decision, HookError> {
    Ok(Decision::AskUser {
        prompt: "ok?".into(),
        default: crate::hooks::AskDefault::Deny,
        scope_key: mur_common::permissions::ScopeKey {
            agent_id: "a".into(),
            tool_name: "bash".into(),
            input_schema_hash: String::new(),
        },
        one_shot,
    })
}

/// A gate behind a store that remembers "allow bash", answered by a human
/// who says deny. Returns (ran, records written).
async fn behind_a_remembered_allow(f: GateFn) -> (u64, u64) {
    let calls = Arc::new(AtomicU64::new(0));
    let records = Arc::new(AtomicU64::new(0));
    let pa = empty_pending_approvals();
    let (ntx, mut nrx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
    let r = with_gate(bash_allowed(calls.clone()), f)
        .with_decision_store(Arc::new(AllowsEverything(records.clone())))
        .with_pending_approvals(pa.clone())
        .with_notifier(ntx)
        .with_hitl_timeout_secs(5);
    let human = tokio::spawn(async move {
        let n = nrx.recv().await?;
        let id = n["params"]["hitl_id"].as_str()?.to_string();
        let tx = pa.lock().await.remove(&id)?;
        let _ = tx.send(crate::hitl::HitlDecision {
            allow: false,
            reason: None,
            surface: Some("cli".into()),
            awaiting_human: false,
            refusal_code: None,
        });
        Some(())
    });
    let _ = run_one(&r, &bash("echo hi")).await;
    drop(r);
    let _ = human.await;
    (
        calls.load(Ordering::Relaxed),
        records.load(Ordering::Relaxed),
    )
}

/// Rule 4's question is about this turn's input, so a standing "allow
/// bash" must not answer it, and the answer must not become one.
#[tokio::test]
async fn a_one_shot_ask_skips_the_decision_store() {
    let (ran, records) = behind_a_remembered_allow(|| ask(true)).await;
    assert_eq!(
        ran, 0,
        "the human's deny must win over the remembered allow"
    );
    assert_eq!(records, 0, "a one-shot answer must not be recorded");
}

/// Rule 1/2's question is a reusable permission: the store answers it.
#[tokio::test]
async fn a_remembering_ask_is_answered_by_the_decision_store() {
    let (ran, _) = behind_a_remembered_allow(|| ask(false)).await;
    assert_eq!(ran, 1, "the remembered allow answers without asking");
}
