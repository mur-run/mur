//! §3.3.1 round sealing: replay drops the events of an unsealed round
//! (AC2a) but keeps their cumulative limits as a lower bound (AC2b).

use std::time::Duration;

use mur_common::limits::Stuck;

use super::ledger::{Ledger, fold_rounds};
use super::schema::{
    Cumulative, FindingStatus, Mode, RebuttalAnswer, RebuttalResponseDto, ReviewPayload, Role,
    SessionLimits, Severity, VerdictKind,
};
use super::verdict::parse_verdict;

fn cum(ms: u64, micros: u64) -> Cumulative {
    Cumulative {
        exec_time_ms: ms,
        cost_usd_micros: micros,
    }
}

fn started() -> ReviewPayload {
    ReviewPayload::SessionStarted {
        members: ["main".into(), "reviewer".into()],
        mode: Mode::SemiAuto,
        limits: SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
    }
}

fn sent(round: u32, to: Role) -> ReviewPayload {
    ReviewPayload::TurnSent {
        round,
        to,
        restart_note: None,
    }
}

fn reject_f1(round: u32, c: Cumulative) -> ReviewPayload {
    ReviewPayload::Rebuttal {
        round,
        responses: vec![RebuttalResponseDto {
            id: "F1".into(),
            answer: RebuttalAnswer::Reject,
            reason: Some("disagree".into()),
        }],
        cumulative: c,
    }
}

fn verdict(round: u32, c: Cumulative) -> ReviewPayload {
    ReviewPayload::Verdict {
        round,
        kind: VerdictKind::Revise,
        cumulative: c,
    }
}

/// Round 1 sealed: F1 (high) issued, verdict last (§3.3.1 order).
fn sealed_round_one() -> Vec<ReviewPayload> {
    vec![
        started(),
        sent(1, Role::Main),
        sent(1, Role::Reviewer),
        ReviewPayload::FindingIssued {
            round: 1,
            id: "F1".into(),
            severity: Severity::High,
            issue: "x".into(),
        },
        verdict(1, cum(100, 1)),
    ]
}

fn reject_count(l: &Ledger) -> u32 {
    l.finding("F1").unwrap().reject_count
}

/// AC2a + AC2b: a trailing rebuttal with no verdict is not in the ledger,
/// but its cumulative figures still raise the limits.
#[test]
fn an_unsealed_trailing_rebuttal_is_dropped_but_its_limits_are_kept() {
    let mut log = sealed_round_one();
    log.extend([
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        reject_f1(2, cum(500, 7)),
    ]);
    let l = fold_rounds(&log).unwrap();
    assert_eq!(reject_count(&l), 0);
    assert!(l.escalations.is_empty());
    assert_eq!(l.round, 1, "resume restarts at round 2");
    assert_eq!((l.exec_time_ms, l.cost_usd_micros), (500, 7));
}

/// AC2a: the crashed round is re-run under the SAME round number. The first
/// attempt's reject must not be counted together with the re-run's.
#[test]
fn a_rerun_round_counts_a_reject_once_and_escalates_exactly_once() {
    let mut log = sealed_round_one();
    log.extend([
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        reject_f1(2, cum(500, 7)),
        // crash; resume appends paused(crashed) + resumed, then re-runs round 2
        ReviewPayload::Paused {
            reason: "crashed".into(),
            cumulative: cum(500, 7),
        },
        ReviewPayload::Resumed {
            cumulative: cum(500, 7),
        },
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        reject_f1(2, cum(600, 8)),
        verdict(2, cum(600, 8)),
    ]);
    let l = fold_rounds(&log).unwrap();
    assert_eq!(reject_count(&l), 1);
    assert!(l.escalations.is_empty());

    log.extend([
        sent(3, Role::Main),
        sent(3, Role::Reviewer),
        reject_f1(3, cum(700, 9)),
        verdict(3, cum(700, 9)),
    ]);
    let l = fold_rounds(&log).unwrap();
    assert_eq!(reject_count(&l), 2);
    assert_eq!(l.escalations.len(), 1, "AC8 counted once, not twice");
}

/// A fresh `turn_sent` to main also starts a new attempt, even with no
/// paused/resumed pair in between.
#[test]
fn a_new_main_turn_discards_the_previous_unsealed_attempt() {
    let mut log = sealed_round_one();
    log.extend([
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        reject_f1(2, cum(500, 7)),
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        reject_f1(2, cum(600, 8)),
        verdict(2, cum(600, 8)),
    ]);
    assert_eq!(reject_count(&fold_rounds(&log).unwrap()), 1);
}

/// AC2a: trailing `finding_issued` / `finding_status` without a verdict are
/// not in the ledger either.
#[test]
fn unsealed_trailing_findings_and_statuses_are_dropped() {
    let mut log = sealed_round_one();
    log.extend([
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        ReviewPayload::FindingIssued {
            round: 2,
            id: "F2".into(),
            severity: Severity::Low,
            issue: "y".into(),
        },
        ReviewPayload::FindingStatus {
            round: 2,
            id: "F1".into(),
            status: FindingStatus::Resolved,
            reason: None,
        },
    ]);
    let l = fold_rounds(&log).unwrap();
    assert!(l.finding("F2").is_none());
    assert_eq!(l.finding("F1").unwrap().status, FindingStatus::Open);
    assert_eq!(l.next_finding_id(), "F2", "the dropped F2 id is reissued");
}

/// AC2a: the live driver's staged round ends with `verdict`, the seal.
#[test]
fn the_live_append_order_ends_with_the_verdict() {
    let ledger = fold_rounds(&sealed_round_one()).unwrap();
    let staged = parse_verdict(
        &ledger,
        2,
        r#"{"verdict":"revise","findings":[{"severity":"low","issue":"y"}],
            "prior":[{"id":"F1","status":"resolved"}]}"#,
    )
    .unwrap();
    let kinds: Vec<&str> = staged
        .payloads
        .iter()
        .map(|p| match p {
            ReviewPayload::FindingIssued { .. } => "issued",
            ReviewPayload::FindingStatus { .. } => "status",
            ReviewPayload::Verdict { .. } => "verdict",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["issued", "status", "verdict"]);
}
