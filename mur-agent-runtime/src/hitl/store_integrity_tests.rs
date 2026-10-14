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

// --- concurrent append + read ------------------------------------------

/// Run a future to completion on the calling thread, so plain OS threads can
/// drive the async store API.
fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

/// Two writers append while two readers verify. A line is written before its
/// head is sealed; a reader that saw the gap would call it a deletion and the
/// store would refuse to remember anything. Readers must never see that, and
/// every record must survive.
#[test]
fn concurrent_append_and_read_never_looks_like_tampering() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const WRITERS: usize = 2;
    const PER_WRITER: usize = 60;

    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    // The channel and marker exist before the race starts.
    block(s.record(resp("seed", true, "seed", 0)));

    let done = AtomicBool::new(false);
    let reads = AtomicUsize::new(0);
    let false_alarms = std::sync::Mutex::new(Vec::<String>::new());

    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..WRITERS)
            .map(|w| {
                let s = &s;
                scope.spawn(move || {
                    for i in 0..PER_WRITER {
                        let h = format!("h-{w}-{i}");
                        block(s.record(resp(&h, true, &h, 0)));
                    }
                })
            })
            .collect();
        for _ in 0..3 {
            scope.spawn(|| {
                // At least one full pass even if the writers win the race.
                loop {
                    let finished = done.load(Ordering::SeqCst);
                    match s.list() {
                        Ok(_) => {}
                        Err(e) => false_alarms.lock().unwrap().push(format!("{e:#}")),
                    }
                    reads.fetch_add(1, Ordering::SeqCst);
                    if finished {
                        break;
                    }
                    // A real reader looks once per tool call. Back-to-back
                    // shared locks from several threads would starve the
                    // writers' exclusive lock and the test would never end.
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            });
        }
        for w in writers {
            w.join().unwrap();
        }
        done.store(true, Ordering::SeqCst);
    });

    assert!(reads.load(Ordering::SeqCst) >= 2, "readers actually ran");
    let alarms = false_alarms.into_inner().unwrap();
    assert!(
        alarms.is_empty(),
        "a reader mistook a write for tampering: {alarms:?}"
    );

    // Nothing was lost or refused to seal: the final log verifies and holds
    // the seed plus every record.
    let live = s.list().expect("final log verifies");
    assert_eq!(live.len(), 1 + WRITERS * PER_WRITER);
    assert_eq!(block(s.lookup("h-1-24")), Some(Settled::Allow));
}

/// The stress test above cannot hit the one real window (a reader sees the
/// log from before an append and the head from after it) because it is
/// microseconds wide. So pin the mechanism that closes it: while a writer
/// holds the channel's append lock, a reader waits instead of reading.
#[test]
fn a_reader_waits_for_a_writer_that_holds_the_append_lock() {
    use fs2::FileExt;
    use std::sync::mpsc;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    block(s.record(resp("h1", true, "a", 0)));
    let lock_path = log_path(tmp.path()).with_file_name("events.lock");
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    writer.lock_exclusive().unwrap();

    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| tx.send(s.list().map(|l| l.len())).unwrap());
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the reader went ahead while a writer held the append lock"
        );
        FileExt::unlock(&writer).unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("reader resumes");
        assert_eq!(got.unwrap(), 1);
    });
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
