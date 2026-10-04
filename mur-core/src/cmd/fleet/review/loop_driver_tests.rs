//! Tests for `loop_driver.rs` (D3, AC12): "a full loop runs to `approve`"
//! (spec line 511), built on the EXISTING driver (`run_turn_with_retry`) and
//! ledger (`fold`, `apply`, `note_round_complete`) rather than inventing a
//! second way to send or fold.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mur_common::channel::EventKind;
use mur_common::limits::Stuck;

use super::driver::ReviewTransport;
use super::ledger::fold;
use super::loop_driver::{LoopDriverStop, run_review_loop};
use super::schema::{NoteClassification, ReviewPayload, classify_note_payload};
use crate::cmd::fleet::loop_run::LoopStop;

/// Test-only transport: counts sends PER MEMBER and returns the next queued
/// reply for that member. Mirrors `driver_tests.rs`'s `StubTransport`, but
/// keyed by member name so a two-party loop can script `main` and
/// `reviewer` independently.
struct StubLoopTransport {
    main_sends: AtomicUsize,
    reviewer_sends: AtomicUsize,
    main_replies: Mutex<Vec<String>>,
    reviewer_replies: Mutex<Vec<String>>,
}

impl StubLoopTransport {
    fn new(main_replies: Vec<&str>, reviewer_replies: Vec<&str>) -> Self {
        let mut main = main_replies
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        main.reverse();
        let mut reviewer = reviewer_replies
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        reviewer.reverse();
        Self {
            main_sends: AtomicUsize::new(0),
            reviewer_sends: AtomicUsize::new(0),
            main_replies: Mutex::new(main),
            reviewer_replies: Mutex::new(reviewer),
        }
    }

    fn main_send_count(&self) -> usize {
        self.main_sends.load(Ordering::SeqCst)
    }

    fn reviewer_send_count(&self) -> usize {
        self.reviewer_sends.load(Ordering::SeqCst)
    }
}

impl ReviewTransport for StubLoopTransport {
    fn send(&self, member: &str, _params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => {
                self.main_sends.fetch_add(1, Ordering::SeqCst);
                Ok(self
                    .main_replies
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| "ok".to_string()))
            }
            "reviewer" => {
                self.reviewer_sends.fetch_add(1, Ordering::SeqCst);
                Ok(self
                    .reviewer_replies
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| serde_json::json!({"verdict": "approve"}).to_string()))
            }
            other => panic!("unexpected member {other:?}"),
        }
    }
}

/// Stub transport for the stuck-detector tests below: a single scripted
/// reply per member, but it advances a shared fake clock by `jump` INSIDE
/// the reviewer's `send` — simulating a reviewer turn that itself takes
/// longer than the stuck window, before `run_review_loop` ever gets to look
/// at the reply.
struct StubClockTransport {
    clock: Rc<Cell<Instant>>,
    jump: Duration,
    main_reply: String,
    reviewer_reply: String,
}

impl ReviewTransport for StubClockTransport {
    fn send(&self, member: &str, _params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => Ok(self.main_reply.clone()),
            "reviewer" => {
                self.clock.set(self.clock.get() + self.jump);
                Ok(self.reviewer_reply.clone())
            }
            other => panic!("unexpected member {other:?}"),
        }
    }
}

/// Returns a fresh `~/.mur`-shaped tempdir with a review channel created and
/// the router's signing identity planted — copied from
/// `driver_tests.rs::setup_channel` (same shape the retry/pause path needs).
fn setup_channel() -> (
    tempfile::TempDir,
    String,
    mur_common::identity::AgentIdentity,
    u32,
) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let identity = crate::channel_writer::plant_writer_identity(home);
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let channel_id = "review-ac12-channel".to_string();
    svc.store()
        .create(&mur_common::channel::Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: channel_id.clone(),
            title: "t".into(),
            goal: mur_common::channel::Goal::default(),
            state: mur_common::channel::ChannelState::Working,
            purpose: None,
            owner: mur_common::channel::ChannelActor::System,
            participants: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
        .unwrap();
    (tmp, channel_id, identity, 0)
}

/// Read back every review payload on `channel_id`, in channel order — the
/// shape `fold` expects.
fn read_payloads(home: &std::path::Path, channel_id: &str) -> Vec<ReviewPayload> {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    svc.load_events(channel_id)
        .unwrap()
        .into_iter()
        .filter(|ev| ev.kind == EventKind::Note)
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect()
}

/// AC12: "a full loop runs to `approve`". The reviewer replies `revise`
/// (with one finding) in round 1, then `approve` in round 2. Assert: stop
/// reason approve, round count == 2, main sends == 2, reviewer sends == 2,
/// and replaying the channel's own payloads through `fold` reproduces the
/// in-memory ledger exactly.
#[test]
fn ac12_full_loop_runs_to_approve() {
    let (tmp, channel_id, identity, kv) = setup_channel();
    let home = tmp.path();

    let round1_reviewer = serde_json::json!({
        "verdict": "revise",
        "findings": [{"severity": "low", "issue": "needs a doc comment"}],
    })
    .to_string();
    let round2_reviewer = serde_json::json!({
        "verdict": "approve",
        "prior": [{"id": "F1", "status": "resolved"}],
    })
    .to_string();

    let transport = StubLoopTransport::new(
        vec!["main round 1 output", "main round 2 output"],
        vec![&round1_reviewer, &round2_reviewer],
    );

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        &identity,
        kv,
        Duration::ZERO,
        Duration::from_secs(3600),
        Stuck::Off,
        &Instant::now,
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Approve, "must stop on approve");
    assert_eq!(ledger.round, 2, "round count must be 2");
    assert_eq!(transport.main_send_count(), 2, "main must be sent to twice");
    assert_eq!(
        transport.reviewer_send_count(),
        2,
        "reviewer must be sent to twice"
    );

    let payloads = read_payloads(home, &channel_id);
    let replayed = fold(&payloads).unwrap();
    assert_eq!(
        replayed, ledger,
        "folding the payloads read back from the channel must equal the in-memory ledger"
    );

    // Sanity on the ledger content itself: one finding issued in round 1,
    // resolved in round 2, so the open set is empty by the time it stops.
    assert_eq!(ledger.findings.len(), 1);
    assert!(ledger.open_set().is_empty());
}

/// S1: a turn that itself blows the stuck window must trip the guard
/// (b)-check right after `run_turn_with_retry` returns `Sent`, BEFORE that
/// reply is folded into the ledger or appended to the channel. The fake
/// clock jumps 6 minutes inside the reviewer's `send`; `Stuck::After(5
/// min)` must therefore fire on the reviewer's own turn, and the late
/// reviewer reply (an `approve`, which would otherwise end the loop
/// cleanly) must never reach the ledger or the channel.
#[test]
fn stuck_trips_when_a_turn_exceeds_window() {
    let (tmp, channel_id, identity, kv) = setup_channel();
    let home = tmp.path();

    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = StubClockTransport {
        clock: Rc::clone(&clock),
        jump: Duration::from_secs(6 * 60),
        main_reply: "main output".to_string(),
        reviewer_reply: serde_json::json!({ "verdict": "approve" }).to_string(),
    };

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        &identity,
        kv,
        Duration::ZERO,
        Duration::from_secs(3600),
        Stuck::After(Duration::from_secs(5 * 60)),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(
        stop,
        LoopDriverStop::Guard(LoopStop::Stuck),
        "a reviewer turn that itself exceeds the stuck window must trip the guard"
    );
    assert!(
        ledger.findings.is_empty() && ledger.round == 0,
        "the late reviewer reply must not be folded into the ledger"
    );
    let payloads = read_payloads(home, &channel_id);
    assert!(
        payloads.is_empty(),
        "the late reviewer reply must not be appended to the channel"
    );
}

/// S1: with `Stuck::Off`, the same 6-minute clock jump inside the
/// reviewer's `send` must never trip the guard — the loop reaches
/// `approve` normally.
#[test]
fn stuck_off_never_trips() {
    let (tmp, channel_id, identity, kv) = setup_channel();
    let home = tmp.path();

    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = StubClockTransport {
        clock: Rc::clone(&clock),
        jump: Duration::from_secs(6 * 60),
        main_reply: "main output".to_string(),
        reviewer_reply: serde_json::json!({ "verdict": "approve" }).to_string(),
    };

    let (_ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        &identity,
        kv,
        Duration::ZERO,
        Duration::from_secs(3600),
        Stuck::Off,
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Approve, "Stuck::Off must never trip");
}
