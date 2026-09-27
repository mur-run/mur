//! Sandbox-not-enforcing gate (decisions D2 / D2b; sub-question settled as
//! "refuse outright").
//!
//! When the B1 sandbox is not enforcing, the agent can read its own
//! `identity.key` and sign an allow event that `verify_one` accepts. So no
//! allow is trustworthy on that boot: not one answered now (D1 + D2 reject it
//! at `tool/hitl_respond`), and not one remembered from before (D2b). Asking
//! anyway would put a question to the human whose "allow" is refused, then
//! leave the agent waiting out the HITL timeout. So an `Ask` tool is refused
//! before the gate asks — whatever the decision store holds.
//!
//! Seam: the public `TaskRunner` builder + `run_sync`, the same seam as
//! `remembered_decisions_are_not_asked_again`.

use super::*;
use crate::hitl::store::{DecisionStore, Settled};

/// A decision store that answers every lookup the same way.
struct Fixed(Option<Settled>);

#[async_trait::async_trait]
impl DecisionStore for Fixed {
    async fn lookup(&self, _h: &str) -> Option<Settled> {
        self.0
    }
    async fn record(&self, _r: mur_common::hitl::HitlResponse) {}
}

/// Agent the refusal cases run as; the refusal must name it in the command it
/// points at.
const AGENT: &str = "unconfined";

/// Run one turn in which the model calls an `Ask` tool, on a runner built by
/// `configure`, and list every way it failed to refuse outright for the
/// sandbox. Empty = refused before asking, tool not run, reason names it and
/// points at `mur agent perm list-paths`, whose header shows the mode the
/// seal ended in. "Restart the agent" alone is wrong advice: an unsealed agent
/// usually stays unsealed across restarts (no Landlock, `sandbox_init`
/// refused).
async fn unsandboxed_refusal_violations(
    case: &str,
    configure: impl FnOnce(TaskRunner) -> TaskRunner,
) -> Vec<String> {
    use crate::llm::stub::SequenceLlm;
    let responses = vec![
        tool_call_response("c-1", "cat identity.key"),
        end_turn_response("OK"),
    ];
    let calls = Arc::new(AtomicU64::new(0));
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let runner = configure(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_tools(vec![Arc::new(CountingBashTool {
                calls: calls.clone(),
                seen_task: Arc::new(Mutex::new(None)),
            })])
            .with_tools_policy(vec![])
            .with_agent_name(AGENT)
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(ntx)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(3),
    );

    let outcome = runner.run_sync(loop_spec("unconfined")).await;

    let mut violations = Vec::new();
    if calls.load(Ordering::Relaxed) != 0 {
        violations.push(format!("{case}: the tool RAN"));
    }
    while let Ok(n) = nrx.try_recv() {
        if n["method"] == "tool/approval_needed" {
            violations.push(format!("{case}: asked the human"));
        }
    }
    match &outcome {
        TaskOutcome::Failed(t) => {
            let e = t.error.as_ref();
            let code = e.map(|e| e.code.as_str()).unwrap_or("");
            let msg = e.map(|e| e.message.as_str()).unwrap_or("");
            if code != "hitl_denied" || !msg.contains("sandbox") {
                violations.push(format!(
                    "{case}: refused, but not because of the sandbox — {code}: {msg}"
                ));
            } else if !msg.contains(&format!("mur agent perm list-paths {AGENT}")) {
                violations.push(format!(
                    "{case}: refusal does not say how to find out why — {msg}"
                ));
            }
        }
        _ => violations.push(format!("{case}: not refused (turn completed)")),
    }
    violations
}

#[tokio::test]
async fn ask_tools_are_refused_without_asking_when_sandbox_is_not_enforcing() {
    let mut violations = Vec::new();
    // Remembered allow first: that is the D2b hole. The other two states pin
    // that the refusal does not depend on what the store holds.
    for remembered in [Some(Settled::Allow), None, Some(Settled::Deny)] {
        let case = format!("remembered={remembered:?}");
        violations.extend(
            unsandboxed_refusal_violations(&case, |r| {
                r.with_decision_store(Arc::new(Fixed(remembered)))
                    .with_sandbox_enforcing(false)
            })
            .await,
        );
    }
    assert!(
        violations.is_empty(),
        "sandbox not enforcing, yet:\n  {}",
        violations.join("\n  ")
    );
}

/// Fail closed: a runner nobody told about the sandbox must not trust an
/// allow. A forgotten `with_sandbox_enforcing(true)` then shows up as "every
/// approval is refused" — loud — instead of an unguarded gate — silent.
#[tokio::test]
async fn a_runner_not_told_the_sandbox_state_refuses_ask_tools() {
    let violations = unsandboxed_refusal_violations("default runner", |r| r).await;
    assert!(
        violations.is_empty(),
        "sandbox state never set, yet:\n  {}",
        violations.join("\n  ")
    );
}

/// The production wiring carries a sealed sandbox into the gate. `build_runner`
/// is the one `with_*` chain both tracks go through (`build_provider_runner`),
/// so an argument dropped here would refuse every approval on a sealed agent.
#[tokio::test]
async fn build_runner_carries_an_enforcing_sandbox_to_the_gate() {
    use crate::llm::stub::SequenceLlm;
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let runner = crate::supervisor_runner::build_runner(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
            tool_call_response("c-1", "echo hi"),
            end_turn_response("OK"),
        ]))),
        None,
        Arc::new(RuntimeSkills::build(vec![])),
        SkillsConfig::default(),
        Default::default(),
        None,
        None,
        None,
        Some(empty_pending_approvals()),
        Some(ntx),
        1,
        mur_common::hitl::Autonomy::default(),
        vec![Arc::new(CountingBashTool::default())],
        vec![],
        Default::default(),
        None,
        None,
        None,
        String::new(),
        None,
        None,
        None,
        None,
        None,
        true,
    );

    let _ = runner.run_sync(loop_spec("sealed")).await;

    let mut asked = false;
    while let Ok(n) = nrx.try_recv() {
        asked |= n["method"] == "tool/approval_needed";
    }
    assert!(asked, "a sealed agent's Ask tool must reach the human");
}
