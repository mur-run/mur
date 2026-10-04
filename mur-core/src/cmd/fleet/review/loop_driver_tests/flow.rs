//! Full-loop flow: approve end-to-end, what main receives, unissued-finding block.

use super::*;

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
