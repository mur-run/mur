//! §3.3: the finding ledger — a pure fold over [`ReviewPayload`] events. Kept
//! free of channel/signing concerns (those live in `rollback.rs`); this
//! module only knows the logical event sequence and the state machine rules.

use std::collections::BTreeMap;

use super::constants::{REJECT_ESCALATION_THRESHOLD, ROUND_STUCK_AFTER_UNCHANGED_ROUNDS};
use super::schema::{
    FindingStatus, HumanNote, Mode, ReviewPayload, Role, RulingDecision, Severity,
};

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
    /// P2-§4: the latest human ruling on this finding, if any. `Fix`
    /// freezes `reject_count` (rule 1) and forbids `disputed` (rule 4);
    /// `Drop` forbids any status but `resolved` (rule 3).
    pub ruled: Option<RulingDecision>,
    /// P2-§5.1 step 3: main's reason on its latest `reject` of this
    /// finding, shown at the ruling prompt. Derived from `rebuttal` events.
    pub last_reject_reason: Option<String>,
    /// P2-§5.1 step 3 (QA P3): the reviewer's reason on its latest
    /// `finding_status` for this finding, shown at the ruling prompt in
    /// place of the original issue. Derived from `finding_status` events.
    pub last_reviewer_reason: Option<String>,
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
    #[error("ruling references finding {0:?}, which was never issued")]
    RulingForUnissuedFinding(String),
    #[error("finding_status reopens finding {0:?}, which a ruling dropped")]
    ReopenAfterDrop(String),
    /// P2-§4 rule 4: after a `fix` ruling only `open` or `resolved` is legal.
    #[error(
        "finding_status marks finding {id:?} {status:?} after a fix ruling; only open or resolved is legal"
    )]
    IllegalStatusAfterFix {
        id: String,
        status: super::schema::FindingStatus,
    },
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
    /// Rulings each role has not yet been sent, indexed by [`role_slot`].
    /// A `ruling` pushes to both; a `turn_sent { to }` clears that role's
    /// list (P2-§5.3 binding note). Event-derived, so replay matches.
    unseen_rulings: [Vec<RulingRecord>; 2],
    /// Human notes each role has not yet been sent, indexed by
    /// [`role_slot`] (P3a-§6.1). Same clear rule as `unseen_rulings`.
    unseen_notes: [Vec<HumanNote>; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationRecord {
    pub finding_id: String,
    pub reason: String,
    /// P2-§4: set when a ruling on `finding_id` folds.
    pub handled: bool,
}

/// One folded ruling, as a binding note carries it (P2-§5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulingRecord {
    pub finding: String,
    pub decision: RulingDecision,
    pub text: String,
}

fn role_slot(role: Role) -> usize {
    match role {
        Role::Main => 0,
        Role::Reviewer => 1,
    }
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

    /// P2-§4 "Single definition": the escalations still owed a ruling.
    /// The stop check, the resume decision, and the stop/resume screens
    /// all call this one function.
    pub fn pending_ruling(&self) -> Vec<&EscalationRecord> {
        self.escalations.iter().filter(|e| !e.handled).collect()
    }

    /// Rulings not yet delivered to `role` (P2-§5.3 binding note).
    pub fn binding_rulings(&self, role: Role) -> &[RulingRecord] {
        &self.unseen_rulings[role_slot(role)]
    }

    /// Human notes not yet delivered to `role` (P3a-§6.1).
    #[allow(dead_code)] // wired in PR 3 (Task 5–7)
    pub fn unseen_notes(&self, role: Role) -> &[HumanNote] {
        &self.unseen_notes[role_slot(role)]
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
            ReviewPayload::TurnSent { to, .. } => {
                self.unseen_rulings[role_slot(*to)].clear();
                self.unseen_notes[role_slot(*to)].clear();
            }
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
                    ruled: None,
                    last_reject_reason: None,
                    last_reviewer_reason: None,
                });
            }
            ReviewPayload::FindingStatus {
                id, status, reason, ..
            } => {
                let Some(f) = self.findings.iter_mut().find(|f| &f.id == id) else {
                    return Err(FoldError::StatusForUnissuedFinding(id.clone()));
                };
                // P2-§4 rules 3 and 4. Rule 4 is an allowlist ("only `open` or
                // `resolved` is legal"); the match is exhaustive over the rest.
                match (f.ruled, *status) {
                    (Some(RulingDecision::Drop), s) if s != FindingStatus::Resolved => {
                        return Err(FoldError::ReopenAfterDrop(id.clone()));
                    }
                    (
                        Some(RulingDecision::Fix),
                        FindingStatus::Disputed | FindingStatus::Withdrawn,
                    ) => {
                        return Err(FoldError::IllegalStatusAfterFix {
                            id: id.clone(),
                            status: *status,
                        });
                    }
                    _ => {}
                }
                f.status = *status;
                if let Some(r) = reason.as_deref().filter(|r| !r.trim().is_empty()) {
                    f.last_reviewer_reason = Some(r.to_string());
                }
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
                        f.last_reject_reason.clone_from(&r.reason);
                    }
                    // P2-§4 rule 1: after `fix` a reject is malformed at the
                    // driver; the fold ignores it as defence in depth.
                    if r.answer == super::schema::RebuttalAnswer::Reject
                        && f.ruled != Some(RulingDecision::Fix)
                    {
                        f.reject_count += 1;
                        // AC8: the SAME finding rejected
                        // REJECT_ESCALATION_THRESHOLD times escalates
                        // automatically. `==` (not `>=`) so it escalates once.
                        if f.reject_count == REJECT_ESCALATION_THRESHOLD {
                            self.escalations.push(EscalationRecord {
                                finding_id: r.id.clone(),
                                reason: "rejected twice by the main agent".to_string(),
                                handled: false,
                            });
                        }
                    }
                }
            }
            ReviewPayload::HumanNote { text, target } => {
                // P3a-§6.1: broadcast queues for both sides, `@` for one.
                let note = HumanNote {
                    text: text.clone(),
                    target: *target,
                };
                match target {
                    Some(role) => self.unseen_notes[role_slot(*role)].push(note),
                    None => {
                        for unseen in &mut self.unseen_notes {
                            unseen.push(note.clone());
                        }
                    }
                }
            }
            ReviewPayload::Ruling {
                finding,
                decision,
                text,
            } => {
                // P2-§4 rule 2.
                let Some(f) = self.findings.iter_mut().find(|f| &f.id == finding) else {
                    return Err(FoldError::RulingForUnissuedFinding(finding.clone()));
                };
                f.reject_count = 0;
                f.ruled = Some(*decision);
                f.status = match decision {
                    RulingDecision::Drop => FindingStatus::Resolved,
                    RulingDecision::Fix => FindingStatus::Open,
                };
                for e in self
                    .escalations
                    .iter_mut()
                    .filter(|e| &e.finding_id == finding && !e.handled)
                {
                    e.handled = true;
                }
                let record = RulingRecord {
                    finding: finding.clone(),
                    decision: *decision,
                    text: text.clone(),
                };
                for unseen in &mut self.unseen_rulings {
                    unseen.push(record.clone());
                }
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
            // A `turn_sent` folds into its round's attempt, so a delivery
            // (P2-§5.3 binding note) counts only once that round seals; a
            // dropped trailing round re-sends the note on re-run (P2-§2).
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
