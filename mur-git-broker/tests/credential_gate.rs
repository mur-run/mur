#![cfg(unix)]
//! Where `on_approval` reads the credential: after the binding is re-read and before the approval
//! is spent. A fake `CredentialSource` records every call, so the order is observable.
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::credential_source::CredentialSource;
use mur_git_broker::enrollment::Enrollments;
use mur_git_broker::{broker::*, error::BrokerError, pending::*};
use secrecy::SecretString;
use std::sync::{Arc, Mutex};
mod common;
use common::orch::*;
use common::*;

const SECRET_PATH: &str = "/secrets/deploy-token";

/// Answers `open` from a settable result and remembers which paths were asked for.
struct Fake {
    answer: Mutex<Result<(), &'static str>>,
    calls: Mutex<Vec<String>>,
}
impl Fake {
    fn new(answer: Result<(), &'static str>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(answer),
            calls: Mutex::new(Vec::new()),
        })
    }
    fn set(&self, answer: Result<(), &'static str>) {
        *self.answer.lock().unwrap() = answer;
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
struct Handle(Arc<Fake>);
impl CredentialSource for Handle {
    fn open(&self, p: &SecretPath) -> Result<SecretString, BrokerError> {
        self.0.calls.lock().unwrap().push(p.as_str().to_string());
        match *self.0.answer.lock().unwrap() {
            Ok(()) => Ok(SecretString::from("tok".to_string())),
            Err(why) => Err(BrokerError::CredentialUnreadable(why)),
        }
    }
}

/// An env whose one enrollment carries a credential, and a broker reading through `fake`.
fn with_credential(fake: &Arc<Fake>) -> (Env, Broker) {
    let e = env();
    e.enroll(policy());
    set_records(&e, |recs| {
        recs.enrollments[0].credential = Some(Credential {
            secret_path: SecretPath::parse(SECRET_PATH).unwrap(),
            username: Username::parse("bot").unwrap(),
        });
    });
    let b = broker_with_credentials(&e, Box::new(Handle(fake.clone())));
    (e, b)
}
fn set_records(e: &Env, f: impl FnOnce(&mut Enrollments)) {
    let mut g = e.records.0.lock().unwrap();
    f(g.as_mut().unwrap().as_mut().unwrap());
}
/// A request stamped from what the records say now, as intake does: the digests cover the
/// credential, so a document pinned from the bare `policy()` no longer matches a record that
/// carries one.
fn request(
    e: &Env,
    id: &str,
) -> (
    RequestKey,
    mur_git_broker::action::ActionDocument,
    std::path::PathBuf,
) {
    let (k, mut d, pack) = req(e, id);
    let current = e
        .records
        .0
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .enrollments[0]
        .policy();
    pin(e, &mut d, &current);
    (k, d, pack)
}
fn pended(e: &Env, b: &Broker) -> (RequestKey, String) {
    let (k, d, pack) = request(e, "r1");
    let h = b.submit_request(&k, &d, &pack).unwrap();
    (k, h)
}

#[test]
fn a_binding_that_no_longer_holds_never_opens_the_credential() {
    let fake = Fake::new(Ok(()));
    let (e, b) = with_credential(&fake);
    let (k, h) = pended(&e, &b);
    *e.records.0.lock().unwrap() = Ok(Some(Enrollments::empty(EPOCH + 1)));
    let r = b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(r.unwrap_err(), BrokerError::PolicyChanged);
    assert_eq!(state(b.store(), &k), State::PolicyChanged);
    assert!(
        fake.calls().is_empty(),
        "the secret was read for a void request"
    );
}

#[test]
fn unreadable_records_never_open_the_credential() {
    let fake = Fake::new(Ok(()));
    let (e, b) = with_credential(&fake);
    let (k, h) = pended(&e, &b);
    *e.records.0.lock().unwrap() = Err(());
    let r = b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(r.unwrap_err(), BrokerError::EnrollmentUnavailable);
    assert!(fake.calls().is_empty());
}

#[test]
fn an_unreadable_credential_leaves_the_request_pending_and_the_approval_unspent() {
    let fake = Fake::new(Err("bad_mode"));
    let (e, b) = with_credential(&fake);
    let (k, h) = pended(&e, &b);
    let r = b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(
        r.unwrap_err(),
        BrokerError::CredentialUnreadable("bad_mode")
    );
    assert_eq!(
        state(b.store(), &k),
        State::PendingApproval,
        "still waiting"
    );
    assert_eq!(git(&e.remote, &["rev-parse", "refs/heads/agent/x"]), e.old);
    // Unspent means the very same approval event works once the file is fixed.
    fake.set(Ok(()));
    assert_eq!(
        b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded,
        "the first attempt never reached accept, so event e1 is still unused"
    );
    assert_eq!(fake.calls(), [SECRET_PATH, SECRET_PATH]);
    assert_eq!(git(&e.remote, &["rev-parse", "refs/heads/agent/x"]), e.new);
}

#[test]
fn a_readable_credential_is_opened_once_for_the_recorded_path_and_the_push_proceeds() {
    let fake = Fake::new(Ok(()));
    let (e, b) = with_credential(&fake);
    let (k, h) = pended(&e, &b);
    assert_eq!(
        b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(
        fake.calls(),
        [SECRET_PATH],
        "exactly one read, of the enrolled path"
    );
    assert_eq!(git(&e.remote, &["rev-parse", "refs/heads/agent/x"]), e.new);
}

#[test]
fn a_binding_without_a_credential_opens_nothing() {
    let e = env();
    let b = broker(&e, policy()); // NoCredentials panics if asked.
    let (k, h) = pended(&e, &b);
    assert_eq!(
        b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
}

#[test]
fn a_failed_accept_after_a_successful_open_leaves_the_request_pending() {
    let fake = Fake::new(Ok(()));
    let (e, b) = with_credential(&fake);
    let (k, h) = pended(&e, &b);
    // Burn event id "e1" on another request, so accepting r1 with it fails inside the store.
    let (k2, d2, pack2) = {
        let (mut k2, mut d2, pack2) = request(&e, "r2");
        k2.request_id = "r2".into();
        d2.request_id = "r2".into();
        (k2, d2, pack2)
    };
    let h2 = b.submit_request(&k2, &d2, &pack2).unwrap();
    b.on_approval(&k2, proof("e1", "r2", &h2)).unwrap();
    let before = fake.calls().len();
    let r = b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(r.unwrap_err(), BrokerError::NotPending);
    assert_eq!(
        fake.calls().len(),
        before + 1,
        "the secret was opened, then the accept failed"
    );
    assert_eq!(state(b.store(), &k), State::PendingApproval);
    assert_eq!(
        git(&e.remote, &["rev-parse", "refs/heads/agent/x"]),
        e.new,
        "only r2 pushed"
    );
}
