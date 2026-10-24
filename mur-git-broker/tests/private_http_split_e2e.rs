#![cfg(unix)]
//! `private_http_e2e.rs` for a *split* enrollment (spec #2093): a read credential
//! (`reader:READ_TOKEN`) for the pre-approval prefetch and a write credential
//! (`bot:WRITE_TOKEN`) for the push, against the same real `git-http-backend`.
//!
//! The server records which login each request carried, so these cases check the wire, not just
//! the broker's own bookkeeping: the prefetch never shows the write login, the push never shows
//! the read login, and neither token ever travels in a request line.
use mur_git_broker::credential_role::CredentialRole;
use mur_git_broker::{broker::*, error::BrokerError, pending::*};
mod common;
#[path = "common/http.rs"]
mod http;
use common::orch::*;
use http::*;

fn kinds_and_roles(r: &Rig, k: &RequestKey) -> Vec<(String, Option<CredentialRole>)> {
    r.b.store()
        .audit_events(k)
        .unwrap()
        .into_iter()
        .map(|e| (e.kind, e.credential))
        .collect()
}

/// Intake reads with the read login only; after approval the push writes with the write login
/// only; the write token is never on the wire during the prefetch.
#[test]
fn a_split_enrollment_prefetches_with_the_read_login_and_pushes_with_the_write_login() {
    let r = rig_split(true, READ_TOKEN, WRITE_TOKEN, None);
    let (k, h) = match submit(&r) {
        Ok(v) => v,
        Err(e) => panic!(
            "intake failed on a split-credential private remote: {e:?}; server saw {:#?}",
            r.seen.lock().unwrap()
        ),
    };
    assert_eq!(
        remote_ref(&r),
        r.e.old,
        "nothing is written before approval"
    );
    {
        let seen = r.seen.lock().unwrap();
        let prefetch: Vec<_> = seen.iter().filter(|s| s.leg == Leg::Prefetch).collect();
        assert!(!prefetch.is_empty(), "{seen:#?}");
        assert!(
            seen.iter().all(|s| s.leg == Leg::Prefetch),
            "the push ran before approval: {seen:#?}"
        );
        assert!(
            prefetch.iter().any(|s| s.carried == Carried::Read),
            "the read login was never offered: {seen:#?}"
        );
        assert!(
            prefetch
                .iter()
                .all(|s| matches!(s.carried, Carried::Nothing | Carried::Read)),
            "the prefetch carried something other than the read login: {seen:#?}"
        );
    }
    assert_eq!(
        r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(remote_ref(&r), r.e.new, "the push moved the remote ref");
    let seen = r.seen.lock().unwrap();
    let upload: Vec<_> = seen
        .iter()
        .filter(|s| s.line.contains("git-upload-pack"))
        .collect();
    let receive: Vec<_> = seen
        .iter()
        .filter(|s| s.line.contains("git-receive-pack"))
        .collect();
    assert!(!upload.is_empty() && !receive.is_empty(), "{seen:#?}");
    assert!(
        upload.iter().all(|s| s.leg == Leg::Prefetch)
            && upload
                .iter()
                .all(|s| matches!(s.carried, Carried::Nothing | Carried::Read)),
        "every upload-pack request authenticates with the read login only: {seen:#?}"
    );
    assert!(
        receive.iter().all(|s| s.leg == Leg::Push)
            && receive
                .iter()
                .all(|s| matches!(s.carried, Carried::Nothing | Carried::Write))
            && receive.iter().any(|s| s.carried == Carried::Write),
        "every receive-pack request authenticates with the write login only: {seen:#?}"
    );
    assert!(seen.iter().any(|s| s.authed), "{seen:#?}");
    assert!(
        seen.iter().all(|s| !s.leaked_token_in_line),
        "a token travelled in a request line: {seen:#?}"
    );
}

/// A public remote never asks, so git never runs the askpass: neither token is sent, even though
/// the enrollment carries two.
#[test]
fn an_anonymous_readable_remote_is_never_sent_either_split_token() {
    let r = rig_split(false, READ_TOKEN, WRITE_TOKEN, None);
    let (k, h) = submit(&r).unwrap();
    r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert_eq!(remote_ref(&r), r.e.new);
    let seen = r.seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|s| s.carried == Carried::Nothing),
        "{seen:#?}"
    );
    assert!(seen.iter().all(|s| !s.leaked_token_in_line), "{seen:#?}");
}

/// The prefetch runs before approval, so a redirect there must not carry either token anywhere:
/// git is told not to follow it, the foreign host sees nothing, and the request fails closed.
#[test]
fn a_split_prefetch_redirect_is_not_followed_and_never_carries_either_token() {
    let t = tempfile::tempdir().unwrap();
    let (evil_port, evil_seen) = serve(t.path().to_path_buf(), false, Logins::split());
    let (port, redirect_seen) = redirector(format!(
        "http://127.0.0.1:{evil_port}/remote.git/info/refs?service=git-upload-pack"
    ));
    let r = rig_split(
        true,
        READ_TOKEN,
        WRITE_TOKEN,
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
        "a token was sent unprompted"
    );
}

/// A rejected read credential is refused by the remote; neither token is echoed in the error, the
/// write login never reaches the wire, and no row is left.
#[test]
fn a_rejected_read_credential_fails_intake_without_echoing_either_token() {
    let r = rig_split(true, WRONG, WRITE_TOKEN, None);
    let err = submit(&r).unwrap_err();
    assert!(matches!(err, BrokerError::PrefetchRejected(_)), "{err:?}");
    let shown = format!("{err:?} {err}");
    for secret in [WRONG, READ_TOKEN, WRITE_TOKEN] {
        assert!(!shown.contains(secret), "{shown}");
    }
    {
        let seen = r.seen.lock().unwrap();
        assert!(
            seen.iter()
                .all(|s| s.leg == Leg::Prefetch && s.carried != Carried::Write),
            "the write login reached the wire during a failed prefetch: {seen:#?}"
        );
        assert!(seen.iter().all(|s| !s.leaked_token_in_line), "{seen:#?}");
    }
    assert!(r.b.store().get(&key("r1")).unwrap().is_none());
    assert_eq!(remote_ref(&r), r.e.old);
}

/// The audit trail names the selected role: read for the prefetch, write for the push.
#[test]
fn a_split_request_audits_the_read_role_for_prefetch_and_the_write_role_for_push() {
    let r = rig_split(true, READ_TOKEN, WRITE_TOKEN, None);
    let (k, h) = submit(&r).unwrap();
    assert_eq!(
        kinds_and_roles(&r, &k),
        vec![(
            AUDIT_PREFETCH_WITH_CREDENTIAL.to_string(),
            Some(CredentialRole::Read)
        )]
    );
    r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert_eq!(
        kinds_and_roles(&r, &k),
        vec![
            (
                AUDIT_PREFETCH_WITH_CREDENTIAL.to_string(),
                Some(CredentialRole::Read)
            ),
            (AUDIT_APPROVAL_ACCEPTED.to_string(), None),
            (
                AUDIT_PUSH_WITH_CREDENTIAL.to_string(),
                Some(CredentialRole::Write)
            ),
            (audit_ended(State::Succeeded), None),
        ]
    );
}
