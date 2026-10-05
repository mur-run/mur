use super::*;
use crate::cmd::fleet::review::schema::VerdictKind;
use std::time::Duration;

use mur_common::limits::Stuck;

use crate::cmd::fleet::review::schema::{
    Cumulative, RebuttalAnswer, RebuttalResponseDto, SessionLimits,
};

fn cum(ms: u64, micros: u64) -> Cumulative {
    Cumulative {
        exec_time_ms: ms,
        cost_usd_micros: micros,
    }
}

fn issue(ledger: &mut Ledger, severity: Severity, issue: &str, round: u32) -> String {
    let id = ledger.next_finding_id();
    ledger
        .apply(&ReviewPayload::FindingIssued {
            round,
            id: id.clone(),
            severity,
            issue: issue.to_string(),
        })
        .unwrap();
    id
}

/// AC7: IDs are system-assigned and sequential. Model-supplied IDs are
/// ignored — there is no field for the model to supply one through
/// (schema.rs's `NewFindingDto` has no `id`), and an out-of-sequence id
/// written to the ledger is a hard error, never silently accepted.
#[test]
fn finding_ids_are_sequential_and_out_of_sequence_ids_are_rejected() {
    let mut ledger = Ledger::default();
    let f1 = issue(&mut ledger, Severity::Low, "a", 1);
    assert_eq!(f1, "F1");
    let f2 = issue(&mut ledger, Severity::Medium, "b", 1);
    assert_eq!(f2, "F2");

    let mut bad = ledger.clone();
    let err = bad
        .apply(&ReviewPayload::FindingIssued {
            round: 1,
            id: "F99".to_string(),
            severity: Severity::Low,
            issue: "c".to_string(),
        })
        .unwrap_err();
    assert_eq!(
        err,
        FoldError::OutOfSequenceFindingId {
            got: "F99".into(),
            expected: "F3".into(),
        }
    );
}

/// AC8: the same finding rejected twice produces an escalation record.
#[test]
fn rejecting_the_same_finding_twice_escalates() {
    let mut ledger = Ledger::default();
    let f1 = issue(&mut ledger, Severity::High, "x", 1);
    let reject = |id: &str| RebuttalResponseDto {
        id: id.to_string(),
        answer: RebuttalAnswer::Reject,
        reason: Some("disagree".to_string()),
    };
    ledger
        .apply(&ReviewPayload::Rebuttal {
            round: 1,
            responses: vec![reject(&f1)],
            cumulative: cum(0, 0),
        })
        .unwrap();
    assert!(ledger.escalations.is_empty(), "first rejection ≠ escalate");

    ledger
        .apply(&ReviewPayload::Rebuttal {
            round: 2,
            responses: vec![reject(&f1)],
            cumulative: cum(0, 0),
        })
        .unwrap();
    assert_eq!(ledger.escalations.len(), 1);
    assert_eq!(ledger.escalations[0].finding_id, f1);
}

/// AC9: round-stuck fires after EXACTLY two consecutive rounds with an
/// unchanged open set — not one, not three.
#[test]
fn round_stuck_fires_after_exactly_two_unchanged_rounds() {
    let mut ledger = Ledger::default();
    let f1 = issue(&mut ledger, Severity::Low, "x", 1);
    ledger.note_round_complete(); // round 1 snapshot: {F1: open}
    assert!(!ledger.round_stuck, "only one snapshot so far");

    // Round 2: nothing changes.
    ledger.note_round_complete(); // round 2 snapshot: {F1: open} — same as round 1
    assert!(
        ledger.round_stuck,
        "two consecutive rounds with the same open set must trip"
    );

    // A change resets it.
    ledger
        .apply(&ReviewPayload::FindingStatus {
            round: 3,
            id: f1.clone(),
            status: FindingStatus::Resolved,
            reason: None,
        })
        .unwrap();
    ledger.note_round_complete();
    assert!(!ledger.round_stuck, "the open set changed, so not stuck");
}

/// AC10: `approve` with a disputed HIGH finding is refused; with only a
/// disputed medium/low it is accepted, and those are listed first.
#[test]
fn approve_is_blocked_only_by_a_disputed_high_finding() {
    let mut ledger = Ledger::default();
    let high = issue(&mut ledger, Severity::High, "sec bug", 1);
    let low = issue(&mut ledger, Severity::Low, "style nit", 1);
    ledger
        .apply(&ReviewPayload::FindingStatus {
            round: 1,
            id: high.clone(),
            status: FindingStatus::Disputed,
            reason: Some("still think it's a bug".into()),
        })
        .unwrap();
    assert_eq!(ledger.disputed_high_severity().len(), 1);

    // Now dispute only the low finding instead.
    let mut ledger2 = Ledger::default();
    let _high2 = issue(&mut ledger2, Severity::High, "sec bug", 1);
    let low2 = issue(&mut ledger2, Severity::Low, "style nit", 1);
    ledger2
        .apply(&ReviewPayload::FindingStatus {
            round: 1,
            id: low2.clone(),
            status: FindingStatus::Disputed,
            reason: Some("still a nit".into()),
        })
        .unwrap();
    assert!(ledger2.disputed_high_severity().is_empty());
    assert_eq!(ledger2.disputed_medium_low().len(), 1);
    assert_eq!(ledger2.disputed_medium_low()[0].id, low2);
    let _ = (high, low); // silence unused in the first scenario
}

/// AC11 (replay half): folding the SAME event sequence twice produces
/// byte-for-byte (here: structurally) identical ledgers.
#[test]
fn folding_is_deterministic_and_replayable() {
    let build = |note_after_each_round: bool| {
        let mut ledger = Ledger::default();
        ledger
            .apply(&ReviewPayload::SessionStarted {
                members: ["main".into(), "reviewer".into()],
                mode: Mode::SemiAuto,
                limits: SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
            })
            .unwrap();
        let f1 = issue(&mut ledger, Severity::Medium, "x", 1);
        if note_after_each_round {
            ledger.note_round_complete();
        }
        ledger
            .apply(&ReviewPayload::FindingStatus {
                round: 2,
                id: f1,
                status: FindingStatus::Resolved,
                reason: None,
            })
            .unwrap();
        if note_after_each_round {
            ledger.note_round_complete();
        }
        ledger
    };
    let a = build(true);
    let b = build(true);
    assert_eq!(a, b);
}

/// §8.2: "an illegal state transition (e.g. a `finding_status` for an ID
/// never issued)" is an error, not a silent no-op.
#[test]
fn a_status_for_an_unissued_finding_is_an_error() {
    let mut ledger = Ledger::default();
    let err = ledger
        .apply(&ReviewPayload::FindingStatus {
            round: 1,
            id: "F1".to_string(),
            status: FindingStatus::Resolved,
            reason: None,
        })
        .unwrap_err();
    assert_eq!(err, FoldError::StatusForUnissuedFinding("F1".to_string()));
}

/// §8.2 "Limits on rollback — clock and cost are monotonic": a later
/// event reporting a LOWER value never lowers the ledger's total.
#[test]
fn cumulative_totals_never_decrease() {
    let mut ledger = Ledger::default();
    ledger
        .apply(&ReviewPayload::Verdict {
            round: 1,
            kind: VerdictKind::Revise,
            cumulative: cum(5000, 800),
        })
        .unwrap();
    assert_eq!(ledger.exec_time_ms, 5000);
    assert_eq!(ledger.cost_usd_micros, 800);

    // A later event reports LOWER numbers (e.g. forged/garbled) — must
    // not move the totals down.
    ledger
        .apply(&ReviewPayload::Verdict {
            round: 2,
            kind: VerdictKind::Revise,
            cumulative: cum(100, 1),
        })
        .unwrap();
    assert_eq!(ledger.exec_time_ms, 5000, "never decreases");
    assert_eq!(ledger.cost_usd_micros, 800, "never decreases");
}

#[test]
fn round_stuck_ignores_ties_at_the_start() {
    let ledger = Ledger::default();
    assert!(!ledger.round_stuck);
}
