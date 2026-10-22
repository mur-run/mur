//! #1859: gate B must notice that a record it once wrote has disappeared, and
//! stop trusting remembered approvals instead of falling back to whatever
//! older allow survived. Every test here drives the real store, the real
//! signature and the real files; "deleting" is done on `events.jsonl` itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mur_common::agent::{HITL_CHANNEL_FILE, HITL_HEAD_DIR};
use mur_common::hitl::HitlResponse;
use mur_common::identity::AgentIdentity;

use super::store::{ChannelDecisionStore, DecisionStore, Listing, Settled};

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
    use std::sync::atomic::{AtomicUsize, Ordering};

    const WRITERS: usize = 2;
    const READERS: usize = 3;
    const READS_EACH: usize = 20;
    // Small on purpose: every append re-reads the log and fsyncs a head, so
    // the cost grows with the square of this, and a loaded CI runner timed
    // out at 120 appends. The lock itself is pinned deterministically below.
    const PER_WRITER: usize = 12;

    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    // The channel and marker exist before the race starts.
    block(s.record(resp("seed", true, "seed", 0)));

    let reads = AtomicUsize::new(0);
    let false_alarms = std::sync::Mutex::new(Vec::<String>::new());

    std::thread::scope(|scope| {
        for w in 0..WRITERS {
            let s = &s;
            scope.spawn(move || {
                for i in 0..PER_WRITER {
                    let h = format!("h-{w}-{i}");
                    block(s.record(resp(&h, true, &h, 0)));
                }
            });
        }
        // A FIXED number of reads, never "until the writers finish". flock has
        // no writer preference on Linux: readers that keep overlapping starve
        // the writers' exclusive lock forever, and CI timed out on exactly
        // that. Bounded readers always leave the writers a way through.
        for _ in 0..READERS {
            scope.spawn(|| {
                for _ in 0..READS_EACH {
                    if let Err(e) = s.list() {
                        false_alarms.lock().unwrap().push(format!("{e:#}"));
                    }
                    reads.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(3));
                }
            });
        }
    });

    assert_eq!(
        reads.load(Ordering::SeqCst),
        READERS * READS_EACH,
        "every read ran"
    );
    let alarms = false_alarms.into_inner().unwrap();
    assert!(
        alarms.is_empty(),
        "a reader mistook a write for tampering: {alarms:?}"
    );

    // Nothing was lost or refused to seal: the final log verifies and holds
    // the seed plus every record.
    let live = s.list().expect("final log verifies");
    assert_eq!(live.len(), 1 + WRITERS * PER_WRITER);
    assert_eq!(block(s.lookup("h-1-11")), Some(Settled::Allow));
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

// --- #2033: reporting an untrusted log ----------------------------------

#[tokio::test]
async fn list_report_names_the_reason_and_channel_for_a_headless_log() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    // The state an install upgraded across #1890 is in: records, no head.
    std::fs::remove_dir_all(heads_dir(tmp.path())).unwrap();

    match s.list_report().unwrap() {
        Listing::Unavailable {
            reason,
            channel_id: got,
        } => {
            assert!(reason.contains("no signed head"), "{reason}");
            assert_eq!(got, channel_id(tmp.path()));
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    // Fail closed is unchanged: the gate still replays nothing.
    assert_eq!(s.lookup("h1").await, None);
    assert!(s.list().is_err());
}

#[tokio::test]
async fn list_report_lists_a_trusted_log_normally() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    match s.list_report().unwrap() {
        Listing::Live(v) => assert_eq!(v.len(), 1),
        other => panic!("expected Live, got {other:?}"),
    }
}

#[tokio::test]
async fn list_report_keeps_an_unreadable_head_an_error() {
    // The head path exists but cannot be read as a file: the check could not
    // run, which is an internal failure, not a verdict `channel repair` fixes.
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    let head = std::fs::read_dir(heads_dir(tmp.path()))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::remove_file(&head).unwrap();
    std::fs::create_dir(&head).unwrap();

    let err = s
        .list_report()
        .expect_err("an I/O failure must stay an error");
    assert!(err.to_string().contains("head unreadable"), "{err:#}");
}

#[tokio::test]
async fn list_report_reports_a_corrupt_head_as_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    s.record(resp("h1", true, "a", 5)).await;
    let head = std::fs::read_dir(heads_dir(tmp.path()))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&head, b"not json").unwrap();
    assert!(matches!(
        s.list_report().unwrap(),
        Listing::Unavailable { .. }
    ));
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
