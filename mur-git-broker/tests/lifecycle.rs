#![cfg(unix)]
//! T10: approval, consume-once, push, orchestration. Real git, a fake clock.
use chrono::{DateTime, Duration, TimeZone, Utc};
use mur_git_broker::{approval::*, error::BrokerError, pending::*, policy::BrokerLimits};
use std::sync::Mutex;
mod common;
use common::*;

struct FakeClock(Mutex<DateTime<Utc>>);
impl FakeClock {
    fn new() -> Self {
        Self(Mutex::new(
            Utc.with_ymd_and_hms(2026, 10, 7, 0, 0, 0).unwrap(),
        ))
    }
    fn advance(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
    }
}
impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn key(req: &str) -> RequestKey {
    RequestKey {
        agent_id: "alice".into(),
        task_id: "t1".into(),
        request_id: req.into(),
    }
}
/// A request row already at `PendingApproval`, plus its hash.
fn pending(s: &PendingStore, req: &str, clock: &FakeClock) -> (RequestKey, String) {
    let mut d = action_for(&"a".repeat(40), &"b".repeat(40), "refs/heads/agent/x");
    d.request_id = req.into();
    let h = d.action_hash().unwrap();
    let k = key(req);
    s.submit(&k, &d, &h, clock.now(), &BrokerLimits::default())
        .unwrap();
    s.transition(&k, State::Validated, State::PendingApproval)
        .unwrap();
    (k, h)
}
fn proof(ev: &str, req: &str, h: &str) -> ApprovalProof {
    ApprovalProof {
        event_id: ev.into(),
        request_id: req.into(),
        action_hash: h.into(),
    }
}
fn store() -> (tempfile::TempDir, PendingStore) {
    let t = tempfile::tempdir().unwrap();
    let s = PendingStore::open(&t.path().join("p.sqlite")).unwrap();
    (t, s)
}
fn state(s: &PendingStore, k: &RequestKey) -> State {
    s.get(k).unwrap().unwrap().state
}

// ---- approval ---------------------------------------------------------------------------------
#[test]
fn wrong_action_hash_or_request_id_does_not_advance() {
    let (_t, s) = store();
    let c = FakeClock::new();
    let (k, h) = pending(&s, "r1", &c);
    assert!(accept_approval(&s, &k, &proof("e1", "r1", &"0".repeat(64)), &c).is_err());
    assert!(accept_approval(&s, &k, &proof("e2", "r-other", &h), &c).is_err());
    assert_eq!(state(&s, &k), State::PendingApproval);
}

#[test]
fn replay_after_consumption_is_inert() {
    let (_t, s) = store();
    let c = FakeClock::new();
    let (k, h) = pending(&s, "r1", &c);
    accept_approval(&s, &k, &proof("e1", "r1", &h), &c).unwrap();
    begin_execution(&s, &k, &c).unwrap();
    assert!(accept_approval(&s, &k, &proof("e1", "r1", &h), &c).is_err());
    assert!(accept_approval(&s, &k, &proof("e2", "r1", &h), &c).is_err());
    assert_eq!(state(&s, &k), State::Executing);
}

#[test]
fn approval_for_cancelled_or_expired_request_is_inert() {
    let (_t, s) = store();
    let c = FakeClock::new();
    for (i, dead) in [
        State::Cancelled,
        State::ApprovalExpired,
        State::PolicyChanged,
        State::Denied,
    ]
    .into_iter()
    .enumerate()
    {
        let (k, h) = pending(&s, &format!("r{i}"), &c);
        s.force_state_for_test(&k, dead).unwrap();
        let p = proof(&format!("e{i}"), &format!("r{i}"), &h);
        assert!(accept_approval(&s, &k, &p, &c).is_err());
        assert_eq!(state(&s, &k), dead);
    }
}

#[test]
fn approval_of_a_different_pending_request_is_inert() {
    let (_t, s) = store();
    let c = FakeClock::new();
    let (k1, h1) = pending(&s, "r1", &c);
    let (k2, _h2) = pending(&s, "r2", &c);
    assert!(accept_approval(&s, &k2, &proof("e1", "r1", &h1), &c).is_err());
    assert_eq!(state(&s, &k2), State::PendingApproval);
    accept_approval(&s, &k1, &proof("e1", "r1", &h1), &c).unwrap();
}

#[test]
fn approval_window_starts_at_accept_not_at_submit() {
    let (_t, s) = store();
    let c = FakeClock::new();
    let (k, h) = pending(&s, "r1", &c);
    c.advance(Duration::days(3));
    accept_approval(&s, &k, &proof("e1", "r1", &h), &c).unwrap();
    c.advance(Duration::seconds(4 * 60 + 59));
    begin_execution(&s, &k, &c).unwrap();
    assert_eq!(state(&s, &k), State::Executing);
}

#[test]
fn approval_older_than_five_minutes_at_execution_is_expired() {
    let (_t, s) = store();
    let c = FakeClock::new();
    let (k, h) = pending(&s, "r1", &c);
    accept_approval(&s, &k, &proof("e1", "r1", &h), &c).unwrap();
    c.advance(Duration::seconds(5 * 60 + 1));
    assert_eq!(
        begin_execution(&s, &k, &c).unwrap_err(),
        BrokerError::ApprovalExpired
    );
    assert_eq!(state(&s, &k), State::ApprovalExpired);
}

#[test]
fn entering_executing_consumes_the_approval() {
    let (_t, s) = store();
    let c = FakeClock::new();
    let (k, h) = pending(&s, "r1", &c);
    accept_approval(&s, &k, &proof("e1", "r1", &h), &c).unwrap();
    begin_execution(&s, &k, &c).unwrap();
    assert_eq!(
        begin_execution(&s, &k, &c).unwrap_err(),
        BrokerError::NotPending
    );
}
