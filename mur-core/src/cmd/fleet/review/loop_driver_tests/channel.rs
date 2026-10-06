//! Channel record tests: session_started / turn_sent, cut rounds, one signing writer.

use super::*;

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
        "task",
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
        "task",
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
        "task",
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

/// AC-P2-10 at driver level: a resumed loop that delivers a ruling's binding
/// note must clear it in the live ledger exactly as replay does. The live
/// loop folds each `turn_sent` into its round ledger, so live == replay on
/// `binding_rulings` once the round seals.
#[test]
fn live_ledger_clears_binding_rulings_like_replay() {
    use super::super::loop_driver::{Members, continue_review_loop};
    use super::super::schema::{RulingDecision, Severity, VerdictKind, to_note_payload};
    use super::super::verdict::zero_cumulative;

    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let sent = |round, to| ReviewPayload::TurnSent {
        round,
        to,
        restart_note: None,
        human_wait_ms: 0,
    };
    let log = vec![
        ReviewPayload::SessionStarted {
            members: ["main".into(), "reviewer".into()],
            mode: Mode::Auto,
            limits: SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        },
        sent(1, Role::Main),
        sent(1, Role::Reviewer),
        ReviewPayload::FindingIssued {
            round: 1,
            id: "F1".into(),
            severity: Severity::Low,
            issue: "needs a doc comment".into(),
        },
        ReviewPayload::Verdict {
            round: 1,
            kind: VerdictKind::Revise,
            cumulative: zero_cumulative(),
        },
        ReviewPayload::Ruling {
            finding: "F1".into(),
            decision: RulingDecision::Drop,
            text: "out of scope".into(),
        },
    ];
    for p in &log {
        crate::channel_writer::append_as_writer(
            &svc,
            home,
            &channel_id,
            crate::channel_writer::ROUTER_AGENT,
            mur_common::channel::ChannelActor::System,
            EventKind::Note,
            to_note_payload(p),
            None,
        )
        .unwrap();
    }
    let resumed = fold_rounds(&log).unwrap();
    assert_eq!(resumed.binding_rulings(Role::Main).len(), 1);

    let transport = StubLoopTransport::new(vec![], vec![]);
    let members = Members {
        fleet_name: "review-x",
        channel_id: &channel_id,
        main: "main",
        reviewer: "reviewer",
        task: "task",
    };
    let (live, stop) = continue_review_loop(
        &transport,
        home,
        &members,
        resumed,
        2,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        Duration::ZERO,
        Duration::ZERO,
        &Instant::now,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);

    let replay = fold_rounds(&read_payloads(home, &channel_id)).unwrap();
    assert!(replay.binding_rulings(Role::Main).is_empty());
    assert!(replay.binding_rulings(Role::Reviewer).is_empty());
    assert_eq!(live, replay, "live ledger must equal replay");
}
