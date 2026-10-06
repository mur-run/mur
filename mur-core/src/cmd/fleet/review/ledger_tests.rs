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

// ---- Phase 2 fold rules (P2-§4) ----

use crate::cmd::fleet::review::schema::{Role, RulingDecision};

fn rebut(ledger: &mut Ledger, id: &str, answer: RebuttalAnswer, round: u32) {
    ledger
        .apply(&ReviewPayload::Rebuttal {
            round,
            responses: vec![RebuttalResponseDto {
                id: id.to_string(),
                answer,
                reason: Some("disagree".to_string()),
            }],
            cumulative: cum(0, 0),
        })
        .unwrap();
}

fn set_status(ledger: &mut Ledger, id: &str, status: FindingStatus) -> Result<(), FoldError> {
    ledger.apply(&ReviewPayload::FindingStatus {
        round: 1,
        id: id.to_string(),
        status,
        reason: None,
    })
}

fn rule(ledger: &mut Ledger, id: &str, decision: RulingDecision) -> Result<(), FoldError> {
    ledger.apply(&ReviewPayload::Ruling {
        finding: id.to_string(),
        decision,
        text: "human says so".to_string(),
    })
}

/// Issue F1, mark it disputed, and reject it twice so it escalates.
fn escalated_f1(ledger: &mut Ledger) -> String {
    let f1 = issue(ledger, Severity::High, "x", 1);
    set_status(ledger, &f1, FindingStatus::Disputed).unwrap();
    rebut(ledger, &f1, RebuttalAnswer::Reject, 1);
    rebut(ledger, &f1, RebuttalAnswer::Reject, 2);
    assert_eq!(ledger.pending_ruling().len(), 1, "precondition: escalated");
    f1
}

#[test]
fn ruling_drop_resolves_and_handles_escalation() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Drop).unwrap();
    let f = ledger.finding(&f1).unwrap();
    assert_eq!(f.status, FindingStatus::Resolved);
    assert_eq!(f.ruled, Some(RulingDecision::Drop));
    assert!(ledger.pending_ruling().is_empty());
    assert_eq!(ledger.escalations.len(), 1);
    assert!(ledger.escalations[0].handled);
}

/// AC-P2-4, fold half.
#[test]
fn ruling_fix_reopens_and_resets_rejects() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Fix).unwrap();
    let f = ledger.finding(&f1).unwrap();
    assert_eq!(f.status, FindingStatus::Open);
    assert_eq!(f.reject_count, 0);
    assert!(ledger.pending_ruling().is_empty());
}

/// P2-§4 rule 1.
#[test]
fn reject_after_fix_is_ignored_by_fold() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Fix).unwrap();
    rebut(&mut ledger, &f1, RebuttalAnswer::Reject, 3);
    rebut(&mut ledger, &f1, RebuttalAnswer::Reject, 4);
    assert_eq!(ledger.finding(&f1).unwrap().reject_count, 0);
    assert_eq!(ledger.escalations.len(), 1, "no new escalation");
}

/// P2-§4 rule 3, AC-P2-3.
#[test]
fn reviewer_reopening_dropped_finding_is_damage() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Drop).unwrap();
    assert_eq!(
        set_status(&mut ledger, &f1, FindingStatus::Open),
        Err(FoldError::ReopenAfterDrop(f1.clone()))
    );
    assert_eq!(
        set_status(&mut ledger, &f1, FindingStatus::Disputed),
        Err(FoldError::ReopenAfterDrop(f1.clone()))
    );
    assert_eq!(
        set_status(&mut ledger, &f1, FindingStatus::Resolved),
        Ok(())
    );
}

/// P2-§4 rule 4.
#[test]
fn disputed_after_fix_is_damage() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Fix).unwrap();
    assert_eq!(
        set_status(&mut ledger, &f1, FindingStatus::Disputed),
        Err(FoldError::IllegalStatusAfterFix {
            id: f1.clone(),
            status: FindingStatus::Disputed,
        })
    );
    // QA P2: "only `open` or `resolved` is legal" — withdrawal included.
    assert_eq!(
        set_status(&mut ledger, &f1, FindingStatus::Withdrawn),
        Err(FoldError::IllegalStatusAfterFix {
            id: f1.clone(),
            status: FindingStatus::Withdrawn,
        })
    );
    assert_eq!(set_status(&mut ledger, &f1, FindingStatus::Open), Ok(()));
    assert_eq!(
        set_status(&mut ledger, &f1, FindingStatus::Resolved),
        Ok(())
    );
}

#[test]
fn ruling_on_unissued_finding_is_damage() {
    let mut ledger = Ledger::default();
    assert_eq!(
        rule(&mut ledger, "F9", RulingDecision::Drop),
        Err(FoldError::RulingForUnissuedFinding("F9".to_string()))
    );
}

/// AC-P2-9: a proactive `fix` (no escalation) folds like AC-P2-4.
#[test]
fn proactive_fix_without_escalation_folds_like_ac4() {
    let mut escalated = Ledger::default();
    let f1 = escalated_f1(&mut escalated);
    rule(&mut escalated, &f1, RulingDecision::Fix).unwrap();

    let mut proactive = Ledger::default();
    let p1 = issue(&mut proactive, Severity::High, "x", 1);
    set_status(&mut proactive, &p1, FindingStatus::Disputed).unwrap();
    rebut(&mut proactive, &p1, RebuttalAnswer::Reject, 1);
    rule(&mut proactive, &p1, RulingDecision::Fix).unwrap();

    assert_eq!(proactive.finding(&p1), escalated.finding(&f1));
    assert!(proactive.escalations.is_empty());
    assert!(proactive.pending_ruling().is_empty());
}

#[test]
fn second_ruling_wins() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Fix).unwrap();
    rule(&mut ledger, &f1, RulingDecision::Drop).unwrap();
    let f = ledger.finding(&f1).unwrap();
    assert_eq!(f.ruled, Some(RulingDecision::Drop));
    assert_eq!(f.status, FindingStatus::Resolved);
}

#[test]
fn binding_rulings_clear_per_role_on_turn_sent() {
    let mut ledger = Ledger::default();
    let f1 = escalated_f1(&mut ledger);
    rule(&mut ledger, &f1, RulingDecision::Drop).unwrap();
    assert_eq!(ledger.binding_rulings(Role::Main).len(), 1);
    assert_eq!(ledger.binding_rulings(Role::Reviewer).len(), 1);
    assert_eq!(ledger.binding_rulings(Role::Main)[0].finding, f1);

    ledger
        .apply(&ReviewPayload::TurnSent {
            round: 3,
            to: Role::Main,
            restart_note: None,
            human_wait_ms: 0,
        })
        .unwrap();
    assert!(ledger.binding_rulings(Role::Main).is_empty());
    assert_eq!(ledger.binding_rulings(Role::Reviewer).len(), 1);
}

/// P2-§5.1 step 3: the ruling prompt shows main's last position, so the
/// fold keeps the reason from the latest `reject` (derived, replay-safe).
#[test]
fn rebuttal_reject_records_last_reason() {
    let mut ledger = Ledger::default();
    let f1 = issue(&mut ledger, Severity::High, "x", 1);
    assert_eq!(ledger.finding(&f1).unwrap().last_reject_reason, None);
    for (round, reason) in [(1, "first"), (2, "second")] {
        ledger
            .apply(&ReviewPayload::Rebuttal {
                round,
                responses: vec![RebuttalResponseDto {
                    id: f1.clone(),
                    answer: RebuttalAnswer::Reject,
                    reason: Some(reason.to_string()),
                }],
                cumulative: cum(0, 0),
            })
            .unwrap();
    }
    assert_eq!(
        ledger.finding(&f1).unwrap().last_reject_reason.as_deref(),
        Some("second")
    );
}

// ---- P3a-§6.1: human notes ----

use crate::cmd::fleet::review::schema::HumanNote;

fn note(ledger: &mut Ledger, text: &str, target: Option<Role>) {
    let n = HumanNote {
        text: text.into(),
        target,
    };
    ledger.apply(&n.into()).unwrap();
}

fn sent_to(ledger: &mut Ledger, to: Role) {
    ledger
        .apply(&ReviewPayload::TurnSent {
            round: 1,
            to,
            restart_note: None,
            human_wait_ms: 0,
        })
        .unwrap();
}

fn texts(ledger: &Ledger, role: Role) -> Vec<&str> {
    ledger
        .unseen_notes(role)
        .iter()
        .map(|n| n.text.as_str())
        .collect()
}

#[test]
fn broadcast_note_queues_for_both_sides() {
    let mut ledger = Ledger::default();
    note(&mut ledger, "a", None);
    assert_eq!(texts(&ledger, Role::Main), ["a"]);
    assert_eq!(texts(&ledger, Role::Reviewer), ["a"]);
}

#[test]
fn targeted_note_queues_only_for_its_side() {
    let mut ledger = Ledger::default();
    note(&mut ledger, "m", Some(Role::Main));
    note(&mut ledger, "r", Some(Role::Reviewer));
    assert_eq!(texts(&ledger, Role::Main), ["m"]);
    assert_eq!(texts(&ledger, Role::Reviewer), ["r"]);
}

#[test]
fn turn_sent_clears_only_that_sides_notes() {
    let mut ledger = Ledger::default();
    note(&mut ledger, "a", None);
    note(&mut ledger, "b", Some(Role::Reviewer));
    sent_to(&mut ledger, Role::Main);
    assert!(ledger.unseen_notes(Role::Main).is_empty());
    assert_eq!(texts(&ledger, Role::Reviewer), ["a", "b"]);
}

#[test]
fn note_never_touches_findings_or_round() {
    let mut ledger = Ledger::default();
    issue(&mut ledger, Severity::High, "x", 1);
    let before = (
        ledger.findings.clone(),
        ledger.round,
        ledger.escalations.clone(),
    );
    note(&mut ledger, "a", None);
    assert_eq!(
        (
            ledger.findings.clone(),
            ledger.round,
            ledger.escalations.clone()
        ),
        before
    );
}

#[test]
fn notes_and_rulings_keep_independent_queues() {
    for note_first in [true, false] {
        let mut ledger = Ledger::default();
        let f1 = escalated_f1(&mut ledger);
        if note_first {
            note(&mut ledger, "n", None);
            rule(&mut ledger, &f1, RulingDecision::Drop).unwrap();
        } else {
            rule(&mut ledger, &f1, RulingDecision::Drop).unwrap();
            note(&mut ledger, "n", None);
        }
        assert_eq!(ledger.binding_rulings(Role::Main).len(), 1);
        assert_eq!(texts(&ledger, Role::Main), ["n"]);
        sent_to(&mut ledger, Role::Main);
        assert!(ledger.binding_rulings(Role::Main).is_empty());
        assert!(ledger.unseen_notes(Role::Main).is_empty());
        assert_eq!(ledger.binding_rulings(Role::Reviewer).len(), 1);
        assert_eq!(texts(&ledger, Role::Reviewer), ["n"]);
    }
}
