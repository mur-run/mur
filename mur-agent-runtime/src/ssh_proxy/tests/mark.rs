//! Signing marks (spec "Signing mark", writer): one mark per approval, only
//! for a signature that came back, never for a refusal.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use mur_ssh_request::{SIGNING_MARK_NOTICE, SigningMark};

use super::fixtures::*;
use super::gate::FakeGate;
use super::session::World;
use crate::ssh_proxy::gate::{GateOutcome, LiveDecision};
use crate::ssh_proxy::keys::fingerprint;
use crate::ssh_proxy::mark::MarkSink;
use crate::ssh_proxy::session::Session;
use crate::ssh_proxy::wire::Answer;

fn decision(issued_at: &str) -> LiveDecision {
    let at: DateTime<Utc> = issued_at.parse().unwrap();
    LiveDecision {
        action_hash: "hash-of-the-pair".into(),
        allow: true,
        issued_at: at,
        expires_at: at,
        label: None,
        surface: "test".into(),
    }
}

/// A sink that remembers what it would have posted.
fn recording() -> (MarkSink, Arc<Mutex<Vec<SigningMark>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    (
        MarkSink::new(Arc::new(move |m: &SigningMark| {
            g.lock().unwrap().push(m.clone())
        })),
        got,
    )
}

/// Marks are posted off the reply path; wait for `n` of them.
async fn settle(got: &Mutex<Vec<SigningMark>>, n: usize) -> Vec<SigningMark> {
    for _ in 0..100 {
        if got.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Give a wrongly spawned extra write the chance to show up.
    tokio::time::sleep(Duration::from_millis(20)).await;
    got.lock().unwrap().clone()
}

/// Sign `times` times on one connection, under `outcome`.
async fn sign_n(w: &World, outcome: GateOutcome, up_down: bool, times: usize, marks: &MarkSink) {
    let gate = FakeGate::new(outcome);
    let (p, sink) = (w.policy(), w.sink());
    let up = if up_down {
        w.upstream().down()
    } else {
        w.upstream()
    };
    let mut sess = Session::new(&p, &up, &sink, &*gate).with_marks(marks);
    assert_eq!(sess.handle(&w.good_bind()).await, Answer::Success);
    for _ in 0..times {
        sess.handle(&sign_body(&w.keys[0].key_blob())).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_returned_signature_writes_one_mark_bound_to_the_approval() {
    let w = World::new();
    let (marks, got) = recording();
    let d = decision("2026-11-11T10:00:00Z");
    sign_n(
        &w,
        GateOutcome::Allow {
            decision: Some(d.clone()),
        },
        false,
        3,
        &marks,
    )
    .await;
    let got = settle(&got, 1).await;
    assert_eq!(got.len(), 1, "one mark per approval, not per signature");
    let m = &got[0];
    assert_eq!(m.notice, SIGNING_MARK_NOTICE);
    assert_eq!(m.action_hash, d.action_hash);
    assert_eq!(m.issued_at, d.issued_at);
    assert_eq!(m.key_fp, fingerprint(&w.keys[0].key_blob()));
    assert_eq!(m.host_key_fp, fingerprint(&w.host.key_blob()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_approval_for_the_pair_writes_a_new_mark() {
    let w = World::new();
    let (marks, got) = recording();
    for at in ["2026-11-11T10:00:00Z", "2026-11-12T10:00:00Z"] {
        sign_n(
            &w,
            GateOutcome::Allow {
                decision: Some(decision(at)),
            },
            false,
            1,
            &marks,
        )
        .await;
    }
    assert_eq!(settle(&got, 2).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_mark_without_a_signature_or_a_decision() {
    let d = decision("2026-11-11T10:00:00Z");
    let cases = [
        ("no decision", GateOutcome::Allow { decision: None }, false),
        (
            "upstream down",
            GateOutcome::Allow {
                decision: Some(d.clone()),
            },
            true,
        ),
        ("denied", GateOutcome::Deny, false),
        ("deferred", GateOutcome::Deferred, false),
    ];
    for (what, outcome, down) in cases {
        let w = World::new();
        let (marks, got) = recording();
        sign_n(&w, outcome, down, 1, &marks).await;
        assert!(settle(&got, 1).await.is_empty(), "{what}: wrote a mark");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deny_decision_never_writes_a_mark() {
    let w = World::new();
    let (marks, got) = recording();
    let d = LiveDecision {
        allow: false,
        ..decision("2026-11-11T10:00:00Z")
    };
    sign_n(
        &w,
        GateOutcome::Allow { decision: Some(d) },
        false,
        1,
        &marks,
    )
    .await;
    assert!(settle(&got, 1).await.is_empty());
}
