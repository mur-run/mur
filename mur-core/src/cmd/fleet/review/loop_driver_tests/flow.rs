//! Full-loop flow: approve end-to-end, what main receives, unissued-finding block.

use super::*;
use crate::cmd::fleet::review::constants::REVIEW_NO_OPEN_FINDINGS;
use crate::cmd::fleet::review::wire::message_text;

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
        "task",
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
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);

    let seen = transport.main_params.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let round1_text = message_text(&seen[0]).expect("round 1 is an A2A text message");
    assert!(round1_text.contains("task"));
    assert!(round1_text.contains(REVIEW_NO_OPEN_FINDINGS));
    let round2_text = message_text(&seen[1]).expect("round 2 is an A2A text message");
    assert!(round2_text.contains("- F1 [high, open]: unchecked unwrap"));
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
    // §3.2: malformed twice (retry once with a hint) → blocked.
    let transport = StubLoopTransport::new(vec![], vec![&bad, &bad]);

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
        &Instant::now,
    )
    .unwrap();
    assert_eq!(
        stop,
        LoopDriverStop::Blocked {
            role: Role::Reviewer
        }
    );

    let payloads = read_payloads(home, &channel_id);
    assert!(
        payloads
            .iter()
            .all(|p| !matches!(p, ReviewPayload::FindingStatus { .. })),
        "the illegal finding_status must never reach the channel"
    );
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}

/// §3.2: real models wrap the verdict in prose and a ```json fence; the
/// loop must read it rather than treat the round as `blocked`.
#[test]
fn fenced_verdict_inside_prose_is_accepted() {
    let (tmp, channel_id) = setup_channel();
    let fenced = "Looks good overall.\n```json\n{\"verdict\": \"approve\"}\n```\n";
    let transport = StubLoopTransport::new(vec![], vec![fenced]);

    let (_, stop) = run_review_loop(
        &transport,
        tmp.path(),
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);
}

/// A reviewer whose task fails ends the loop as `TaskFailed`, not
/// `Blocked` — and no verdict is signed for a reply that never existed.
#[test]
fn reviewer_task_failure_stops_as_task_failed_not_blocked() {
    struct FailingReviewer;
    impl ReviewTransport for FailingReviewer {
        fn send(&self, member: &str, _p: &serde_json::Value) -> anyhow::Result<String> {
            match member {
                "main" => Ok("draft".into()),
                _ => Err(crate::cmd::fleet::review::driver::TaskFailed {
                    member: member.into(),
                    cause: "tool call denied: timed out".into(),
                }
                .into()),
            }
        }
    }
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let (_ledger, stop) = run_review_loop(
        &FailingReviewer,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    assert_eq!(
        stop,
        LoopDriverStop::TaskFailed {
            member: "reviewer".into(),
            cause: "tool call denied: timed out".into(),
        }
    );
    assert!(
        read_payloads(home, &channel_id)
            .iter()
            .all(|p| !matches!(p, ReviewPayload::Verdict { .. })),
        "no verdict may be recorded for a turn that produced none"
    );
}
