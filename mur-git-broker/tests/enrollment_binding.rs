#![cfg(unix)]
//! What a running broker does when the enrollment records move under a pending request, and how
//! it fails closed when they cannot be read. Real git, a fake clock, records the test can edit.
use chrono::Utc;
use mur_git_broker::broker::*;
use mur_git_broker::enrollment::Enrollments;
use mur_git_broker::{error::BrokerError, pending::*};
mod common;
use common::orch::*;
use common::*;

/// Pend one request, return what is needed to approve it.
fn pended(e: &Env, b: &Broker) -> (RequestKey, String) {
    let (k, d, pack) = req(e, "r1");
    let h = b.submit_request(&k, &d, &pack).unwrap();
    (k, h)
}
fn assert_voided_and_unpushed(e: &Env, b: &Broker, k: &RequestKey, h: &str) {
    let r = b.on_approval(k, proof("e1", "r1", h));
    assert_eq!(r.unwrap_err(), BrokerError::PolicyChanged);
    assert_eq!(state(b.store(), k), State::PolicyChanged);
    assert_eq!(git(&e.remote, &["rev-parse", "refs/heads/agent/x"]), e.old);
    assert_eq!(leftovers(e), 0, "its private repo is gone");
}

#[test]
fn revoked_enrollment_voids_a_pending_request() {
    let e = env();
    let b = broker(&e, policy());
    let (k, h) = pended(&e, &b);
    *e.records.0.lock().unwrap() = Ok(Some(Enrollments::empty(EPOCH + 1)));
    assert_voided_and_unpushed(&e, &b, &k, &h);
}

#[test]
fn deleted_enrollment_file_voids_a_pending_request() {
    let e = env();
    let b = broker(&e, policy());
    let (k, h) = pended(&e, &b);
    *e.records.0.lock().unwrap() = Ok(None);
    assert_voided_and_unpushed(&e, &b, &k, &h);
}

#[test]
fn re_enrolled_with_a_new_epoch_voids_a_pending_request() {
    let e = env();
    let b = broker(&e, policy());
    let (k, h) = pended(&e, &b);
    // Same policy, same repo: only the epoch moved (revoke then enroll again).
    let mut recs = records_for(&e.endpoint(), &e.fit(policy()));
    recs.enrollments[0].epoch = EPOCH + 1;
    *e.records.0.lock().unwrap() = Ok(Some(recs));
    assert_voided_and_unpushed(&e, &b, &k, &h);
}

#[test]
fn rebinding_the_repo_id_to_another_repo_voids_a_pending_request() {
    let e = env();
    let b = broker(&e, policy());
    let (k, h) = pended(&e, &b);
    let mut recs = records_for(&e.endpoint(), &e.fit(policy()));
    recs.repos.get_mut("repo").unwrap().identity = "other-repo".into();
    *e.records.0.lock().unwrap() = Ok(Some(recs));
    assert_voided_and_unpushed(&e, &b, &k, &h);
}

#[test]
fn unreadable_enrollments_leave_a_pending_request_and_its_approval_untouched() {
    let e = env();
    let b = broker(&e, policy());
    let (k, h) = pended(&e, &b);
    *e.records.0.lock().unwrap() = Err(());
    let r = b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(r.unwrap_err(), BrokerError::EnrollmentUnavailable);
    assert_eq!(
        state(b.store(), &k),
        State::PendingApproval,
        "still waiting"
    );
    assert_eq!(git(&e.remote, &["rev-parse", "refs/heads/agent/x"]), e.old);
    // The same approval, once the file reads again, is not burnt.
    e.enroll(policy());
    assert_eq!(
        b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(git(&e.remote, &["rev-parse", "refs/heads/agent/x"]), e.new);
}

#[test]
fn unreadable_enrollments_refuse_a_new_request_and_leave_nothing_behind() {
    let e = env();
    let b = broker(&e, policy());
    let (k, d, pack) = req(&e, "r1");
    *e.records.0.lock().unwrap() = Err(());
    let r = b.submit_request(&k, &d, &pack);
    assert_eq!(r.unwrap_err(), BrokerError::EnrollmentUnavailable);
    assert!(b.store().get(&k).unwrap().is_none());
    assert_eq!(leftovers(&e), 0);
}

#[test]
fn request_for_an_unenrolled_remote_is_refused_with_no_row() {
    let e = env();
    let b = broker(&e, policy());
    let (k, mut d, pack) = req(&e, "r1");
    d.remote_id = "elsewhere".into();
    let r = b.submit_request(&k, &d, &pack);
    assert_eq!(r.unwrap_err(), BrokerError::NotEnrolled);
    assert!(b.store().get(&k).unwrap().is_none());
    assert_eq!(leftovers(&e), 0);
}

#[test]
fn every_decision_rereads_the_records() {
    let e = env();
    let b = broker(&e, policy());
    let (k, d, pack) = req(&e, "r1");
    let before = e.reads();
    let h = b.submit_request(&k, &d, &pack).unwrap();
    let after_submit = e.reads();
    assert!(after_submit > before, "submit reads the records");
    b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert!(e.reads() > after_submit, "approval reads them again");
}

#[test]
fn push_goes_to_the_endpoint_the_approval_was_checked_against() {
    // `execute` is handed the binding `on_approval` validated and never reads the records itself.
    let e = env();
    let b = broker(&e, policy());
    let (k, h) = pended(&e, &b);
    let before = e.reads();
    assert_eq!(
        b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(
        e.reads() - before,
        1,
        "one read per approval, not one per step"
    );
}

#[test]
fn verified_request_is_stamped_with_the_current_epoch_and_identity() {
    use mur_git_broker::request_file::RequestFile;
    let e = env();
    let b = broker(&e, policy());
    let (k, d, pack) = req(&e, "r1");
    let rf = RequestFile {
        agent: k.agent_id.clone(),
        task_id: k.task_id.clone(),
        request_id: k.request_id.clone(),
        repo_id: d.repo_id.clone(),
        remote_id: d.remote_id.clone(),
        r#ref: d.updates[0].r#ref.clone(),
        old_sha: d.updates[0].old_sha.clone(),
        new_sha: d.updates[0].new_sha.clone(),
        pack_sha256: String::new(),
        requested_at: Utc::now(),
        key_version: 0,
        sig: String::new(),
    };
    let h = b.submit_verified(&rf, &pack).unwrap();
    let row = b.store().get(&k).unwrap().unwrap();
    let stored: mur_git_broker::action::ActionDocument =
        serde_json::from_str(&row.doc_json).unwrap();
    assert_eq!(stored.enrollment_epoch, EPOCH);
    assert_eq!(stored.repo_identity, IDENTITY);
    assert_eq!(stored.action_hash().unwrap(), h);
}
