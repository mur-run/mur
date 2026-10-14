//! Orchestration harness shared by the broker lifecycle and enrollment-binding tests: a fake
//! clock, a settable stand-in for `enrollments.yaml`, and a broker built on both.
#![allow(dead_code)]
use super::*;
use chrono::{DateTime, Duration, TimeZone, Utc};
use mur_git_broker::binding::EnrollmentSource;
use mur_git_broker::credential::SecretPath;
use mur_git_broker::credential_source::CredentialSource;
use mur_git_broker::enrollment::{Endpoint, EnrollmentRecord, Enrollments, RepoRecord};
use mur_git_broker::{
    approval::*, error::BrokerError, pending::*, policy::BrokerLimits, push::*, repo::PrivateRepo,
};
use mur_git_broker::{broker::*, import::RlimitSpawn, policy::RemotePolicy, prefetch::*};
use secrecy::SecretString;
use std::sync::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub struct FakeClock(Mutex<DateTime<Utc>>);
impl FakeClock {
    pub fn new() -> Self {
        Self(Mutex::new(
            Utc.with_ymd_and_hms(2026, 10, 7, 0, 0, 0).unwrap(),
        ))
    }
    pub fn advance(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
    }
}
impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

pub fn key(req: &str) -> RequestKey {
    RequestKey {
        agent_id: "alice".into(),
        task_id: "t1".into(),
        request_id: req.into(),
    }
}
pub fn proof(ev: &str, req: &str, h: &str) -> ApprovalProof {
    ApprovalProof {
        event_id: ev.into(),
        request_id: req.into(),
        action_hash: h.into(),
    }
}
pub fn state(s: &PendingStore, k: &RequestKey) -> State {
    s.get(k).unwrap().unwrap().state
}

// ---- orchestration ----------------------------------------------------------------------------
pub struct Env {
    pub t: tempfile::TempDir,
    pub work: std::path::PathBuf,
    pub remote: std::path::PathBuf,
    pub old: String,
    pub new: String,
    pub clock: Arc<FakeClock>,
    pub fetches: Arc<AtomicUsize>,
    pub records: Records,
}
/// What the enrollment file would say, shared with every broker built on this `Env` so a test
/// can change it between a submit and an approval. `None` models a missing file, `Err` an
/// unreadable one.
#[derive(Clone)]
pub struct Records(
    pub Arc<Mutex<Result<Option<Enrollments>, ()>>>,
    pub Arc<AtomicUsize>,
);
impl EnrollmentSource for Records {
    fn current(&self) -> Result<Option<Enrollments>, BrokerError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        match &*self.0.lock().unwrap() {
            Ok(v) => Ok(v.clone()),
            Err(()) => Err(BrokerError::EnrollmentUnavailable),
        }
    }
}
pub const EPOCH: u64 = 1;
pub const IDENTITY: &str = "id";
pub fn records_for(endpoint: &str, p: &RemotePolicy) -> Enrollments {
    let mut e = Enrollments::empty(EPOCH + 1);
    e.repos.insert(
        "repo".into(),
        RepoRecord {
            identity: IDENTITY.into(),
            path: "/unused".into(),
            object_format: ObjectFormat::Sha1,
        },
    );
    e.enrollments.push(EnrollmentRecord {
        agent: "alice".into(),
        repo_id: "repo".into(),
        remote_id: p.remote_id.clone(),
        endpoint: Endpoint::parse(endpoint).unwrap(),
        ref_prefix: p.ref_prefix.clone(),
        creation_base_refs: p.creation_base_refs.clone(),
        credential: None,
        epoch: EPOCH,
    });
    e
}
impl Env {
    pub fn endpoint(&self) -> String {
        format!("file://{}", self.remote.to_str().unwrap())
    }
    /// `p` as the records carry it: same rules, this test's real remote.
    pub fn fit(&self, mut p: RemotePolicy) -> RemotePolicy {
        p.canonical_remote_endpoint = self.endpoint();
        p
    }
    pub fn enroll(&self, p: RemotePolicy) {
        let p = self.fit(p);
        *self.records.0.lock().unwrap() = Ok(Some(records_for(&p.canonical_remote_endpoint, &p)));
    }
    pub fn reads(&self) -> usize {
        self.records.1.load(Ordering::SeqCst)
    }
}
/// Counting wrapper so "no re-prefetch" is observable.
pub struct CountingReader {
    inner: GitFetchReader,
    n: Arc<AtomicUsize>,
}
impl RemoteReader for CountingReader {
    fn fetch(&self, r: &PrivateRepo, s: &[String], b: &PrefetchBudget) -> Result<(), BrokerError> {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch(r, s, b)
    }
}
pub fn env() -> Env {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let old = commit(&work, "old");
    let remote = bare_remote(t.path());
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:refs/heads/agent/x"),
        ],
    );
    let new = commit(&work, "new");
    Env {
        t,
        work,
        remote,
        old,
        new,
        clock: Arc::new(FakeClock::new()),
        fetches: Arc::new(AtomicUsize::new(0)),
        records: Records(
            Arc::new(Mutex::new(Ok(None))),
            Arc::new(AtomicUsize::new(0)),
        ),
    }
}
pub fn broker(e: &Env, p: RemotePolicy) -> Broker {
    e.enroll(p);
    broker_on_current_records(e)
}
pub fn broker_on_current_records(e: &Env) -> Broker {
    broker_with_credentials(e, Box::new(NoCredentials))
}
/// For tests whose enrollments carry no credential: asking for one is a bug in the broker.
pub struct NoCredentials;
impl CredentialSource for NoCredentials {
    fn open(&self, _: &SecretPath) -> Result<SecretString, BrokerError> {
        panic!("credential requested for a binding that has none")
    }
}
pub fn broker_with_credentials(e: &Env, credentials: Box<dyn CredentialSource>) -> Broker {
    let n = e.fetches.clone();
    Broker::new(BrokerConfig {
        store_path: e.t.path().join("pending.sqlite"),
        work_root: e.t.path().join("work-root"),
        limits: BrokerLimits::default(),
        git_bin: git_bin(),
        enrollments: Box::new(e.records.clone()),
        reader: Box::new(move |url: &str| {
            Box::new(CountingReader {
                inner: GitFetchReader {
                    remote_url: url.strip_prefix("file://").unwrap().to_string(),
                    git_bin: git_bin(),
                },
                n: n.clone(),
            })
        }),
        spawn: Box::new(RlimitSpawn),
        auth: Box::new(NoAuth),
        askpass_bin: std::path::PathBuf::from("/nonexistent/askpass"),
        credentials,
        clock: e.clock.clone(),
    })
    .unwrap()
}
pub fn req(
    e: &Env,
    id: &str,
) -> (
    RequestKey,
    mur_git_broker::action::ActionDocument,
    std::path::PathBuf,
) {
    let mut d = action_for(&e.old, &e.new, "refs/heads/agent/x");
    d.request_id = id.into();
    pin(e, &mut d, &policy());
    (key(id), d, range_pack(&e.work, &e.new, Some(&e.old), false))
}
/// Make `d` carry what the records say about `p`, as intake would stamp it.
pub fn pin(e: &Env, d: &mut mur_git_broker::action::ActionDocument, p: &RemotePolicy) {
    let p = e.fit(p.clone());
    d.canonical_remote_endpoint = p.canonical_remote_endpoint.clone();
    d.remote_policy_digest = p.remote_digest();
    d.ref_policy_digest = p.digest();
    d.enrollment_epoch = EPOCH;
    d.repo_identity = IDENTITY.into();
}
pub fn leftovers(e: &Env) -> usize {
    std::fs::read_dir(e.t.path().join("work-root"))
        .map(|d| d.count())
        .unwrap_or(0)
}
