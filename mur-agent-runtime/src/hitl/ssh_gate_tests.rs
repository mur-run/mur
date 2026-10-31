//! `SshSignGate` against an in-memory store, a real approvals map, and a
//! notification channel the test answers from.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mur_common::hitl::HitlResponse;
use tokio::sync::mpsc;

use super::*;
use crate::hitl::store::{DecisionStore, Settled};
use crate::hitl::{HitlApprovals, HitlDecision};
use crate::ssh_proxy::gate::{GateOutcome, SignAsk, SignGate};

const AGENT: &str = "coder";
const WAIT: Duration = Duration::from_millis(300);

#[derive(Default)]
struct MemStore(Mutex<HashMap<String, HitlResponse>>);

impl MemStore {
    fn forget(&self) {
        self.0.lock().unwrap().clear();
    }
    fn recorded(&self) -> Vec<HitlResponse> {
        self.0.lock().unwrap().values().cloned().collect()
    }
}

#[async_trait::async_trait]
impl DecisionStore for MemStore {
    async fn lookup(&self, h: &str) -> Option<Settled> {
        self.0.lock().unwrap().get(h).map(|r| {
            if r.allow {
                Settled::Allow
            } else {
                Settled::Deny
            }
        })
    }
    async fn record(&self, r: HitlResponse) {
        self.0.lock().unwrap().insert(r.action_hash.clone(), r);
    }
}

struct T {
    gate: Arc<SshSignGate>,
    store: Arc<MemStore>,
    approvals: HitlApprovals,
    rx: mpsc::Receiver<serde_json::Value>,
}

fn rig(audit_only: bool) -> T {
    rig_with_capacity(audit_only, 8)
}

/// `capacity` bounds the notification channel, so a test can fill it and
/// hold every prompt inside `notifier.send`.
fn rig_with_capacity(audit_only: bool, capacity: usize) -> T {
    let store = Arc::new(MemStore::default());
    let approvals: HitlApprovals = Default::default();
    let (tx, rx) = mpsc::channel(capacity);
    let gate = Arc::new(SshSignGate {
        agent: AGENT.into(),
        store: store.clone(),
        approvals: approvals.clone(),
        notifier: tx,
        parking: Default::default(),
        wait: WAIT,
        audit_only,
        pending: Default::default(),
    });
    T {
        gate,
        store,
        approvals,
        rx,
    }
}

fn ask() -> SignAsk {
    SignAsk {
        key_fp: "SHA256:key".into(),
        key_label: Some("deploy".into()),
        host_key_fp: "SHA256:host".into(),
        host_label: Some("github.com".into()),
    }
}

async fn answer(approvals: &HitlApprovals, hitl_id: &str, allow: bool) {
    let tx = approvals
        .lock()
        .await
        .remove(hitl_id)
        .expect("a pending approval for this hitl_id");
    tx.send(HitlDecision {
        allow,
        reason: None,
        surface: Some("test".into()),
        awaiting_human: false,
        refusal_code: None,
    })
    .unwrap();
}

fn call_of(n: &serde_json::Value) -> serde_json::Value {
    n["params"]["calls"][0].clone()
}

/// Decide in the background, answer the one prompt, return the outcome.
async fn decide_answering(t: &mut T, allow: bool) -> (GateOutcome, serde_json::Value) {
    let g = t.gate.clone();
    let h = tokio::spawn(async move { g.decide(&ask()).await });
    let n = t.rx.recv().await.expect("a prompt");
    let id = call_of(&n)["hitl_id"].as_str().unwrap().to_string();
    answer(&t.approvals, &id, allow).await;
    (h.await.unwrap(), n)
}

#[tokio::test]
async fn approve_then_remembered_without_a_second_prompt() {
    let mut t = rig(false);
    let (out, _) = decide_answering(&mut t, true).await;
    assert_eq!(out, GateOutcome::Allow);
    let rec = t.store.recorded();
    assert_eq!(rec.len(), 1);
    assert_eq!(
        rec[0].action_hash,
        ssh_action_hash(AGENT, "SHA256:key", "SHA256:host")
    );
    assert!(rec[0].allow);
    assert_eq!(t.gate.decide(&ask()).await, GateOutcome::Allow);
    assert!(t.rx.try_recv().is_err(), "remembered: no second prompt");
}

#[tokio::test]
async fn deny_is_remembered_as_a_deny() {
    let mut t = rig(false);
    let (out, _) = decide_answering(&mut t, false).await;
    assert_eq!(out, GateOutcome::Deny);
    assert_eq!(t.gate.decide(&ask()).await, GateOutcome::Deny);
    assert!(t.rx.try_recv().is_err());
}

#[tokio::test]
async fn forgetting_the_decision_prompts_again() {
    let mut t = rig(false);
    decide_answering(&mut t, true).await;
    t.store.forget();
    let (out, _) = decide_answering(&mut t, true).await;
    assert_eq!(out, GateOutcome::Allow);
}

#[tokio::test]
async fn the_prompt_names_agent_key_and_host() {
    let mut t = rig(false);
    let (_, n) = decide_answering(&mut t, true).await;
    let c = call_of(&n);
    assert_eq!(c["tool_name"], SSH_SIGN_TOOL);
    assert_eq!(
        c["action_hash"],
        ssh_action_hash(AGENT, "SHA256:key", "SHA256:host")
    );
    let p = c["prompt"].as_str().unwrap();
    for want in [AGENT, "deploy", "SHA256:key", "github.com", "SHA256:host"] {
        assert!(p.contains(want), "{want} missing from {p:?}");
    }
    assert!(!p.contains(AUDIT_ONLY_LINE));
    // Never auto-allowable by a session lane: the tier is above the grant
    // ceiling.
    assert_eq!(c["risk"], serde_json::json!(SSH_SIGN_RISK));
    assert!(!mur_common::hitl::tier_may_be_granted(SSH_SIGN_RISK));
}

#[tokio::test]
async fn an_audit_only_prompt_carries_the_extra_line() {
    let mut t = rig(true);
    let (_, n) = decide_answering(&mut t, true).await;
    let p = call_of(&n)["prompt"].as_str().unwrap().to_string();
    assert!(p.contains(AUDIT_ONLY_LINE), "{p:?}");
    assert_eq!(
        AUDIT_ONLY_LINE,
        "this agent can reach your ssh-agent directly; this approval is not the only gate"
    );
}

/// Unattended: nobody answers in time, the sign is deferred, the ask stays
/// pending, and a later answer settles the next request without a prompt.
#[tokio::test]
async fn unanswered_defers_and_a_late_answer_settles_the_retry() {
    let mut t = rig(false);
    let g = t.gate.clone();
    let h = tokio::spawn(async move { g.decide(&ask()).await });
    let n = t.rx.recv().await.unwrap();
    assert_eq!(h.await.unwrap(), GateOutcome::Deferred);
    let id = call_of(&n)["hitl_id"].as_str().unwrap().to_string();
    assert_eq!(t.gate.parking.list().len(), 1, "still listed as pending");
    answer(&t.approvals, &id, true).await;
    // The background waiter records it; poll briefly.
    for _ in 0..50 {
        if !t.store.recorded().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(t.gate.parking.list().is_empty());
    assert_eq!(t.gate.decide(&ask()).await, GateOutcome::Allow);
    assert!(t.rx.try_recv().is_err());
}

/// Two connections asking for the same pair share one prompt.
#[tokio::test]
async fn concurrent_asks_for_one_pair_share_one_prompt() {
    let mut t = rig(false);
    let (g1, g2) = (t.gate.clone(), t.gate.clone());
    let h1 = tokio::spawn(async move { g1.decide(&ask()).await });
    let n = t.rx.recv().await.unwrap();
    let h2 = tokio::spawn(async move { g2.decide(&ask()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let id = call_of(&n)["hitl_id"].as_str().unwrap().to_string();
    answer(&t.approvals, &id, true).await;
    assert_eq!(h1.await.unwrap(), GateOutcome::Allow);
    assert_eq!(h2.await.unwrap(), GateOutcome::Allow);
    assert!(t.rx.try_recv().is_err(), "one prompt only");
}

/// Review of #2220: the pending entry must be reserved before any await.
/// With the notification channel full, both requests reach the gate while
/// the first prompt is still stuck in `send`; they must still raise one
/// prompt and share its answer.
#[tokio::test]
async fn concurrent_asks_while_delivery_is_blocked_raise_one_prompt() {
    let mut t = rig_with_capacity(false, 1);
    t.gate
        .notifier
        .send(serde_json::json!("filler"))
        .await
        .unwrap();
    let (g1, g2) = (t.gate.clone(), t.gate.clone());
    let h1 = tokio::spawn(async move { g1.decide(&ask()).await });
    let h2 = tokio::spawn(async move { g2.decide(&ask()).await });
    // Current-thread runtime: this lets both tasks run until they block.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(t.rx.recv().await.unwrap(), serde_json::json!("filler"));
    let n = t.rx.recv().await.expect("a prompt");
    let id = call_of(&n)["hitl_id"].as_str().unwrap().to_string();
    answer(&t.approvals, &id, true).await;
    assert_eq!(h1.await.unwrap(), GateOutcome::Allow);
    assert_eq!(h2.await.unwrap(), GateOutcome::Allow);
    assert!(t.rx.try_recv().is_err(), "one prompt only");
    assert!(t.approvals.lock().await.is_empty(), "no second approval");
    assert_eq!(t.store.recorded().len(), 1);
}

/// Requests that joined an ask whose prompt could not be sent are refused
/// too, and the reservation is released so a later request can try again.
#[tokio::test]
async fn a_failed_send_releases_the_reservation_for_joiners() {
    let t = rig_with_capacity(false, 1);
    t.gate
        .notifier
        .send(serde_json::json!("filler"))
        .await
        .unwrap();
    let (g1, g2) = (t.gate.clone(), t.gate.clone());
    let h1 = tokio::spawn(async move { g1.decide(&ask()).await });
    let h2 = tokio::spawn(async move { g2.decide(&ask()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(t.rx);
    for h in [h1, h2] {
        match h.await.unwrap() {
            GateOutcome::Unavailable(_) => {}
            o => panic!("expected Unavailable, got {o:?}"),
        }
    }
    assert!(t.gate.pending.lock().unwrap().is_empty());
    assert!(t.approvals.lock().await.is_empty());
    assert!(t.gate.parking.list().is_empty());
}

#[tokio::test]
async fn a_closed_channel_is_unavailable_and_leaves_nothing_pending() {
    let t = rig(false);
    drop(t.rx);
    match t.gate.decide(&ask()).await {
        GateOutcome::Unavailable(_) => {}
        o => panic!("expected Unavailable, got {o:?}"),
    }
    assert!(t.approvals.lock().await.is_empty());
    assert!(t.gate.parking.list().is_empty());
}

/// `--no-hitl` never answers a sign request (spec "no HITL switches"). The
/// waiver is attached to the task runner only; this pins the tier that keeps
/// it out even if someone later threads it into the ssh gate.
#[tokio::test]
async fn a_startup_waiver_never_settles_an_unremembered_sign() {
    use mur_common::agent::{OverrideFlags, OverrideRecord};
    use mur_common::hitl::startup_waiver::{AskOrigin, StartupWaiver};
    use mur_common::startup_override::ConfirmedBy;

    let waiver = StartupWaiver::from_record(&OverrideRecord {
        nonce: "nonce-ssh".into(),
        flags: OverrideFlags {
            no_hitl: true,
            ..OverrideFlags::default()
        },
        confirmed_by: ConfirmedBy::Tty,
    })
    .expect("no_hitl builds a waiver");
    for bits in 0u8..32 {
        let origin = AskOrigin {
            explicit: bits & 1 != 0,
            default: bits & 2 != 0,
            clamp: bits & 4 != 0,
            intent_raised: bits & 8 != 0,
            hook_raised: bits & 16 != 0,
        };
        assert!(
            !waiver.may_waive(origin, Some(SSH_SIGN_RISK)),
            "a waiver must not pass a sign ask from {origin:?}"
        );
    }

    // With the waiver live, an unremembered pair still raises a prompt and
    // only the human's answer settles it.
    let mut t = rig(false);
    let (out, n) = decide_answering(&mut t, false).await;
    assert_eq!(out, GateOutcome::Deny);
    assert_eq!(call_of(&n)["tool_name"], SSH_SIGN_TOOL);
    assert_eq!(t.store.recorded().len(), 1, "the human's deny is recorded");
}
