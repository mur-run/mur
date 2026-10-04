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
use super::ledger::fold_rounds;
use super::loop_driver::{LoopDriverStop, run_review_loop};
use super::schema::{
    Mode, NoteClassification, ReviewPayload, Role, SessionLimits, classify_note_payload,
};
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
fn setup_channel() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
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
    (tmp, channel_id)
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
    let (tmp, channel_id) = setup_channel();
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
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
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
    let replayed = fold_rounds(&payloads).unwrap();
    assert_eq!(
        replayed, ledger,
        "folding the payloads read back from the channel must equal the in-memory ledger"
    );

    // Sanity on the ledger content itself: one finding issued in round 1,
    // resolved in round 2, so the open set is empty by the time it stops.
    assert_eq!(ledger.findings.len(), 1);
    assert!(ledger.open_set().is_empty());
}

/// S1 (revised): a turn that takes longer than the stuck window but DOES
/// return is activity, not a stall (§3.5: stuck = no agent-authored event
/// for the window). The fake clock jumps 6 minutes inside the reviewer's
/// `send` with `Stuck::After(5 min)`; the late `approve` must still be
/// folded, signed, and end the loop cleanly.
#[test]
fn long_turn_that_returns_is_not_stuck() {
    let (tmp, channel_id) = setup_channel();
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
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(
            Duration::from_secs(3600),
            Stuck::After(Duration::from_secs(5 * 60)),
            None,
        ),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(
        stop,
        LoopDriverStop::Approve,
        "a slow reviewer turn that returns must not trip the stuck guard"
    );
    assert_eq!(ledger.round, 1, "the late approve must be folded");
    let payloads = read_payloads(home, &channel_id);
    assert!(
        !payloads.is_empty(),
        "the late approve must be appended to the channel"
    );
}

/// S1: with `Stuck::Off`, the same 6-minute clock jump inside the
/// reviewer's `send` must never trip the guard — the loop reaches
/// `approve` normally.
#[test]
fn stuck_off_never_trips() {
    let (tmp, channel_id) = setup_channel();
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
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Approve, "Stuck::Off must never trip");
}

/// Scripted per-member replies plus a fake clock that advances by `main_jump`
/// inside every `main` send — lets a test land a limit at a chosen round.
struct ScriptedClockTransport {
    clock: Rc<Cell<Instant>>,
    main_jump: Duration,
    main_replies: Mutex<Vec<String>>,
    reviewer_replies: Mutex<Vec<String>>,
}

impl ScriptedClockTransport {
    fn new(clock: Rc<Cell<Instant>>, main_jump: Duration, reviewer: Vec<String>) -> Self {
        let mut reviewer = reviewer;
        reviewer.reverse();
        Self {
            clock,
            main_jump,
            main_replies: Mutex::new(Vec::new()),
            reviewer_replies: Mutex::new(reviewer),
        }
    }
}

impl ReviewTransport for ScriptedClockTransport {
    fn send(&self, member: &str, _params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => {
                self.clock.set(self.clock.get() + self.main_jump);
                Ok(self
                    .main_replies
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| "main output".to_string()))
            }
            "reviewer" => Ok(self
                .reviewer_replies
                .lock()
                .unwrap()
                .pop()
                .expect("reviewer asked for more rounds than scripted")),
            other => panic!("unexpected member {other:?}"),
        }
    }
}

/// AC9: round-stuck fires after exactly two consecutive rounds with an
/// unchanged open set — the loop stops after round 2 (no round 3 send), and
/// a channel replay through `fold_rounds` reproduces the stuck ledger.
#[test]
fn ac9_round_stuck_stops_loop_after_two_unchanged_rounds() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = ScriptedClockTransport::new(
        Rc::clone(&clock),
        Duration::ZERO,
        vec![
            serde_json::json!({
                "verdict": "revise",
                "findings": [{"severity": "medium", "issue": "missing error path"}],
            })
            .to_string(),
            serde_json::json!({
                "verdict": "revise",
                "prior": [{"id": "F1", "status": "open"}],
            })
            .to_string(),
        ],
    );

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::RoundStuck);
    assert_eq!(ledger.round, 2, "stops after round 2, never starts round 3");
    assert!(ledger.round_stuck);
    let replayed = fold_rounds(&read_payloads(home, &channel_id)).unwrap();
    assert_eq!(replayed, ledger, "replay reproduces round-stuck state");
}

/// AC13: the loop is stopped by a limit (deadline, landed at the start of
/// round 3), and the stop-screen data lists every open + disputed finding —
/// in issue order normally, disputed first after an approve (§8.3).
#[test]
fn ac13_limit_stop_lists_open_and_disputed_findings() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let clock = Rc::new(Cell::new(Instant::now()));
    // 25 s per main turn, deadline 55 s: rounds 1–2 complete (t=25, t=50),
    // round 3's main turn returns at t=75 and trips the deadline before
    // anything from round 3 is folded.
    let transport = ScriptedClockTransport::new(
        Rc::clone(&clock),
        Duration::from_secs(25),
        vec![
            serde_json::json!({
                "verdict": "revise",
                "findings": [
                    {"severity": "high", "issue": "unchecked unwrap"},
                    {"severity": "medium", "issue": "no timeout"},
                    {"severity": "low", "issue": "naming"},
                ],
            })
            .to_string(),
            serde_json::json!({
                "verdict": "revise",
                "prior": [
                    {"id": "F1", "status": "open"},
                    {"id": "F2", "status": "resolved"},
                    {"id": "F3", "status": "disputed"},
                ],
            })
            .to_string(),
        ],
    );

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(55), Stuck::Off, None),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Guard(LoopStop::Deadline));
    assert_eq!(ledger.round, 2);
    let ids = |after_approve: bool| -> Vec<String> {
        ledger
            .stop_screen_findings(after_approve)
            .into_iter()
            .map(|f| f.id.clone())
            .collect()
    };
    assert_eq!(
        ids(false),
        ["F1", "F3"],
        "open + disputed, resolved F2 omitted"
    );
    assert_eq!(ids(true), ["F3", "F1"], "after approve, disputed first");
}

/// Records the params main receives each turn, then delegates to the
/// scripted `StubLoopTransport`.
struct RecordingTransport {
    inner: StubLoopTransport,
    main_params: Mutex<Vec<serde_json::Value>>,
}

impl ReviewTransport for RecordingTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        if member == "main" {
            self.main_params.lock().unwrap().push(params.clone());
        }
        self.inner.send(member, params)
    }
}

/// §3.3: from round 2 on, main must receive the open findings it has to
/// answer — not just the round number.
#[test]
fn main_receives_open_findings_from_round_two() {
    let (tmp, channel_id) = setup_channel();
    let round1 = serde_json::json!({
        "verdict": "revise",
        "findings": [{"severity": "high", "issue": "unchecked unwrap"}],
    })
    .to_string();
    let round2 = serde_json::json!({
        "verdict": "approve",
        "prior": [{"id": "F1", "status": "resolved"}],
    })
    .to_string();
    let transport = RecordingTransport {
        inner: StubLoopTransport::new(vec![], vec![&round1, &round2]),
        main_params: Mutex::new(vec![]),
    };

    let (_, stop) = run_review_loop(
        &transport,
        tmp.path(),
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);

    let seen = transport.main_params.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["open_findings"], serde_json::json!([]));
    let open = seen[1]["open_findings"].as_array().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["id"], "F1");
    assert_eq!(open[0]["issue"], "unchecked unwrap");
    assert_eq!(open[0]["status"], "open");
}

/// §8.2: a reviewer reply naming an unissued finding must not be signed
/// into the channel. The loop stops `Blocked`, and the channel still
/// replays cleanly to the in-memory ledger.
#[test]
fn unissued_finding_id_blocks_without_poisoning_channel() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let bad = serde_json::json!({
        "verdict": "approve",
        "prior": [{"id": "F99", "status": "resolved"}],
    })
    .to_string();
    let transport = StubLoopTransport::new(vec![], vec![&bad]);

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::Blocked);

    let payloads = read_payloads(home, &channel_id);
    assert!(
        payloads
            .iter()
            .all(|p| !matches!(p, ReviewPayload::FindingStatus { .. })),
        "the illegal finding_status must never reach the channel"
    );
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}

/// §4: the channel opens with `session_started` (members, mode), and every
/// delivered turn leaves a `turn_sent` before the round's verdict.
#[test]
fn channel_records_session_started_and_turn_sent() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let revise = serde_json::json!({
        "verdict": "revise",
        "findings": [{"severity": "low", "issue": "needs a doc comment"}],
    })
    .to_string();
    let approve = serde_json::json!({
        "verdict": "approve",
        "prior": [{"id": "F1", "status": "resolved"}],
    })
    .to_string();
    let transport = StubLoopTransport::new(vec![], vec![&revise, &approve]);
    let limits = SessionLimits::new(
        Duration::from_secs(3600),
        Stuck::After(Duration::from_secs(10 * 60)),
        Some(2.5),
    );

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::Auto,
        Duration::ZERO,
        limits,
        &Instant::now,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);

    let payloads = read_payloads(home, &channel_id);
    assert_eq!(
        payloads[0],
        ReviewPayload::SessionStarted {
            members: ["main".into(), "reviewer".into()],
            mode: Mode::Auto,
            limits: SessionLimits {
                deadline_ms: 3_600_000,
                stuck_ms: Some(600_000),
                cost_usd_micros: Some(2_500_000),
            },
        }
    );
    let turns: Vec<(u32, Role)> = payloads
        .iter()
        .filter_map(|p| match p {
            ReviewPayload::TurnSent { round, to, .. } => Some((*round, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(
        turns,
        vec![
            (1, Role::Main),
            (1, Role::Reviewer),
            (2, Role::Main),
            (2, Role::Reviewer),
        ]
    );
    // Each round's reviewer `turn_sent` precedes that round's verdict.
    for r in 1..=2 {
        let sent = payloads
            .iter()
            .position(|p| matches!(p, ReviewPayload::TurnSent { round, to: Role::Reviewer, .. } if *round == r))
            .unwrap();
        let verdict = payloads
            .iter()
            .position(|p| matches!(p, ReviewPayload::Verdict { round, .. } if *round == r))
            .unwrap();
        assert!(sent < verdict, "round {r}: turn_sent must precede verdict");
    }
    assert_eq!(ledger.mode, Mode::Auto);
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}

/// A round cut short after `turn_sent` (deadline hit once main's reply is
/// back) has no verdict. Replay must not seal it, or `fold_rounds` would
/// push an extra open-set snapshot the live loop never took.
#[test]
fn round_cut_after_turn_sent_replays_to_live_ledger() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let revise = serde_json::json!({
        "verdict": "revise",
        "findings": [{"severity": "low", "issue": "needs a doc comment"}],
    })
    .to_string();
    // Round 1 reviewer jumps the clock 6 min; deadline is 10 min, so round 2
    // starts, and round 2's reviewer jump crosses the deadline after its
    // `turn_sent` is already signed.
    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = StubClockTransport {
        clock: Rc::clone(&clock),
        jump: Duration::from_secs(6 * 60),
        main_reply: "main output".to_string(),
        reviewer_reply: revise,
    };

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(10 * 60), Stuck::Off, None),
        &|| clock.get(),
    )
    .unwrap();
    assert!(
        matches!(stop, LoopDriverStop::Guard(LoopStop::Deadline)),
        "{stop:?}"
    );

    let payloads = read_payloads(home, &channel_id);
    assert!(
        payloads.iter().any(|p| matches!(
            p,
            ReviewPayload::TurnSent {
                round: 2,
                to: Role::Reviewer,
                ..
            }
        )),
        "round 2's reviewer send happened and must be recorded"
    );
    assert!(
        !payloads
            .iter()
            .any(|p| matches!(p, ReviewPayload::Verdict { round: 2, .. })),
        "the late round-2 verdict must be discarded"
    );
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}

/// Main answers; the reviewer's transport always fails, so the loop pauses
/// after one retry and `driver.rs` writes `paused` + `mode_changed`.
struct ReviewerDownTransport;

impl ReviewTransport for ReviewerDownTransport {
    fn send(&self, member: &str, _params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => Ok("main output".to_string()),
            _ => anyhow::bail!("reviewer offline"),
        }
    }
}

/// One writer per session channel: the loop's own events and the
/// `paused` / `mode_changed` pair `driver.rs` writes on a transport failure
/// are all signed by the router identity, so every one verifies under
/// `require_sig = true` — the mode the product is meant to run in.
#[test]
fn every_session_event_verifies_under_one_writer() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();

    let (_ledger, stop) = run_review_loop(
        &ReviewerDownTransport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        Mode::Auto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    assert!(
        matches!(stop, LoopDriverStop::Paused { .. }),
        "reviewer offline must pause, got {stop:?}"
    );

    let svc = mur_channel::ChannelService::open(home).unwrap();
    let events = svc.load_events(&channel_id).unwrap();
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(match env.payload {
                ReviewPayload::SessionStarted { .. } => "session_started",
                ReviewPayload::TurnSent { .. } => "turn_sent",
                ReviewPayload::Paused { .. } => "paused",
                ReviewPayload::ModeChanged { .. } => "mode_changed",
                _ => "other",
            }),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["session_started", "turn_sent", "paused", "mode_changed"],
        "both writers (loop_driver + driver) must land on the channel"
    );
    for ev in &events {
        assert!(ev.sig.is_some(), "event seq {} is unsigned", ev.seq);
        assert!(
            crate::channel_verify::verify_event(home, &channel_id, ev, true),
            "event seq {} does not verify under the router key",
            ev.seq
        );
    }
}
