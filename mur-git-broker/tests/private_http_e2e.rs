#![cfg(unix)]
//! A private remote behind HTTP Basic auth, served by the real `git-http-backend`, driven through
//! the broker from `submit_request` to `on_approval`. Plain loopback http (the `test-support`
//! endpoint seam): git's 401 -> askpass -> retry flow is the same with or without TLS.
//!
//! The question this file answers: does a credential-bound enrollment get past *intake*? Intake
//! prefetches the target ref from the remote before any human has approved anything, so a remote
//! that refuses anonymous reads must be readable at that point too, or the request never reaches
//! approval at all.
use mur_git_broker::{error::BrokerError, pending::State};
mod common;
#[path = "common/http.rs"]
mod http;
use common::orch::*;
use http::*;

/// Control: the harness itself works. With anonymous reads allowed, intake gets to approval.
#[test]
fn an_anonymous_readable_remote_reaches_approval() {
    let r = rig(false);
    submit(&r).expect("intake succeeds when the remote allows anonymous reads");
}

/// The private-repo path end to end: intake must read the remote with the enrolled credential,
/// the push after approval must land, and the token must never travel in a request line.
#[test]
fn a_private_remote_is_prefetched_with_the_credential_and_the_push_lands() {
    let r = rig(true);
    let (k, h) = match submit(&r) {
        Ok(v) => v,
        Err(e) => panic!(
            "intake failed on a credential-bound private remote: {e:?}; server saw {:#?}",
            r.seen.lock().unwrap()
        ),
    };
    assert_eq!(
        remote_ref(&r),
        r.e.old,
        "nothing is written before approval"
    );
    assert_eq!(
        r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(remote_ref(&r), r.e.new, "the push moved the remote ref");
    let seen = r.seen.lock().unwrap();
    assert!(seen.iter().any(|s| s.authed), "{seen:#?}");
    assert!(seen.iter().all(|s| !s.leaked_token_in_line), "{seen:#?}");
}

/// A public remote never asks, so git never runs the askpass and the token is never sent, even
/// though the enrollment carries one.
#[test]
fn an_anonymous_readable_remote_is_never_sent_the_token() {
    let r = rig(false);
    let (k, h) = submit(&r).unwrap();
    r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert_eq!(remote_ref(&r), r.e.new);
    let seen = r.seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|s| !s.authed), "{seen:#?}");
}

/// The prefetch runs before approval, so a redirect there must not carry the token anywhere:
/// git is told not to follow it, and the request fails closed with no pending row.
#[test]
fn a_prefetch_redirect_is_not_followed_and_never_carries_the_token() {
    let t = tempfile::tempdir().unwrap();
    let (evil_port, evil_seen) = serve(t.path().to_path_buf(), false, Logins::single());
    let (port, redirect_seen) = redirector(format!(
        "http://127.0.0.1:{evil_port}/remote.git/info/refs?service=git-upload-pack"
    ));
    let r = rig_with(
        true,
        TOKEN,
        Some(format!("http://127.0.0.1:{port}/remote.git")),
    );
    let err = submit(&r).unwrap_err();
    assert!(matches!(err, BrokerError::PrefetchRejected(_)), "{err:?}");
    assert!(
        r.b.store().get(&key("r1")).unwrap().is_none(),
        "no pending row"
    );
    assert!(
        evil_seen.lock().unwrap().is_empty(),
        "the redirect was followed"
    );
    assert!(
        redirect_seen.lock().unwrap().iter().all(|a| !a),
        "token sent unprompted"
    );
}

/// A wrong credential is refused by the remote; the error names no token and leaves no row.
#[test]
fn a_rejected_credential_fails_intake_without_echoing_the_token() {
    let r = rig_with(true, WRONG, None);
    let err = submit(&r).unwrap_err();
    assert!(matches!(err, BrokerError::PrefetchRejected(_)), "{err:?}");
    let shown = format!("{err:?} {err}");
    assert!(!shown.contains(WRONG) && !shown.contains(TOKEN), "{shown}");
    assert!(r.b.store().get(&key("r1")).unwrap().is_none());
    assert_eq!(remote_ref(&r), r.e.old);
}

/// Every credential use is audited under its own kind, tied to the request: the pre-approval
/// prefetch, the accepted approval, the push, and how the request ended.
#[test]
fn a_credentialed_request_leaves_a_full_audit_trail() {
    use mur_git_broker::broker::{
        AUDIT_APPROVAL_ACCEPTED, AUDIT_PREFETCH_WITH_CREDENTIAL, AUDIT_PUSH_WITH_CREDENTIAL,
        audit_ended,
    };
    let r = rig(true);
    let (k, h) = submit(&r).unwrap();
    assert_eq!(
        r.b.store().audit_trail(&k).unwrap(),
        vec![AUDIT_PREFETCH_WITH_CREDENTIAL]
    );
    r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert_eq!(
        r.b.store().audit_trail(&k).unwrap(),
        vec![
            AUDIT_PREFETCH_WITH_CREDENTIAL.to_string(),
            AUDIT_APPROVAL_ACCEPTED.to_string(),
            AUDIT_PUSH_WITH_CREDENTIAL.to_string(),
            audit_ended(State::Succeeded),
        ]
    );
}
