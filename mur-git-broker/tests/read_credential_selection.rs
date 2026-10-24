#![cfg(unix)]
//! Which secret each phase opens, with no network (spec #2093 §3, §7.1, §7.2, §7.5, §7.6).
//!
//! The enrollment names a loopback `http://` endpoint so the broker takes its network path
//! (`file://` opens nothing before approval). A fake `RemoteReader` stands in for the prefetch:
//! it records the login the broker would hand to git (the askpass environment) and then fetches
//! from the local bare remote with no auth. The push after approval does reach for the network
//! and fails; that is fine here, because the secret is opened and the audit row written before
//! git runs. `private_http_e2e.rs` covers the same ground against a real HTTP server.
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::credential_role::CredentialRole;
use mur_git_broker::credential_source::CredentialSource;
use mur_git_broker::{broker::*, error::BrokerError, import::RlimitSpawn, pending::*};
use mur_git_broker::{policy::BrokerLimits, prefetch::*, push::NoAuth, push::RemoteAuth};
use secrecy::SecretString;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
mod common;
use common::orch::*;
use common::*;

const WRITE_PATH: &str = "/secrets/write-token";
const READ_PATH: &str = "/secrets/read-token";
const WRITE_USER: &str = "bot";
const READ_USER: &str = "reader";
const URL: &str = "http://127.0.0.1:1/remote.git";
/// The env var the askpass wiring puts the login's username in, and the origin it is bound to.
const USER_ENV: &str = "GIT_CONFIG_VALUE_0";
const ORIGIN_ENV: &str = mur_git_broker::askpass::ENV_ORIGIN;

struct Recording(Arc<Mutex<Vec<String>>>);
impl CredentialSource for Recording {
    fn open(&self, p: &SecretPath) -> Result<SecretString, BrokerError> {
        self.0.lock().unwrap().push(p.as_str().to_string());
        Ok(SecretString::from(format!("tok-for-{}", p.as_str())))
    }
}

/// The username and origin the askpass wiring of `a` would hand to git.
fn login_of(a: &dyn RemoteAuth) -> (String, String) {
    let mut probe = Command::new("git");
    a.apply(&mut probe);
    let get = |k: &str| -> String {
        probe
            .get_envs()
            .find(|(n, _)| *n == std::ffi::OsStr::new(k))
            .and_then(|(_, v)| v.map(OsString::from))
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    (get(USER_ENV), get(ORIGIN_ENV))
}

/// Fetches from the local remote and remembers the username and origin of the auth it was given.
struct CapturingReader {
    inner: GitFetchReader,
    logins: Arc<Mutex<Vec<(String, String)>>>,
}
impl RemoteReader for CapturingReader {
    fn fetch(
        &self,
        r: &mur_git_broker::repo::PrivateRepo,
        s: &[String],
        b: &PrefetchBudget,
        a: &dyn RemoteAuth,
    ) -> Result<(), BrokerError> {
        self.logins.lock().unwrap().push(login_of(a));
        self.inner.fetch(r, s, b, &NoAuth)
    }
}

struct Rig {
    e: Env,
    b: Broker,
    opened: Arc<Mutex<Vec<String>>>,
    /// Prefetch-phase (username, origin) pairs.
    logins: Arc<Mutex<Vec<(String, String)>>>,
    /// Push-phase (username, origin) pairs, from the auth the push is about to run with.
    push_logins: Arc<Mutex<Vec<(String, String)>>>,
}

fn cred(path: &str, user: &str) -> Credential {
    Credential {
        secret_path: SecretPath::parse(path).unwrap(),
        username: Username::parse(user).unwrap(),
    }
}

fn rig(read: bool) -> Rig {
    let e = env();
    let mut p = policy();
    p.canonical_remote_endpoint = URL.into();
    let mut records = records_for(URL, &p);
    records.enrollments[0].credential = Some(cred(WRITE_PATH, WRITE_USER));
    records.enrollments[0].read_credential = read.then(|| cred(READ_PATH, READ_USER));
    *e.records.0.lock().unwrap() = Ok(Some(records));
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let logins: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let (remote, seen) = (e.remote.to_str().unwrap().to_string(), logins.clone());
    let mut b = Broker::new(BrokerConfig {
        store_path: e.t.path().join("pending.sqlite"),
        work_root: e.t.path().join("work-root"),
        limits: BrokerLimits::default(),
        git_bin: git_bin(),
        enrollments: Box::new(e.records.clone()),
        reader: Box::new(move |_url: &str| {
            Box::new(CapturingReader {
                inner: GitFetchReader {
                    remote_url: remote.clone(),
                    git_bin: git_bin(),
                },
                logins: seen.clone(),
            })
        }),
        spawn: Box::new(RlimitSpawn),
        auth: Box::new(NoAuth),
        askpass_bin: PathBuf::from("/nonexistent/askpass"),
        credentials: Box::new(Recording(opened.clone())),
        clock: e.clock.clone(),
    })
    .unwrap();
    let push_logins: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let sink = push_logins.clone();
    b.observe_push_auth(Box::new(move |a| sink.lock().unwrap().push(login_of(a))));
    Rig {
        e,
        b,
        opened,
        logins,
        push_logins,
    }
}

fn submit(r: &Rig) -> (RequestKey, String) {
    let mut d = action_for(&r.e.old, &r.e.new, "refs/heads/agent/x");
    d.request_id = "r1".into();
    let p =
        r.e.records
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .enrollments[0]
            .policy();
    d.canonical_remote_endpoint = URL.into();
    d.remote_policy_digest = p.remote_digest();
    d.ref_policy_digest = p.digest();
    d.enrollment_epoch = EPOCH;
    d.repo_identity = IDENTITY.into();
    let pack = range_pack(&r.e.work, &r.e.new, Some(&r.e.old), false);
    let k = key("r1");
    let h =
        r.b.submit_request(&k, &d, &pack)
            .expect("intake reads the local remote through the fake reader");
    (k, h)
}

fn opened(r: &Rig) -> Vec<String> {
    r.opened.lock().unwrap().clone()
}

/// Spec §7.1: with a read credential, intake never names the write secret; approval opens it
/// exactly once.
#[test]
fn with_a_read_credential_intake_never_opens_the_write_secret() {
    let r = rig(true);
    let (k, h) = submit(&r);
    assert_eq!(
        opened(&r),
        vec![READ_PATH],
        "write secret opened before approval"
    );
    let _ = r.b.on_approval(&k, proof("e1", "r1", &h)); // the push itself cannot connect here
    let all = opened(&r);
    assert_eq!(all, vec![READ_PATH, WRITE_PATH], "{all:?}");
    assert_eq!(all.iter().filter(|p| *p == WRITE_PATH).count(), 1);
}

/// Spec §7.2: no read credential, today's behavior: the one credential for both phases.
#[test]
fn without_a_read_credential_both_phases_open_the_one_credential() {
    let r = rig(false);
    let (k, h) = submit(&r);
    assert_eq!(opened(&r), vec![WRITE_PATH]);
    let _ = r.b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(opened(&r), vec![WRITE_PATH, WRITE_PATH]);
}

/// Spec §7.6 (prefetch phase): the username git is told matches the secret that was opened.
#[test]
fn the_prefetch_login_username_belongs_to_the_secret_opened() {
    for (read, user) in [(true, READ_USER), (false, WRITE_USER)] {
        let r = rig(read);
        submit(&r);
        let logins = r.logins.lock().unwrap().clone();
        assert!(!logins.is_empty());
        for (u, origin) in &logins {
            assert_eq!(u, user, "read={read}");
            assert_eq!(origin, &format!("http://{user}@127.0.0.1:1"), "read={read}");
        }
    }
}

/// Spec §7.6 (both phases): with a split enrollment the prefetch is logged in as the read login
/// and the push as the write login, each matching the secret opened for it; with one credential
/// both phases use the same username.
#[test]
fn askpass_username_matches_the_opened_secret_in_each_phase() {
    let origin = |u: &str| format!("http://{u}@127.0.0.1:1");
    let pairs = |m: &Arc<Mutex<Vec<(String, String)>>>| m.lock().unwrap().clone();

    let r = rig(true);
    let (k, h) = submit(&r);
    let _ = r.b.on_approval(&k, proof("e1", "r1", &h)); // the push itself cannot connect here
    assert_eq!(opened(&r), vec![READ_PATH, WRITE_PATH]);
    assert_eq!(
        pairs(&r.logins),
        vec![(READ_USER.into(), origin(READ_USER))]
    );
    assert_eq!(
        pairs(&r.push_logins),
        vec![(WRITE_USER.into(), origin(WRITE_USER))]
    );

    let r = rig(false);
    let (k, h) = submit(&r);
    let _ = r.b.on_approval(&k, proof("e1", "r1", &h));
    assert_eq!(opened(&r), vec![WRITE_PATH, WRITE_PATH]);
    assert_eq!(
        pairs(&r.logins),
        vec![(WRITE_USER.into(), origin(WRITE_USER))]
    );
    assert_eq!(pairs(&r.push_logins), pairs(&r.logins));
}

/// Spec §7.5: the audit row carries `read` / `write` / `shared`.
#[test]
fn the_audit_rows_carry_the_selected_role() {
    let credentialed = |r: &Rig, k: &RequestKey| -> Vec<(String, Option<CredentialRole>)> {
        r.b.store()
            .audit_events(k)
            .unwrap()
            .into_iter()
            .filter(|e| e.credential.is_some())
            .map(|e| (e.kind, e.credential))
            .collect()
    };
    for (read, first, second) in [
        (true, CredentialRole::Read, CredentialRole::Write),
        (false, CredentialRole::Shared, CredentialRole::Shared),
    ] {
        let r = rig(read);
        let (k, h) = submit(&r);
        assert_eq!(
            credentialed(&r, &k),
            vec![(AUDIT_PREFETCH_WITH_CREDENTIAL.to_string(), Some(first))]
        );
        let _ = r.b.on_approval(&k, proof("e1", "r1", &h));
        assert_eq!(
            credentialed(&r, &k),
            vec![
                (AUDIT_PREFETCH_WITH_CREDENTIAL.to_string(), Some(first)),
                (AUDIT_PUSH_WITH_CREDENTIAL.to_string(), Some(second)),
            ],
            "read={read}"
        );
        // Kind-only readers are unchanged.
        assert_eq!(
            r.b.store().audit_trail(&k).unwrap()[0],
            AUDIT_PREFETCH_WITH_CREDENTIAL
        );
    }
}

/// A `file://` endpoint opens nothing before approval, split or not (spec §3).
#[test]
fn a_file_endpoint_opens_nothing_before_approval_even_with_a_read_credential() {
    let e = env();
    e.enroll(policy());
    {
        let mut g = e.records.0.lock().unwrap();
        let rec = &mut g.as_mut().unwrap().as_mut().unwrap().enrollments[0];
        rec.credential = Some(cred(WRITE_PATH, WRITE_USER));
        rec.read_credential = Some(cred(READ_PATH, READ_USER));
    }
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let b = broker_with_credentials(&e, Box::new(Recording(opened.clone())));
    let mut d = action_for(&e.old, &e.new, "refs/heads/agent/x");
    d.request_id = "r1".into();
    let p = e
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
    d.canonical_remote_endpoint = p.canonical_remote_endpoint.clone();
    d.remote_policy_digest = p.remote_digest();
    d.ref_policy_digest = p.digest();
    d.enrollment_epoch = EPOCH;
    d.repo_identity = IDENTITY.into();
    let pack = range_pack(&e.work, &e.new, Some(&e.old), false);
    let k = key("r1");
    let h = b.submit_request(&k, &d, &pack).unwrap();
    assert!(opened.lock().unwrap().is_empty());
    assert_eq!(
        b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(*opened.lock().unwrap(), vec![WRITE_PATH]);
}
