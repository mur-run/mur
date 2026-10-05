//! Guard tests: turn-stuck, round-stuck (AC9), and limit stops (AC13).

use super::*;

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
        "task",
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
        "task",
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
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => {
                self.clock.set(self.clock.get() + self.main_jump);
                let text = self
                    .main_replies
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| "main output".to_string());
                Ok(with_accept_all(&text, params))
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
        "task",
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
        "task",
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

/// Semi-auto stub: every send first waits at the human gate for `gate_wait`
/// (fake clock advanced, and reported back as human-input wait, the way
/// `TerminalGate` does), then the member itself runs for `exec`.
struct HumanGateTransport {
    clock: Rc<Cell<Instant>>,
    gate_wait: Duration,
    exec: Duration,
    waited: Cell<Duration>,
}

impl ReviewTransport for HumanGateTransport {
    fn confirm_send(&self, _member: &str, _params: &serde_json::Value) -> anyhow::Result<bool> {
        self.clock.set(self.clock.get() + self.gate_wait);
        self.waited.set(self.waited.get() + self.gate_wait);
        Ok(true)
    }

    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        self.clock.set(self.clock.get() + self.exec);
        match member {
            "main" => Ok(with_accept_all("main output", params)),
            "reviewer" => Ok(serde_json::json!({ "verdict": "approve" }).to_string()),
            other => panic!("unexpected member {other:?}"),
        }
    }

    fn take_human_wait(&self) -> Duration {
        self.waited.take()
    }
}

/// AC4a: time spent waiting on human input (the semi-auto send prompt, a
/// tool-approval prompt) is not execution time. Deadline 10 s, 60 s at each
/// prompt plus 2.5 s of execution per turn: the loop reaches `approve`, and
/// each `turn_sent` records its wait so replay can rebuild the same clock.
#[test]
fn ac4a_human_input_wait_does_not_count_toward_deadline() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = HumanGateTransport {
        clock: Rc::clone(&clock),
        gate_wait: Duration::from_secs(60),
        exec: Duration::from_millis(2500),
        waited: Cell::new(Duration::ZERO),
    };

    let (_ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(10), Stuck::Off, None),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Approve);
    let waits: Vec<u64> = read_payloads(home, &channel_id)
        .iter()
        .filter_map(|p| match p {
            ReviewPayload::TurnSent { human_wait_ms, .. } => Some(*human_wait_ms),
            _ => None,
        })
        .collect();
    assert_eq!(waits, [60_000, 60_000]);
}

/// AC4a, the other half: human wait is still bounded by real execution. The
/// same 60 s prompts with 6 s of execution per turn (12 s > 10 s) trip.
#[test]
fn ac4a_execution_past_the_deadline_still_trips() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = HumanGateTransport {
        clock: Rc::clone(&clock),
        gate_wait: Duration::from_secs(60),
        exec: Duration::from_secs(6),
        waited: Cell::new(Duration::ZERO),
    };

    let (_ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(10), Stuck::Off, None),
        &|| clock.get(),
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Guard(LoopStop::Deadline));
}
