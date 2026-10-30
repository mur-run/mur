//! Phase 3: the approval step (plan Task 3.2). A fake gate stands in for
//! gate B; every outcome but `Allow` must leave the upstream without a single
//! sign request.

use std::sync::{Arc, Mutex};

use super::fixtures::*;
use super::session::{FakeUpstream, World};
use crate::ssh_proxy::gate::{GATE_NOT_READY, GateOutcome, GateSlot, SignAsk, SignGate};
use crate::ssh_proxy::keys::fingerprint;
use crate::ssh_proxy::policy::{PinTrust, Policy};
use crate::ssh_proxy::session::Session;
use crate::ssh_proxy::wire::{self, Answer};
use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule};

/// Answers every ask with `outcome` and remembers what it was asked.
pub(super) struct FakeGate {
    outcome: GateOutcome,
    pub(super) asked: Mutex<Vec<SignAsk>>,
}

impl FakeGate {
    pub(super) fn new(outcome: GateOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            asked: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl SignGate for FakeGate {
    async fn decide(&self, ask: &SignAsk) -> GateOutcome {
        self.asked.lock().unwrap().push(ask.clone());
        self.outcome.clone()
    }
}

/// For tests whose request must never reach the gate: asking it fails the
/// test.
pub(super) struct NeverAsked;

#[async_trait::async_trait]
impl SignGate for NeverAsked {
    async fn decide(&self, ask: &SignAsk) -> GateOutcome {
        panic!("the approval gate was reached: {ask:?}");
    }
}

pub(super) fn forwarded_signs(up: &FakeUpstream) -> usize {
    up.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f[4] == wire::SSH_AGENTC_SIGN_REQUEST)
        .count()
}

/// Bind to the world's host, then ask key `key` to sign, through `gate`.
async fn sign_through(w: &World, gate: &dyn SignGate, key: usize) -> (Answer, FakeUpstream) {
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let ans = {
        let mut sess = Session::new(&p, &up, &sink, gate);
        assert_eq!(sess.handle(&w.good_bind()).await, Answer::Success);
        sess.handle(&sign_body(&w.keys[key].key_blob())).await
    };
    (ans, up)
}

#[tokio::test]
async fn approved_sign_is_forwarded_and_the_signature_returned() {
    let w = World::new();
    let gate = FakeGate::new(GateOutcome::Allow);
    let (ans, up) = sign_through(&w, &*gate, 0).await;
    assert_eq!(ans, Answer::SignResponse(b"A REAL SIGNATURE".to_vec()));
    assert_eq!(forwarded_signs(&up), 1);
    assert_eq!(w.last_code(), None, "an approved sign is not a refusal");
}

#[tokio::test]
async fn the_gate_is_asked_with_policy_labels_and_bind_fingerprints() {
    let w = World::new();
    let gate = FakeGate::new(GateOutcome::Allow);
    sign_through(&w, &*gate, 0).await;
    let asked = gate.asked.lock().unwrap();
    assert_eq!(
        asked.as_slice(),
        [SignAsk {
            key_fp: fingerprint(&w.keys[0].key_blob()),
            key_label: Some("deploy".into()),
            host_key_fp: fingerprint(&w.host.key_blob()),
            host_label: Some("test-host".into()),
        }]
    );
}

#[tokio::test]
async fn each_refusing_outcome_has_its_own_code_and_forwards_nothing() {
    for (outcome, want) in [
        (GateOutcome::Deny, "hitl_denied"),
        (GateOutcome::Unavailable("down".into()), "hitl_unavailable"),
        (GateOutcome::Deferred, "hitl_deferred"),
    ] {
        let w = World::new();
        let gate = FakeGate::new(outcome);
        let (ans, up) = sign_through(&w, &*gate, 0).await;
        assert_eq!(ans, Answer::Failure, "{want}");
        assert_eq!(w.last_code().as_deref(), Some(want));
        assert_eq!(forwarded_signs(&up), 0, "{want}: reached upstream");
    }
}

/// A request the policy refuses never reaches the gate: no prompt the user
/// could approve by mistake (spec "Binding sign requests…", rule 6).
#[tokio::test]
async fn a_policy_refusal_never_asks() {
    let w = World::new();
    let gate = FakeGate::new(GateOutcome::Allow);
    let (ans, up) = sign_through(&w, &*gate, 1).await;
    assert_eq!(ans, Answer::Failure);
    assert_eq!(w.last_code().as_deref(), Some("key_not_allowed"));
    assert!(gate.asked.lock().unwrap().is_empty());
    assert_eq!(forwarded_signs(&up), 0);
}

/// An approved sign the upstream cannot answer is `upstream_unavailable`,
/// not a signature.
#[tokio::test]
async fn approved_but_upstream_down_is_upstream_unavailable() {
    let w = World::new();
    let gate = FakeGate::new(GateOutcome::Allow);
    let (p, up, sink) = (w.policy(), w.upstream().down(), w.sink());
    let mut sess = Session::new(&p, &up, &sink, &*gate);
    sess.handle(&w.good_bind()).await;
    let ans = sess.handle(&sign_body(&w.keys[0].key_blob())).await;
    assert_eq!(ans, Answer::Failure);
    assert_eq!(w.last_code().as_deref(), Some("upstream_unavailable"));
}

/// The slot the supervisor fills after the seal refuses until it is filled,
/// and then cannot be swapped.
#[tokio::test]
async fn an_empty_gate_slot_is_unavailable_and_fills_once() {
    let slot = GateSlot::default();
    let ask = SignAsk {
        key_fp: "k".into(),
        key_label: None,
        host_key_fp: "h".into(),
        host_label: None,
    };
    assert_eq!(
        slot.decide(&ask).await,
        GateOutcome::Unavailable(GATE_NOT_READY.into())
    );
    assert!(slot.set(FakeGate::new(GateOutcome::Deny)));
    assert!(!slot.set(FakeGate::new(GateOutcome::Allow)));
    assert_eq!(slot.decide(&ask).await, GateOutcome::Deny);
}

#[test]
fn policy_carries_the_host_label_of_the_allowed_pair() {
    let (k, h1, h2) = (
        fingerprint(b"key"),
        fingerprint(b"host1"),
        fingerprint(b"host2"),
    );
    let host = |fp: &str, label: Option<&str>| SshHostRule {
        host_key: fp.into(),
        label: label.map(str::to_string),
    };
    let p = Policy::build(
        &SshEntitlement {
            keys: vec![SshKeyRule {
                fingerprint: k.clone(),
                label: None,
                allow_hosts: vec![host(&h1, Some("github.com")), host(&h2, None)],
            }],
        },
        PinTrust::Pinned,
        true,
    );
    assert_eq!(
        p.check_sign(&k, Some(&h1)).unwrap().host_label.as_deref(),
        Some("github.com")
    );
    assert_eq!(p.check_sign(&k, Some(&h2)).unwrap().host_label, None);
}
