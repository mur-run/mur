//! #1859: gate B must notice that a record it once wrote has disappeared, and
//! stop trusting remembered approvals instead of falling back to whatever
//! older allow survived. Every test here drives the real store, the real
//! signature and the real files; "deleting" is done on `events.jsonl` itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mur_common::agent::{HITL_CHANNEL_FILE, HITL_HEAD_DIR};
use mur_common::hitl::HitlResponse;
use mur_common::identity::AgentIdentity;

use super::store::{ChannelDecisionStore, DecisionStore, Settled};

const AGENT: &str = "qa";

fn store(home: &Path) -> ChannelDecisionStore {
    ChannelDecisionStore::new(
        home.to_path_buf(),
        AGENT.into(),
        Arc::new(AgentIdentity::generate()),
        0,
    )
}

fn resp(hash: &str, allow: bool, id: &str, age_secs: i64) -> HitlResponse {
    HitlResponse {
        hitl_id: id.into(),
        action_hash: hash.into(),
        allow,
        reason: String::new(),
        surface: "hub".into(),
        issued_at: Some(chrono::Utc::now() - chrono::Duration::seconds(age_secs)),
        label: None,
    }
}

fn agent_home(home: &Path) -> PathBuf {
    home.join("agents").join(AGENT)
}

fn channel_id(home: &Path) -> String {
    std::fs::read_to_string(agent_home(home).join(HITL_CHANNEL_FILE))
        .unwrap()
        .trim()
        .to_string()
}

fn log_path(home: &Path) -> PathBuf {
    home.join("channels")
        .join(channel_id(home))
        .join("events.jsonl")
}

fn heads_dir(home: &Path) -> PathBuf {
    agent_home(home).join(HITL_HEAD_DIR)
}

/// Remove the last line of the log, as a deleted record would.
fn drop_last_line(home: &Path) {
    let p = log_path(home);
    let text = std::fs::read_to_string(&p).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.pop().expect("log has a line to drop");
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&p, out).unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

// --- the two cases the issue names -------------------------------------

#[tokio::test]
async fn deleting_a_revocation_note_does_not_revive_the_decision() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
    s.forget(None).unwrap();
    assert_eq!(s.lookup("h1").await, None, "revoked");

    drop_last_line(tmp.path()); // the revocation Note

    assert_eq!(
        s.lookup("h1").await,
        None,
        "a deleted revocation must not bring the old allow back"
    );
}

#[tokio::test]
async fn deleting_a_newer_deny_does_not_revive_the_older_allow() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 30)).await;
    s.record(resp("h1", false, "b", 0)).await;
    assert_eq!(s.lookup("h1").await, Some(Settled::Deny));

    drop_last_line(tmp.path()); // the newer deny

    assert_eq!(
        s.lookup("h1").await,
        None,
        "with the deny gone the older allow must not win"
    );
}

// --- "written then gone" vs "never written" ------------------------------

#[tokio::test]
async fn a_log_that_never_had_a_record_is_not_a_tamper_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    assert_eq!(s.lookup("h1").await, None);
    assert!(s.list().unwrap().is_empty(), "empty is not an error");
    s.record(resp("h1", true, "a", 0)).await;
    assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
}

#[tokio::test]
async fn honest_use_keeps_working_after_a_forget() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    s.forget(None).unwrap();
    s.record(resp("h1", true, "b", 0)).await;
    assert_eq!(
        s.lookup("h1").await,
        Some(Settled::Allow),
        "a decision recorded after the revocation stands"
    );
    assert_eq!(s.list().unwrap().len(), 1);
}

// --- the other ways a record can disappear -----------------------------

#[tokio::test]
async fn tail_truncation_mid_line_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 30)).await;
    s.record(resp("h1", false, "b", 0)).await;
    let p = log_path(tmp.path());
    let len = std::fs::metadata(&p).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(len - 20)
        .unwrap();
    assert_eq!(s.lookup("h1").await, None);
}

#[tokio::test]
async fn deleting_the_whole_log_file_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 0)).await;
    std::fs::remove_file(log_path(tmp.path())).unwrap();
    assert_eq!(s.lookup("h1").await, None);
}

#[tokio::test]
async fn deleting_a_record_from_the_middle_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 30)).await;
    s.record(resp("h2", true, "b", 20)).await;
    s.record(resp("h3", true, "c", 10)).await;
    let p = log_path(tmp.path());
    let text = std::fs::read_to_string(&p).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| l)
        .collect();
    std::fs::write(&p, kept.join("\n") + "\n").unwrap();
    assert_eq!(s.lookup("h1").await, None);
    assert_eq!(s.lookup("h3").await, None);
}

#[tokio::test]
async fn a_missing_head_over_a_non_empty_log_fails_closed() {
    // Also the migration case: a log written before heads existed has no head.
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 0)).await;
    std::fs::remove_dir_all(heads_dir(tmp.path())).unwrap();
    assert_eq!(s.lookup("h1").await, None, "ask again rather than trust");
}

// --- fail closed, and stay closed --------------------------------------

#[tokio::test]
async fn list_says_why_instead_of_showing_an_empty_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 0)).await;
    drop_last_line(tmp.path());
    let err = s.list().unwrap_err().to_string();
    assert!(err.contains("integrity"), "the reason is named: {err}");
}

#[tokio::test]
async fn recording_after_tampering_does_not_re_bless_the_surviving_allow() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    s.forget(None).unwrap();
    drop_last_line(tmp.path()); // revocation deleted
    assert_eq!(s.lookup("h1").await, None);

    // The user answers an unrelated prompt. That must not re-seal a log whose
    // revocation is missing and thereby make the old allow live again.
    s.record(resp("h2", true, "b", 0)).await;

    assert_eq!(s.lookup("h1").await, None, "h1 stays unremembered");
    assert_eq!(
        s.lookup("h2").await,
        Some(Settled::Allow),
        "h2 is remembered"
    );
}

// --- what is NOT detected, written down as a test ----------------------

#[tokio::test]
async fn known_gap_restoring_log_and_head_together_is_not_detected() {
    // Out of scope (#1859 threat model): a copy of both files from before the
    // revocation is an older, internally consistent state. Only an anchor the
    // agent cannot write could tell it apart. If this test ever fails, the gap
    // got closed: update the PR's Decisions and delete the test.
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    let keep = tmp.path().join("snapshot");
    let log_copy = std::fs::read(log_path(tmp.path())).unwrap();
    copy_dir(&heads_dir(tmp.path()), &keep);

    s.forget(None).unwrap();
    assert_eq!(s.lookup("h1").await, None);

    std::fs::write(log_path(tmp.path()), log_copy).unwrap();
    std::fs::remove_dir_all(heads_dir(tmp.path())).unwrap();
    copy_dir(&keep, &heads_dir(tmp.path()));

    assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
}
