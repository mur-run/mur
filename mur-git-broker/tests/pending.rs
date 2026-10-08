#![cfg(unix)]
// tests/pending.rs — real SQLite in a tempdir.
use chrono::{DateTime, Duration, TimeZone, Utc};
use mur_git_broker::{error::BrokerError, pending::*, policy::BrokerLimits};
use std::sync::{Arc, Barrier};
mod common;
use common::action_for;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 0, 0, 0).unwrap()
}
fn key(agent: &str, req: &str) -> RequestKey {
    RequestKey {
        agent_id: agent.into(),
        task_id: "t1".into(),
        request_id: req.into(),
    }
}
fn doc(agent: &str, req: &str, new: &str) -> mur_git_broker::action::ActionDocument {
    let mut d = action_for(&"a".repeat(40), new, "refs/heads/agent/x");
    d.agent_id = agent.into();
    d.request_id = req.into();
    d
}
fn store() -> (tempfile::TempDir, PendingStore) {
    let t = tempfile::tempdir().unwrap();
    let s = PendingStore::open(&t.path().join("pending.sqlite")).unwrap();
    (t, s)
}
fn submit(
    s: &PendingStore,
    agent: &str,
    req: &str,
    new: &str,
    now: DateTime<Utc>,
    l: &BrokerLimits,
) -> Result<Submitted, BrokerError> {
    let d = doc(agent, req, new);
    let h = d.action_hash().unwrap();
    s.submit(&key(agent, req), &d, &h, now, l)
}
fn ok(s: &PendingStore, k: &RequestKey, from: State, to: State) {
    s.transition(k, from, to).unwrap();
}

#[test]
fn same_id_same_content_returns_existing_without_a_new_row() {
    let (_t, s) = store();
    let l = BrokerLimits::default();
    assert!(matches!(
        submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap(),
        Submitted::New
    ));
    assert!(matches!(
        submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap(),
        Submitted::Existing(_)
    ));
    ok(
        &s,
        &key("alice", "r1"),
        State::Validated,
        State::PendingApproval,
    );
    assert_eq!(s.list_pending().unwrap().len(), 1);
}
#[test]
fn same_id_different_content_is_request_conflict() {
    let (_t, s) = store();
    let l = BrokerLimits::default();
    submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap();
    assert_eq!(
        submit(&s, "alice", "r1", &"c".repeat(40), t0(), &l).unwrap_err(),
        BrokerError::RequestConflict
    );
}
#[test]
fn same_request_id_in_another_agent_is_a_different_request() {
    let (_t, s) = store();
    let l = BrokerLimits::default();
    submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap();
    assert!(matches!(
        submit(&s, "bob", "r1", &"c".repeat(40), t0(), &l).unwrap(),
        Submitted::New
    ));
}
#[test]
fn pending_cap_returns_queue_full_and_writes_nothing() {
    let (_t, s) = store();
    let l = BrokerLimits {
        max_pending_per_agent: 2,
        ..BrokerLimits::default()
    };
    for i in 0..2 {
        submit(&s, "alice", &format!("r{i}"), &"b".repeat(40), t0(), &l).unwrap();
    }
    assert_eq!(
        submit(&s, "alice", "r9", &"b".repeat(40), t0(), &l).unwrap_err(),
        BrokerError::QueueFull
    );
    assert!(
        s.get(&key("alice", "r9")).unwrap().is_none(),
        "no row for the rejected request"
    );
    submit(&s, "bob", "r0", &"b".repeat(40), t0(), &l).unwrap(); // another agent is unaffected
}
#[test]
fn rate_cap_returns_rate_limited_even_below_pending_cap() {
    let (_t, s) = store();
    let l = BrokerLimits {
        max_new_requests_per_window: 2,
        max_pending_per_agent: 100,
        ..BrokerLimits::default()
    };
    for i in 0..2 {
        let k = key("alice", &format!("r{i}"));
        submit(&s, "alice", &format!("r{i}"), &"b".repeat(40), t0(), &l).unwrap();
        ok(&s, &k, State::Validated, State::PendingApproval);
        ok(&s, &k, State::PendingApproval, State::Cancelled);
    }
    assert_eq!(
        submit(&s, "alice", "r2", &"b".repeat(40), t0(), &l).unwrap_err(),
        BrokerError::RateLimited
    );
    let later = t0() + Duration::seconds(l.request_window_secs + 1);
    submit(&s, "alice", "r2", &"b".repeat(40), later, &l).unwrap(); // window slid
}
#[test]
fn cap_rejections_are_audited_but_create_no_row() {
    let (_t, s) = store();
    let l = BrokerLimits {
        max_pending_per_agent: 1,
        ..BrokerLimits::default()
    };
    submit(&s, "alice", "r0", &"b".repeat(40), t0(), &l).unwrap();
    let _ = submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l);
    assert_eq!(s.audit_count("alice", "queue_full").unwrap(), 1);
    assert!(s.get(&key("alice", "r1")).unwrap().is_none());
}
#[test]
fn pending_rows_are_never_evicted_by_age() {
    let (_t, s) = store();
    let l = BrokerLimits::default();
    let k = key("alice", "r1");
    submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap();
    ok(&s, &k, State::Validated, State::PendingApproval);
    // 30 days later another request arrives; the old row must be untouched.
    submit(
        &s,
        "alice",
        "r2",
        &"b".repeat(40),
        t0() + Duration::days(30),
        &l,
    )
    .unwrap();
    assert_eq!(s.get(&k).unwrap().unwrap().state, State::PendingApproval);
    assert!(s.list_pending().unwrap().iter().any(|r| r.key == k));
}
#[test]
fn pending_survives_reopen() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("pending.sqlite");
    let l = BrokerLimits::default();
    {
        let s = PendingStore::open(&p).unwrap();
        submit(&s, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap();
        ok(
            &s,
            &key("alice", "r1"),
            State::Validated,
            State::PendingApproval,
        );
    }
    let s = PendingStore::open(&p).unwrap();
    assert_eq!(
        s.get(&key("alice", "r1")).unwrap().unwrap().state,
        State::PendingApproval
    );
}
#[test]
fn illegal_edges_are_rejected() {
    use State::*;
    let (_t, s) = store();
    let l = BrokerLimits::default();
    for (i, (from, to)) in [
        (Succeeded, Executing),
        (Denied, Approved),
        (PendingApproval, Executing),
        (Cancelled, PendingApproval),
        (Validated, Approved),
    ]
    .into_iter()
    .enumerate()
    {
        let req = format!("r{i}");
        submit(&s, "alice", &req, &"b".repeat(40), t0(), &l).unwrap();
        s.force_state_for_test(&key("alice", &req), from).unwrap();
        assert_eq!(
            s.transition(&key("alice", &req), from, to).unwrap_err(),
            BrokerError::NotPending,
            "{from:?}→{to:?}"
        );
        assert_eq!(
            s.get(&key("alice", &req)).unwrap().unwrap().state,
            from,
            "state unchanged"
        );
    }
}
#[test]
fn transition_is_compare_and_swap() {
    let (t, _s) = store();
    let p = t.path().join("pending.sqlite");
    let l = BrokerLimits::default();
    let s0 = PendingStore::open(&p).unwrap();
    submit(&s0, "alice", "r1", &"b".repeat(40), t0(), &l).unwrap();
    ok(
        &s0,
        &key("alice", "r1"),
        State::Validated,
        State::PendingApproval,
    );
    let bar = Arc::new(Barrier::new(2));
    let hs: Vec<_> = (0..2)
        .map(|_| {
            let (p, bar) = (p.clone(), bar.clone());
            std::thread::spawn(move || {
                let s = PendingStore::open(&p).unwrap();
                bar.wait();
                s.transition(&key("alice", "r1"), State::PendingApproval, State::Approved)
                    .is_ok()
            })
        })
        .collect();
    let wins = hs
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|b| *b)
        .count();
    assert_eq!(wins, 1);
}
#[test]
fn explicit_events_are_the_only_way_out_of_pending() {
    use State::*;
    let (_t, s) = store();
    let l = BrokerLimits::default();
    for (i, to) in [Approved, Denied, Cancelled, PolicyChanged]
        .into_iter()
        .enumerate()
    {
        let req = format!("r{i}");
        let k = key("alice", &req);
        submit(&s, "alice", &req, &"b".repeat(40), t0(), &l).unwrap();
        ok(&s, &k, Validated, PendingApproval);
        ok(&s, &k, PendingApproval, to);
    }
    // there is no API that moves a pending row on elapsed time:
    assert!(
        !include_str!("../src/pending.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            // a DELETE may only ever name the `validated` state (a request whose git work failed)
            .any(|l| l.contains("DELETE") && !l.contains("state='validated'")
                || l.contains("ApprovalExpired") && l.contains("PendingApproval"))
    );
}
