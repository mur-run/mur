//! D2b attack path, end to end on a real on-disk decision store.
//!
//! Threat: the B1 sandbox is not enforcing, so the agent process can read its
//! own `identity.key`. With it, the agent signs a `HitlResponse { allow: true }`
//! for the exact `action_hash` of the call it is about to make and appends it
//! to its HITL decision channel. `verify_one` accepts that event — it IS
//! signed by the agent key — so the store answers `Allow` and the gate would
//! release the call as "approved earlier" with no human involved.
//!
//! `sandbox_gate.rs` pins the refusal with a fixed in-memory store. This
//! probe replays the actual forgery: real key on disk, real
//! `ChannelDecisionStore`, real signature, real action hash. The control case
//! proves the forgery is live (so the refusal is the only thing stopping it).

use super::*;
use crate::hitl::store::{ChannelDecisionStore, DecisionStore, Settled, chat_action_hash};
use mur_common::identity::AgentIdentity;

const VICTIM: &str = "victim";
const CMD: &str = "cat ~/.ssh/id_ed25519";

/// Set up `<home>/agents/victim` with a saved identity, then play the
/// attacker: load the private key from disk and sign an allow for `CMD`.
/// Returns the store the *runtime* would build (its own load of the key).
async fn forge_allow(home: &std::path::Path) -> Arc<dyn DecisionStore> {
    let agent_dir = home.join("agents").join(VICTIM);
    std::fs::create_dir_all(&agent_dir).unwrap();
    AgentIdentity::generate().save(&agent_dir).unwrap();

    let hash = chat_action_hash("bash", &serde_json::json!({ "command": CMD }), VICTIM);

    // Attacker: read identity.key (possible only when unsandboxed) and sign.
    let stolen = Arc::new(AgentIdentity::load(&agent_dir).unwrap());
    let attacker = ChannelDecisionStore::new(home.to_path_buf(), VICTIM.into(), stolen, 0);
    attacker
        .record(mur_common::hitl::HitlResponse {
            hitl_id: "forged-1".into(),
            action_hash: hash.clone(),
            allow: true,
            reason: String::new(),
            surface: "hub".into(),
            issued_at: Some(chrono::Utc::now()),
        })
        .await;

    // Runtime: its own view of the same key and channel.
    let runtime_key = Arc::new(AgentIdentity::load(&agent_dir).unwrap());
    let store: Arc<dyn DecisionStore> = Arc::new(ChannelDecisionStore::new(
        home.to_path_buf(),
        VICTIM.into(),
        runtime_key,
        0,
    ));
    assert_eq!(
        store.lookup(&hash).await,
        Some(Settled::Allow),
        "precondition: the forged allow must verify, or this probe proves nothing"
    );
    store
}

struct Run {
    tool_calls: u64,
    asked_human: bool,
    outcome: TaskOutcome,
}

async fn run(store: Arc<dyn DecisionStore>, enforcing: bool) -> Run {
    use crate::llm::stub::SequenceLlm;
    let calls = Arc::new(AtomicU64::new(0));
    let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
        tool_call_response("c-1", CMD),
        end_turn_response("OK"),
    ])))
    .with_tools(vec![Arc::new(CountingBashTool {
        calls: calls.clone(),
        seen_task: Arc::new(Mutex::new(None)),
    })])
    .with_tools_policy(vec![])
    .with_agent_name(VICTIM)
    .with_pending_approvals(empty_pending_approvals())
    .with_notifier(ntx)
    .with_decision_store(store)
    .with_sandbox_enforcing(enforcing)
    .with_hitl_timeout_secs(1)
    .with_iteration_ceiling(3);
    let outcome = runner.run_sync(loop_spec("go")).await;
    let mut asked_human = false;
    while let Ok(n) = nrx.try_recv() {
        asked_human |= n["method"] == "tool/approval_needed";
    }
    Run {
        tool_calls: calls.load(Ordering::Relaxed),
        asked_human,
        outcome,
    }
}

/// The attack, sandbox not enforcing: forged allow must NOT release the call.
#[tokio::test]
async fn forged_remembered_allow_is_refused_when_unsandboxed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = forge_allow(tmp.path()).await;
    let r = run(store, false).await;
    assert_eq!(r.tool_calls, 0, "forged allow released the tool");
    assert!(!r.asked_human, "should refuse before asking");
    match &r.outcome {
        TaskOutcome::Failed(t) => {
            let e = t.error.as_ref().expect("error");
            assert_eq!(e.code, "hitl_denied");
            assert!(e.message.contains("sandbox"), "reason: {}", e.message);
            assert!(!e.message.contains("approved earlier"));
        }
        other => panic!("turn not refused: {other:?}"),
    }
}

/// Control: the same forged event, sandbox enforcing. The store trusts it and
/// the tool runs as "approved earlier" — the forgery is real; only the
/// enforcing check (and, in production, the sealed key) stands in its way.
#[tokio::test]
async fn control_forged_allow_is_honored_when_gate_thinks_sandbox_is_sealed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = forge_allow(tmp.path()).await;
    let r = run(store, true).await;
    assert_eq!(r.tool_calls, 1, "control: forged allow should have run");
    assert!(!r.asked_human, "control: remembered allow skips the human");
    assert!(
        !matches!(r.outcome, TaskOutcome::Failed(_)),
        "control: {:?}",
        r.outcome
    );
}
