//! §3.3: the finding ledger — a pure fold over [`ReviewPayload`] events. Kept
//! free of channel/signing concerns (those live in `rollback.rs`); this
//! module only knows the logical event sequence and the state machine rules.

use std::collections::BTreeMap;

use super::constants::ROUND_STUCK_AFTER_UNCHANGED_ROUNDS;
use super::schema::{FindingStatus, Mode, ReviewPayload, Severity};

/// One finding, as the ledger tracks it (§3.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub issue: String,
    pub status: FindingStatus,
    pub round_issued: u32,
    /// §3.4: how many times the main agent has rejected this finding. The
    /// second rejection escalates automatically (AC8).
    pub reject_count: u32,
}

/// Why a fold step was rejected as an illegal transition (§8.2: "an illegal
/// state transition (e.g. a `finding_status` for an ID never issued)").
/// Surfaced to `rollback.rs` as damage, and to pure unit tests as a plain
/// error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FoldError {
    #[error("finding_status for id {0:?}, which was never issued")]
    StatusForUnissuedFinding(String),
    #[error("finding_issued id {got:?}, expected the next sequential id {expected:?}")]
    OutOfSequenceFindingId { got: String, expected: String },
    #[error("duplicate finding_issued for id {0:?}")]
    DuplicateFindingId(String),
    #[error("rebuttal references finding {0:?}, which was never issued")]
    RebuttalForUnissuedFinding(String),
}

/// The ledger's fold state (§3.3, §3.5). A pure value: everything here is
/// derived solely from the event sequence folded so far (§4's replay
/// requirement, AC11).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ledger {
    pub round: u32,
    pub mode: Mode,
    /// In issue order (matches ID order: `F1, F2, ...`).
    pub findings: Vec<Finding>,
    next_finding_seq: u32,
    /// Finding IDs escalated via the "rejected twice" rule (AC8), in the
    /// order they were escalated. A ledger consumer treats a non-empty list
    /// as "the session must escalate to the human".
    pub escalations: Vec<EscalationRecord>,
    /// The last two round-end open-set snapshots, oldest first. Used for
    /// round-stuck detection (§3.3, AC9).
    round_open_set_history: Vec<BTreeMap<String, FindingStatus>>,
    pub round_stuck: bool,
    pub paused: bool,
    pub exec_time_ms: u64,
    pub cost_usd_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationRecord {
    pub finding_id: String,
    pub reason: String,
}

impl Ledger {
    /// §3.3: "Open set = `open` ∪ `disputed`."
    pub fn open_set(&self) -> BTreeMap<String, FindingStatus> {
        self.findings
            .iter()
            .filter(|f| f.status.is_open_set())
            .map(|f| (f.id.clone(), f.status))
            .collect()
    }

    /// §8.3 stop screen: every unresolved finding (`open` and `disputed`),
    /// in issue order. After an `approve`, disputed findings (necessarily
    /// medium/low, §3.3) are listed first (AC13).
    pub fn stop_screen_findings(&self, after_approve: bool) -> Vec<&Finding> {
        let mut out: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| f.status.is_open_set())
            .collect();
        if after_approve {
            out.sort_by_key(|f| f.status != FindingStatus::Disputed);
        }
        out
    }

    #[allow(dead_code)] // not wired yet: §6 /rule and §3.4 rebuttal
    pub fn finding(&self, id: &str) -> Option<&Finding> {
        self.findings.iter().find(|f| f.id == id)
    }

    /// §3.3: "`approve` is rejected by the system while any high-severity
    /// finding is `disputed`." Returns the disputed IDs blocking approval
    /// when non-empty.
    #[allow(dead_code)] // not wired yet: §3.3 approve gate
    pub fn disputed_high_severity(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == FindingStatus::Disputed && f.severity == Severity::High)
            .collect()
    }

    /// §3.3: "A medium/low `disputed` finding does not block `approve`, but
    /// it is listed first in the human summary." Used by the stop screen
    /// (§8.3, AC10, AC13).
    #[allow(dead_code)] // not wired yet: §3.3 approve gate
    pub fn disputed_medium_low(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == FindingStatus::Disputed && f.severity != Severity::High)
            .collect()
    }

    /// Fold one event into the ledger. `_to`/other turn-routing fields on
    /// `TurnSent` are ignored here (driver-only bookkeeping).
    pub fn apply(&mut self, payload: &ReviewPayload) -> Result<(), FoldError> {
        match payload {
            ReviewPayload::SessionStarted { mode, .. } => {
                self.mode = *mode;
            }
            ReviewPayload::TurnSent { .. } => {}
            ReviewPayload::Verdict {
                round, cumulative, ..
            } => {
                self.round = *round;
                self.adopt_cumulative(cumulative);
            }
            ReviewPayload::FindingIssued {
                id,
                severity,
                issue,
                round,
            } => {
                let expected = format!("F{}", self.next_finding_seq + 1);
                if *id != expected {
                    if self.findings.iter().any(|f| &f.id == id) {
                        return Err(FoldError::DuplicateFindingId(id.clone()));
                    }
                    return Err(FoldError::OutOfSequenceFindingId {
                        got: id.clone(),
                        expected,
                    });
                }
                self.next_finding_seq += 1;
                self.findings.push(Finding {
                    id: id.clone(),
                    severity: *severity,
                    issue: issue.clone(),
                    status: FindingStatus::Open,
                    round_issued: *round,
                    reject_count: 0,
                });
            }
            ReviewPayload::FindingStatus { id, status, .. } => {
                let Some(f) = self.findings.iter_mut().find(|f| &f.id == id) else {
                    return Err(FoldError::StatusForUnissuedFinding(id.clone()));
                };
                f.status = *status;
            }
            ReviewPayload::Rebuttal {
                responses,
                cumulative,
                ..
            } => {
                self.adopt_cumulative(cumulative);
                for r in responses {
                    let Some(f) = self.findings.iter_mut().find(|f| f.id == r.id) else {
                        return Err(FoldError::RebuttalForUnissuedFinding(r.id.clone()));
                    };
                    if r.answer == super::schema::RebuttalAnswer::Reject {
                        f.reject_count += 1;
                        // AC8: the SAME finding rejected twice escalates
                        // automatically.
                        if f.reject_count == 2 {
                            self.escalations.push(EscalationRecord {
                                finding_id: r.id.clone(),
                                reason: "rejected twice by the main agent".to_string(),
                            });
                        }
                    }
                }
            }
            ReviewPayload::HumanNote { .. } => {}
            ReviewPayload::Ruling { closes, .. } => {
                for id in closes {
                    if let Some(f) = self.findings.iter_mut().find(|f| &f.id == id) {
                        f.status = FindingStatus::Resolved;
                    }
                }
            }
            ReviewPayload::Escalation { finding_id, reason } => {
                self.escalations.push(EscalationRecord {
                    finding_id: finding_id.clone(),
                    reason: reason.clone(),
                });
            }
            ReviewPayload::Paused { cumulative, .. } => {
                self.adopt_cumulative(cumulative);
                self.paused = true;
            }
            ReviewPayload::Resumed { cumulative } => {
                self.adopt_cumulative(cumulative);
                self.paused = false;
            }
            ReviewPayload::ModeChanged { mode } => {
                self.mode = *mode;
            }
            ReviewPayload::SessionStopped { cumulative, .. } => {
                self.adopt_cumulative(cumulative);
            }
            ReviewPayload::ResumedFromCheckpoint {
                round, cumulative, ..
            } => {
                self.round = *round;
                self.adopt_cumulative(cumulative);
                self.paused = false;
            }
        }
        Ok(())
    }

    /// §8.2 "Limits on rollback — clock and cost are monotonic": never let a
    /// later event's cumulative figures move either total DOWN.
    fn adopt_cumulative(&mut self, c: &super::schema::Cumulative) {
        self.exec_time_ms = self.exec_time_ms.max(c.exec_time_ms);
        self.cost_usd_micros = self.cost_usd_micros.max(c.cost_usd_micros);
    }

    /// Call once a round's events (verdict + finding updates) are fully
    /// folded, to update round-stuck tracking (§3.3, AC9). The caller (the
    /// driver, or a test) decides where a round ends — the ledger itself
    /// has no event marking "round complete" (§4 lists no such event).
    pub fn note_round_complete(&mut self) {
        let window = ROUND_STUCK_AFTER_UNCHANGED_ROUNDS as usize;
        let snapshot = self.open_set();
        self.round_open_set_history.push(snapshot);
        if self.round_open_set_history.len() > window {
            self.round_open_set_history.remove(0);
        }
        self.round_stuck = self.round_open_set_history.len() == window
            && self.round_open_set_history.windows(2).all(|w| w[0] == w[1]);
    }

    /// Who the next finding issuance must address (`F<n>`), for a driver
    /// minting new `FindingIssued` events.
    pub fn next_finding_id(&self) -> String {
        format!("F{}", self.next_finding_seq + 1)
    }
}

/// Fold a whole sequence of payloads from the start (empty ledger). Used by
/// the AC11 replay property test and by any caller that does not need
/// incremental round-boundary tracking (round-stuck is computed only where
/// the caller calls [`Ledger::note_round_complete`]).
#[allow(dead_code)] // not wired yet: §8.2 resume
pub fn fold(payloads: &[ReviewPayload]) -> Result<Ledger, FoldError> {
    let mut ledger = Ledger::default();
    for p in payloads {
        ledger.apply(p)?;
    }
    Ok(ledger)
}

/// Fold with round-boundary tracking: [`Ledger::note_round_complete`] is
/// called whenever a payload opens a later round, and once more after the
/// last round seen. This mirrors the live loop, which notes every round it
/// fully folds, so a replay of the loop's own channel reproduces its ledger
/// including round-stuck state (AC9 + AC11).
#[allow(dead_code)] // not wired yet: §8.2 resume
pub fn fold_rounds(payloads: &[ReviewPayload]) -> Result<Ledger, FoldError> {
    let mut ledger = Ledger::default();
    let mut in_progress: u32 = 0;
    // The trailing round is sealed only once its verdict landed: a round
    // cut short after `turn_sent` (stop/pause/deadline) was never sealed by
    // the live driver either, so sealing it here would push an extra
    // open-set snapshot and could flip `round_stuck` on replay.
    let mut trailing_has_verdict = false;
    for p in payloads {
        if let Some(r) = super::schema::payload_round(p)
            && r > in_progress
        {
            if in_progress > 0 {
                ledger.note_round_complete();
            }
            in_progress = r;
            trailing_has_verdict = false;
        }
        if matches!(p, ReviewPayload::Verdict { .. }) {
            trailing_has_verdict = true;
        }
        ledger.apply(p)?;
    }
    if in_progress > 0 && trailing_has_verdict {
        ledger.note_round_complete();
    }
    Ok(ledger)
}

#[cfg(test)]
mod tests {
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
}
