//! Tests for `RetryOutcome::Aborted` and the human wait carried by every
//! pause path (P3b-§6.3 steps 4–6, D7, D8; AC-P3b-18, 20b).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::driver::{
    FlushFailed, RetryOutcome, ReviewTransport, SendGate, TaskFailed, run_turn_with_retry,
};
use super::driver_tests::setup_channel;
use super::schema::{
    HumanNote, NoteClassification, PauseKind, ReviewPayload, classify_note_payload,
};

static EMPTY: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
const GATE: SendGate<'static> = SendGate {
    boundary: false,
    pre_confirmed: false,
    open: &EMPTY,
};

/// A transport whose `turn_committed()` and gate wait are scripted.
struct Scripted {
    committed: bool,
    replies: Mutex<Vec<anyhow::Result<String>>>,
    sends: AtomicUsize,
    wait: Mutex<Duration>,
}

impl Scripted {
    fn new(committed: bool, replies: Vec<anyhow::Result<String>>, wait: Duration) -> Self {
        let mut r = replies;
        r.reverse();
        Self {
            committed,
            replies: Mutex::new(r),
            sends: AtomicUsize::new(0),
            wait: Mutex::new(wait),
        }
    }
}

impl ReviewTransport for Scripted {
    fn send(&self, _m: &str, _p: &serde_json::Value) -> anyhow::Result<String> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.replies
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(String::new()))
    }
    fn turn_committed(&self) -> bool {
        self.committed
    }
    fn take_human_wait(&self) -> Duration {
        std::mem::take(&mut *self.wait.lock().unwrap())
    }
}

fn run(
    t: &dyn ReviewTransport,
    home: &std::path::Path,
    channel_id: &str,
    flush: &mut dyn FnMut(&[HumanNote]) -> anyhow::Result<()>,
) -> anyhow::Result<RetryOutcome> {
    run_turn_with_retry(
        t,
        home,
        "review-x",
        "reviewer",
        &|_| serde_json::json!({}),
        &mut Vec::new(),
        flush,
        GATE,
        channel_id,
        Duration::ZERO,
    )
}

fn payloads(home: &std::path::Path, channel_id: &str) -> Vec<ReviewPayload> {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    svc.load_events(channel_id)
        .unwrap()
        .iter()
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect()
}

fn paused_wait(home: &std::path::Path, channel_id: &str) -> (PauseKind, u64) {
    payloads(home, channel_id)
        .into_iter()
        .find_map(|p| match p {
            ReviewPayload::Paused {
                kind,
                human_wait_ms,
                ..
            } => Some((kind, human_wait_ms)),
            _ => None,
        })
        .expect("a paused event")
}

#[test]
fn turn_not_committed_is_aborted_not_sent() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(false, vec![Ok("REPLY-X".into())], Duration::ZERO);
    let out = run(&t, tmp.path(), &ch, &mut |_| Ok(())).unwrap();
    assert_eq!(out, RetryOutcome::Aborted);
    assert!(
        !payloads(tmp.path(), &ch)
            .iter()
            .any(|p| matches!(p, ReviewPayload::TurnSent { .. })),
        "an aborted reply is never ledgered"
    );
}

/// AC-P3b-18: cancelling the task makes the runtime answer `cancelled`;
/// that is the abort, not a task failure.
#[test]
fn turn_not_committed_and_task_cancelled_is_aborted_not_task_failed() {
    let (tmp, ch) = setup_channel();
    let failed = TaskFailed {
        member: "reviewer".into(),
        cause: "cancelled".into(),
    };
    let t = Scripted::new(false, vec![Err(failed.into())], Duration::ZERO);
    let out = run(&t, tmp.path(), &ch, &mut |_| Ok(())).unwrap();
    assert_eq!(out, RetryOutcome::Aborted);
}

#[test]
fn turn_not_committed_and_transport_error_is_aborted_no_retry() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(
        false,
        vec![
            Err(anyhow::anyhow!("connection dropped")),
            Ok("late".into()),
        ],
        Duration::ZERO,
    );
    let out = run(&t, tmp.path(), &ch, &mut |_| Ok(())).unwrap();
    assert_eq!(out, RetryOutcome::Aborted);
    assert_eq!(t.sends.load(Ordering::SeqCst), 1, "no retry after an abort");
    assert!(
        !payloads(tmp.path(), &ch)
            .iter()
            .any(|p| matches!(p, ReviewPayload::Paused { .. })),
        "the driver writes no transport pause for an abort"
    );
}

/// The default `turn_committed()` is `true`: pre-3b outcomes are untouched.
#[test]
fn default_turn_committed_is_unchanged() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(true, vec![Ok("REPLY-X".into())], Duration::ZERO);
    match run(&t, tmp.path(), &ch, &mut |_| Ok(())).unwrap() {
        RetryOutcome::Sent { reply, .. } => assert_eq!(reply, "REPLY-X"),
        other => panic!("expected Sent, got {other:?}"),
    }
}

/// AC-P3b-20b: the gate wait rides on the transport pause, once.
#[test]
fn transport_pause_carries_gate_wait() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(
        true,
        vec![
            Err(anyhow::anyhow!("offline")),
            Err(anyhow::anyhow!("offline")),
        ],
        Duration::from_secs(90),
    );
    let out = run(&t, tmp.path(), &ch, &mut |_| Ok(())).unwrap();
    assert!(matches!(out, RetryOutcome::Paused { .. }));
    assert_eq!(paused_wait(tmp.path(), &ch), (PauseKind::Transport, 90_000));
    assert_eq!(t.take_human_wait(), Duration::ZERO, "taken exactly once");
}

#[test]
fn flush_failed_pause_carries_gate_wait() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(true, vec![Ok("unused".into())], Duration::from_secs(90));
    let mut flush = |_: &[HumanNote]| -> anyhow::Result<()> {
        Err(FlushFailed {
            recorded: 0,
            total: 1,
            cause: "disk".into(),
        }
        .into())
    };
    let out = run_turn_with_retry(
        &t,
        tmp.path(),
        "review-x",
        "reviewer",
        &|_| serde_json::json!({}),
        &mut vec![HumanNote {
            text: "NOTE-A".into(),
            target: None,
        }],
        &mut flush,
        GATE,
        &ch,
        Duration::ZERO,
    )
    .unwrap();
    assert!(matches!(out, RetryOutcome::Paused { .. }));
    assert_eq!(paused_wait(tmp.path(), &ch), (PauseKind::Other, 90_000));
}
