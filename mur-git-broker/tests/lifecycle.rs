#![cfg(unix)]
//! T10: approval, consume-once, push, orchestration. Real git, a fake clock.
use chrono::{DateTime, Duration, TimeZone, Utc};
use mur_git_broker::{
    approval::*, error::BrokerError, oid::*, pending::*, policy::BrokerLimits, push::*,
    repo::PrivateRepo,
};
use std::process::Command;
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

// ---- push -------------------------------------------------------------------------------------
struct Fx {
    t: tempfile::TempDir,
    work: std::path::PathBuf,
    remote: std::path::PathBuf,
    repo: PrivateRepo,
    old: String,
    new: String,
}
/// OLD is on the remote's agent/x; NEW (its child) sits in a frozen private repo.
fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let old = commit(&work, "old");
    let remote = bare_remote(t.path());
    let url = remote.to_str().unwrap();
    git(
        &work,
        &["push", "-q", url, &format!("{old}:refs/heads/agent/x")],
    );
    let new = commit(&work, "new");
    let repo = PrivateRepo::create(&t.path().join("p"), ObjectFormat::Sha1, &git_bin()).unwrap();
    let w = work.to_str().unwrap();
    let spec = "+refs/heads/main:refs/prefetch/all";
    let o = repo
        .runner()
        .run(
            &["fetch", "-q", "--no-tags", w, spec],
            std::time::Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(o.code, 0);
    repo.freeze().unwrap();
    Fx {
        t,
        work,
        remote,
        repo,
        old,
        new,
    }
}
fn url(f: &Fx) -> String {
    f.remote.to_str().unwrap().to_string()
}
fn remote_ref(f: &Fx, r: &str) -> Option<String> {
    let o = std::process::Command::new(git_bin())
        .current_dir(&f.remote)
        .args(["rev-parse", "-q", "--verify", r])
        .output()
        .unwrap();
    o.status
        .success()
        .then(|| String::from_utf8(o.stdout).unwrap().trim().to_string())
}
fn frozen(f: &Fx) -> String {
    f.repo.control_digest().unwrap()
}
fn lim() -> BrokerLimits {
    BrokerLimits::default()
}

#[test]
fn update_push_lands_and_remote_moves() {
    let f = fx();
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    push(&f.repo, &a, &url(&f), &NoAuth, &lim(), &frozen(&f)).unwrap();
    assert_eq!(remote_ref(&f, "refs/heads/agent/x"), Some(f.new.clone()));
}

#[test]
fn creation_push_lands_with_empty_lease() {
    let f = fx();
    let a = action_for(
        &zero_oid(ObjectFormat::Sha1),
        &f.new,
        "refs/heads/agent/fresh",
    );
    push(&f.repo, &a, &url(&f), &NoAuth, &lim(), &frozen(&f)).unwrap();
    assert_eq!(
        remote_ref(&f, "refs/heads/agent/fresh"),
        Some(f.new.clone())
    );
}

#[test]
fn toctou_remote_moved_after_approval_is_stale_old_sha() {
    let f = fx();
    git(&f.work, &["checkout", "-q", "-b", "other", &f.old]);
    let moved = commit(&f.work, "someone else");
    git(
        &f.work,
        &[
            "push",
            "-q",
            "-f",
            &url(&f),
            &format!("{moved}:refs/heads/agent/x"),
        ],
    );
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    let e = push(&f.repo, &a, &url(&f), &NoAuth, &lim(), &frozen(&f)).unwrap_err();
    assert_eq!(e, BrokerError::StaleOldSha);
    assert_eq!(
        remote_ref(&f, "refs/heads/agent/x"),
        Some(moved),
        "remote untouched"
    );
}

#[test]
fn creation_race_is_stale_old_sha() {
    let f = fx();
    let a = action_for(&zero_oid(ObjectFormat::Sha1), &f.new, "refs/heads/agent/x");
    let e = push(&f.repo, &a, &url(&f), &NoAuth, &lim(), &frozen(&f)).unwrap_err();
    assert_eq!(e, BrokerError::StaleOldSha);
}

#[test]
fn new_sha_mismatch_before_push_is_outcome_unknown_with_no_push() {
    let f = fx();
    let a = action_for(&f.old, &"e".repeat(40), "refs/heads/agent/x");
    let r = push(&f.repo, &a, &url(&f), &NoAuth, &lim(), &frozen(&f));
    assert!(matches!(r, Err(BrokerError::OutcomeUnknown(_))), "{r:?}");
    assert_eq!(remote_ref(&f, "refs/heads/agent/x"), Some(f.old.clone()));
}

#[test]
fn porcelain_parse_table() {
    let line = |flag: &str, tail: &str| {
        format!(
            "To remote\n{flag}\t{}:refs/heads/agent/x\t{tail}\nDone\n",
            "a".repeat(40)
        )
    };
    let rr = || Err(BrokerError::Rejected("remote_rejected".into()));
    let cases: Vec<(i32, String, Result<(), BrokerError>)> = vec![
        (0, line("*", "[new branch]"), Ok(())),
        (0, line("+", "a..b (forced update)"), Ok(())),
        (0, line(" ", "a..b"), Ok(())),
        (0, line("=", "[up to date]"), Ok(())),
        (
            1,
            line("!", "[rejected] (stale info)"),
            Err(BrokerError::StaleOldSha),
        ),
        (1, line("!", "[remote rejected] (non-fast-forward)"), rr()),
        (
            1,
            line("!", "[remote rejected] (pre-receive hook declined)"),
            rr(),
        ),
        (
            128,
            String::new(),
            Err(BrokerError::Rejected("git_exit_128".into())),
        ),
        (
            1,
            String::new(),
            Err(BrokerError::Rejected("git_exit_1".into())),
        ),
    ];
    for (code, out, want) in cases {
        assert_eq!(parse_push_outcome(code, &out), want, "{code} {out:?}");
    }
}

#[test]
fn control_digest_mismatch_before_push_is_outcome_unknown_with_no_push() {
    let f = fx();
    let frozen_at_submit = frozen(&f);
    let c = f.repo.path().join("config");
    let mut perm = std::fs::metadata(&c).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o644);
    std::fs::set_permissions(&c, perm).unwrap();
    let mut s = std::fs::read_to_string(&c).unwrap();
    s.push_str("[url \"/evil\"]\n\tinsteadOf = x\n");
    std::fs::write(&c, s).unwrap();
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    let r = push(&f.repo, &a, &url(&f), &NoAuth, &lim(), &frozen_at_submit);
    assert!(matches!(r, Err(BrokerError::OutcomeUnknown(_))), "{r:?}");
    assert_eq!(remote_ref(&f, "refs/heads/agent/x"), Some(f.old.clone()));
}

#[test]
fn git_killed_mid_push_is_outcome_unknown_and_never_retried() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let hang = f.t.path().join("hang.sh");
    std::fs::write(&hang, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).unwrap();
    let can_exec = Command::new(&hang)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .spawn()
        .and_then(|mut c| {
            let _ = c.kill();
            c.wait()
        })
        .is_ok();
    if !can_exec {
        assert!(hook_skip_allowed(), "cannot exec scripts here");
        eprintln!("SKIP: cannot exec scripts here");
        return;
    }
    struct Hang(std::path::PathBuf);
    impl RemoteAuth for Hang {
        fn apply(&self, c: &mut Command) {
            c.env("GIT_SSH_COMMAND", &self.0);
        }
    }
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    let l = BrokerLimits {
        push_timeout_secs: 1,
        ..lim()
    };
    let start = std::time::Instant::now();
    let r = push(
        &f.repo,
        &a,
        "ssh://example.invalid/x",
        &Hang(hang),
        &l,
        &frozen(&f),
    );
    assert!(matches!(r, Err(BrokerError::OutcomeUnknown(_))), "{r:?}");
    assert!(start.elapsed().as_secs() < 10);
}
