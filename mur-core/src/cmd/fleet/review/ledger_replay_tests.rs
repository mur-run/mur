//! §3.3.1 round sealing: replay drops the events of an unsealed round
//! (AC2a) but keeps their cumulative limits as a lower bound (AC2b).

use std::time::Duration;

use mur_common::limits::Stuck;

use super::ledger::{Ledger, fold_rounds};
use super::schema::{
    Cumulative, FindingStatus, Mode, PauseKind, RebuttalAnswer, RebuttalResponseDto, ReviewPayload,
    Role, RulingDecision, SessionLimits, Severity, VerdictKind,
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
        human_wait_ms: 0,
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
            kind: PauseKind::Other,
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

// ---- Phase 2: replay across rulings (AC-P2-10) ----

fn ruling(id: &str, decision: RulingDecision) -> ReviewPayload {
    ReviewPayload::Ruling {
        finding: id.into(),
        decision,
        text: "human says so".into(),
    }
}

/// The live driver's view of a fully sealed log: every event applied in
/// order, the open set snapshotted at each verdict (`loop_driver.rs`).
fn live(log: &[ReviewPayload]) -> Ledger {
    let mut l = Ledger::default();
    for p in log {
        l.apply(p).unwrap();
        if matches!(p, ReviewPayload::Verdict { .. }) {
            l.note_round_complete();
        }
    }
    l
}

/// Round 1 sealed, F1 escalated in round 2 (two rejects across rounds 1–2).
fn escalated_log() -> Vec<ReviewPayload> {
    let mut log = sealed_round_one();
    log.extend([
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        reject_f1(2, cum(200, 2)),
        verdict(2, cum(200, 2)),
        sent(3, Role::Main),
        sent(3, Role::Reviewer),
        reject_f1(3, cum(300, 3)),
        verdict(3, cum(300, 3)),
    ]);
    log
}

/// AC-P2-10: for every sealed log, replay equals the live ledger, rulings,
/// `handled`, and per-role binding state included. Rulings sit at each
/// position the driver can write them: after a seal (reviewer prompt, held
/// ruling) and before main's `turn_sent` (main prompt).
#[test]
fn sealed_logs_with_rulings_replay_equal_to_live() {
    for decision in [RulingDecision::Drop, RulingDecision::Fix] {
        // Held ruling: written after round 3's seal, read by round 4.
        let mut held = escalated_log();
        held.push(ruling("F1", decision));
        // Main-prompt ruling, then round 4 sent to main only so far.
        let mut main_prompt = held.clone();
        main_prompt.push(sent(4, Role::Main));
        // ... and round 4 sealed.
        let mut sealed = main_prompt.clone();
        sealed.extend([sent(4, Role::Reviewer), verdict(4, cum(400, 4))]);

        for (name, log) in [("held", &held), ("sealed", &sealed)] {
            assert_eq!(fold_rounds(log).unwrap(), live(log), "{name} {decision:?}");
        }
        let l = fold_rounds(&sealed).unwrap();
        assert!(l.pending_ruling().is_empty());
        assert!(l.escalations.iter().all(|e| e.handled));
        assert_eq!(l.finding("F1").unwrap().ruled, Some(decision));
        assert!(l.binding_rulings(Role::Main).is_empty(), "{decision:?}");
        assert!(l.binding_rulings(Role::Reviewer).is_empty(), "{decision:?}");
    }
}

/// P2-§2: a ruling carries no round, so it survives the drop of an
/// unsealed trailing round — and the dropped round's `turn_sent` does not
/// count as delivery, so the re-run round gets the binding note again.
#[test]
fn a_ruling_survives_a_dropped_trailing_round_and_stays_binding() {
    let mut log = escalated_log();
    log.extend([
        ruling("F1", RulingDecision::Fix),
        sent(4, Role::Main),
        sent(4, Role::Reviewer),
        reject_f1(4, cum(450, 5)),
    ]);
    let l = fold_rounds(&log).unwrap();
    assert_eq!(l.round, 3, "round 4 is re-run");
    let f = l.finding("F1").unwrap();
    assert_eq!(f.ruled, Some(RulingDecision::Fix));
    assert_eq!((f.status, f.reject_count), (FindingStatus::Open, 0));
    assert!(l.pending_ruling().is_empty());
    assert_eq!(l.binding_rulings(Role::Main).len(), 1);
    assert_eq!(l.binding_rulings(Role::Reviewer).len(), 1);
    assert_eq!((l.exec_time_ms, l.cost_usd_micros), (450, 5));
}
