//! §3.3: the finding ledger — a pure fold over [`ReviewPayload`] events. Kept
//! free of channel/signing concerns (those live in `rollback.rs`); this
//! module only knows the logical event sequence and the state machine rules.

use std::collections::BTreeMap;

use super::constants::{REJECT_ESCALATION_THRESHOLD, ROUND_STUCK_AFTER_UNCHANGED_ROUNDS};
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
    pub fn disputed_high_severity(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == FindingStatus::Disputed && f.severity == Severity::High)
            .collect()
    }

    /// High-severity findings still `open` (not disputed). They do not
    /// block `approve` (§3.3) but are called out on the stop screen (#1721).
    pub fn open_high_severity(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == FindingStatus::Open && f.severity == Severity::High)
            .collect()
    }

    /// §3.3: "A medium/low `disputed` finding does not block `approve`, but
    /// it is listed first in the human summary." Used by the stop screen
    /// (§8.3, AC10, AC13).
    #[allow(dead_code)] // not wired yet: stop-screen summary
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
                        // AC8: the SAME finding rejected
                        // REJECT_ESCALATION_THRESHOLD times escalates
                        // automatically. `==` (not `>=`) so it escalates once.
                        if f.reject_count == REJECT_ESCALATION_THRESHOLD {
                            self.escalations.push(EscalationRecord {
                                finding_id: r.id.clone(),
                                reason: "rejected twice by the main agent".to_string(),
                            });
                        }
                    }
                }
            }
            ReviewPayload::HumanNote { .. } => {}
            // Phase 2 fold rules land in the next commit (P2-§4).
            ReviewPayload::Ruling { .. } => {}
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

    /// §3.3.1 AC2b: an unsealed round's ledger effect is dropped, but the
    /// time and cost it carried still count as a lower bound.
    fn adopt_limits_from(&mut self, other: &Ledger) {
        self.exec_time_ms = self.exec_time_ms.max(other.exec_time_ms);
        self.cost_usd_micros = self.cost_usd_micros.max(other.cost_usd_micros);
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

/// Fold with round sealing (§3.3.1). A round's `rebuttal`, `finding_issued`
/// and `finding_status` events are staged on a scratch copy and only enter
/// the ledger when that round's `verdict` (always appended last) seals it;
/// [`Ledger::note_round_complete`] is called at that seal, as the live loop
/// does (AC9 + AC11). A new main `turn_sent` starts a new attempt and
/// discards an unsealed one, so a round re-run after a crash never counts a
/// reject twice (AC2a). Discarded events still raise the cumulative limits
/// (AC2b). Staged events are still validated, so an illegal transition in an
/// unsealed round remains an error.
pub fn fold_rounds(payloads: &[ReviewPayload]) -> Result<Ledger, FoldError> {
    let mut sealed = Ledger::default();
    let mut attempt: Option<(u32, Ledger)> = None;
    for p in payloads {
        let round = super::schema::payload_round(p);
        let new_attempt = match p {
            ReviewPayload::TurnSent { to, .. } => *to == super::schema::Role::Main,
            _ => false,
        } || matches!((round, &attempt), (Some(r), Some((a, _))) if r != *a);
        if new_attempt && let Some((_, dropped)) = attempt.take() {
            sealed.adopt_limits_from(&dropped);
        }
        match (p, round) {
            (ReviewPayload::TurnSent { .. }, _) => {}
            (_, Some(r)) => {
                let (_, scratch) = attempt.get_or_insert_with(|| (r, sealed.clone()));
                scratch.apply(p)?;
                if matches!(p, ReviewPayload::Verdict { .. }) {
                    let (_, mut done) = attempt.take().expect("attempt was just set");
                    done.note_round_complete();
                    sealed = done;
                }
            }
            (_, None) => {
                sealed.apply(p)?;
                if let Some((_, scratch)) = attempt.as_mut() {
                    scratch.apply(p)?;
                }
            }
        }
    }
    if let Some((_, dropped)) = attempt {
        sealed.adopt_limits_from(&dropped);
    }
    Ok(sealed)
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod ledger_tests;
