//! The seam that connects triage to a real dispatch.
//!
//! [`super::triage`] decides, [`super::triage_model`] asks, and
//! [`super::triage_calibration`] scores — but until something calls them in
//! order around an actual run, all three are library surface producing no
//! numbers. This module is that call, and it is deliberately the only place
//! where the three meet.
//!
//! # Shadow mode, and why the default is not to enforce
//!
//! Triage ships OFF as a gate. It runs, it records, and it lets the dispatch
//! through regardless of what it decided. That is not timidity — it is the
//! only way the feedback loop can ever produce evidence.
//!
//! `triage_calibration` cannot score a hold-back: if triage stops a task, the
//! run never happens, so nobody can say whether it would have been fine. That
//! is the `held_back` counter, and it is unfalsifiable by construction. A
//! triage that enforces from day one therefore generates exactly the evidence
//! that cannot justify it, and the first time it wrongly blocks a task, the
//! user sees a fleet that refuses work for a reason nobody measured.
//!
//! Shadow mode inverts that. Every hold-back is recorded as a prediction and
//! the run proceeds anyway, so the outcome arrives and
//! `Calibration::shadow_precision` can answer the one question that matters:
//! when triage wanted to stop something, how often was it right? Turning
//! `triage.enforce` on before that number exists is a decision made on
//! nothing.
//!
//! # What is guaranteed regardless of configuration
//!
//! - **Nothing here can widen a limit.** The gate receives `ResolvedLimits`
//!   and passes them through untouched; there is no return path that grants
//!   budget.
//! - **A failure here never blocks a dispatch.** Model down, ledger
//!   unwritable, disk full — every path ends in the run proceeding, because a
//!   triage step that can stop the fleet is a new way to take the fleet down.
//!   This mirrors `triage::triage`, which never returns `Err`.
//! - **Recording is best-effort and says so.** A lost ledger write is logged
//!   at `warn` and dropped. It costs a sample, not a run.

use std::path::Path;
use std::time::Duration;

use mur_common::limits::ResolvedLimits;

use super::triage::{Decision, TriageOutcome};
use super::triage_calibration::{CalibrationLog, OutcomeRecord};
use super::triage_model::LocalTriageModel;

/// How long triage may take before it is treated as unavailable.
///
/// Short on purpose: this is pure overhead on the dispatch path, paid before
/// any real work starts. A triage that takes longer than this has already
/// cost more than the mistake it was trying to prevent is worth catching
/// late.
pub const TRIAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Where the calibration ledger lives. One directory for the whole install:
/// the sample is small and thinly spread, and splitting it per fleet would
/// mean no fleet ever accumulates enough verdicts to compute a rate from.
pub fn calibration_dir(mur_home: &Path) -> std::path::PathBuf {
    mur_home.join("triage-calibration")
}

/// What the gate concluded, and whether that conclusion was allowed to act.
#[derive(Debug, Clone)]
pub struct GateResult {
    /// What triage decided, for the record and for display.
    pub outcome: TriageOutcome,
    /// Was the decision binding? False in shadow mode (the default).
    pub enforced: bool,
}

impl GateResult {
    /// Should the caller stop? Only ever true when triage decided against the
    /// task AND the user opted into enforcement.
    pub fn blocks(&self) -> bool {
        self.enforced && self.outcome.decision != Decision::Proceed
    }

    /// One line for the console, written so the shadow case cannot be mistaken
    /// for a block. Silence when triage had nothing to say: a gate that
    /// narrates every uneventful dispatch trains people to skip its output.
    pub fn console_line(&self) -> Option<String> {
        if self.outcome.decision == Decision::Proceed {
            return None;
        }
        let what = match self.outcome.decision {
            Decision::Proceed => unreachable!("guarded above"),
            Decision::Split => "should be split",
            Decision::AskHuman => "needs a human's call",
            Decision::Reject => "should not run",
        };
        Some(if self.enforced {
            format!("  ✗ triage: this task {what} — not dispatching")
        } else {
            format!(
                "  ℹ triage (shadow): this task {what} — dispatching anyway, recording the prediction"
            )
        })
    }
}

/// Run triage for one about-to-be-dispatched task and record the verdict.
///
/// `task_id` MUST be the same id later handed to [`record_outcome`], or the
/// two halves never pair and the verdict counts as `unpaired_verdicts`.
///
/// Never fails: every error path yields a `Proceed` outcome. See the module
/// header for why that is a design guarantee rather than leniency.
pub async fn gate(
    mur_home: &Path,
    task_id: &str,
    description: &str,
    limits: &ResolvedLimits,
    enforce: bool,
) -> GateResult {
    let model = LocalTriageModel::from_home(mur_home);
    let outcome = super::triage::triage(description, limits, TRIAGE_TIMEOUT, |prompt| async move {
        model?.ask(prompt, TRIAGE_TIMEOUT).await
    })
    .await;

    // Best-effort: a verdict that cannot be written costs a sample, never a
    // run. Logged rather than swallowed so a permanently broken ledger is
    // findable — a silently empty calibration reads exactly like a triage
    // that never fires.
    match CalibrationLog::open(&calibration_dir(mur_home)) {
        Ok(mut log) => {
            if let Err(error) = log.record_verdict(task_id, &outcome, enforce) {
                tracing::warn!(%task_id, %error, "triage verdict not recorded; calibration will show it unpaired");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "triage calibration ledger unavailable; verdict not recorded");
        }
    }

    GateResult {
        outcome,
        enforced: enforce,
    }
}

/// Record what the run actually cost, closing the loop on an earlier
/// [`gate`] call with the same `task_id`.
///
/// Best-effort by the same rule: a lost outcome makes its verdict unpaired,
/// which `Calibration` reports rather than hides.
pub fn record_outcome(
    mur_home: &Path,
    task_id: &str,
    spent_usd: Option<f64>,
    elapsed: Duration,
    stop_reason: Option<String>,
    success: bool,
) {
    let rec = OutcomeRecord {
        task_id: task_id.to_string(),
        spent_usd,
        elapsed_secs: elapsed.as_secs(),
        stop_reason,
        success,
    };
    match CalibrationLog::open(&calibration_dir(mur_home)) {
        Ok(mut log) => {
            if let Err(error) = log.record_outcome(rec) {
                tracing::warn!(%task_id, %error, "triage outcome not recorded; its verdict stays unpaired");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "triage calibration ledger unavailable; outcome not recorded");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::triage::{Basis, Prefilter};
    use crate::executor::triage_calibration::{CalibrationEvent, calibrate};
    use mur_common::limits::{Resolved, Source, Stuck};

    fn limits(cost: Option<f64>) -> ResolvedLimits {
        ResolvedLimits {
            deadline: Resolved {
                value: Some(Duration::from_secs(600)),
                source: Source::BuiltIn,
            },
            stuck: Resolved {
                value: Stuck::After(Duration::from_secs(600)),
                source: Source::BuiltIn,
            },
            cost_usd: Resolved {
                value: cost,
                source: Source::BuiltIn,
            },
        }
    }

    fn result(decision: Decision, enforced: bool) -> GateResult {
        GateResult {
            outcome: TriageOutcome {
                decision,
                basis: Basis::NoRuleFired,
                prefilter: Prefilter::Skip,
                limits: limits(None),
            },
            enforced,
        }
    }

    /// The default posture: triage disagrees, the work happens anyway.
    #[test]
    fn a_shadow_hold_back_never_blocks() {
        for d in [Decision::Split, Decision::AskHuman, Decision::Reject] {
            assert!(
                !result(d, false).blocks(),
                "shadow mode must never stop a dispatch"
            );
        }
    }

    /// Enforcement is the only thing that can stop a dispatch.
    #[test]
    fn only_an_enforced_hold_back_blocks() {
        for d in [Decision::Split, Decision::AskHuman, Decision::Reject] {
            assert!(result(d, true).blocks());
        }
    }

    /// A proceed cannot block in either mode — enforcing must not invent a
    /// refusal out of agreement.
    #[test]
    fn a_proceed_never_blocks_in_either_mode() {
        assert!(!result(Decision::Proceed, false).blocks());
        assert!(!result(Decision::Proceed, true).blocks());
    }

    /// The console must not let a shadow note read as a refusal — that is the
    /// exact confusion that would make someone "fix" a fleet that is fine.
    #[test]
    fn the_shadow_line_says_it_is_dispatching_anyway() {
        let line = result(Decision::Reject, false).console_line().unwrap();
        assert!(line.contains("shadow"), "{line}");
        assert!(line.contains("dispatching anyway"), "{line}");
        assert!(!line.contains("not dispatching"), "{line}");
    }

    /// An enforced refusal must say plainly that nothing ran.
    #[test]
    fn the_enforced_line_says_nothing_was_dispatched() {
        let line = result(Decision::Reject, true).console_line().unwrap();
        assert!(line.contains("not dispatching"), "{line}");
        assert!(!line.contains("shadow"), "{line}");
    }

    /// An uneventful triage says nothing at all.
    #[test]
    fn a_proceed_prints_nothing() {
        assert!(result(Decision::Proceed, false).console_line().is_none());
        assert!(result(Decision::Proceed, true).console_line().is_none());
    }

    /// End to end through the real ledger: a gate call followed by an outcome
    /// with the same id must pair, because an id mismatch is the one bug that
    /// would silently produce a permanently empty calibration.
    #[tokio::test]
    async fn a_gated_dispatch_and_its_outcome_pair_in_the_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // No Hub running under a temp home, so the model is unavailable and
        // triage degrades to Proceed — exactly the path a real machine takes
        // when the local model is down, and it must still record.
        let g = gate(
            home,
            "task-1",
            "refactor mur-core and mur-common end to end",
            &limits(Some(1.0)),
            false,
        )
        .await;
        assert_eq!(
            g.outcome.decision,
            Decision::Proceed,
            "an unavailable model must degrade to Proceed, never block"
        );

        record_outcome(
            home,
            "task-1",
            Some(2.0),
            Duration::from_secs(30),
            Some("deadline".into()),
            false,
        );

        let events: Vec<CalibrationEvent> =
            mur_common::ledger::Ledger::<CalibrationEvent>::scan_days(&calibration_dir(home), 2)
                .into_iter()
                .flatten()
                .collect();
        assert_eq!(events.len(), 2, "one verdict + one outcome");

        let c = calibrate(events);
        assert_eq!(c.paired, 1, "the two halves must pair on task_id");
        assert_eq!(c.unpaired_verdicts, 0);
        assert_eq!(c.unpaired_outcomes, 0);
        // Degraded proceed = a default, not a judgement.
        assert_eq!(c.proceeded, 0);
        assert_eq!(c.proceeded_by_default, 1);
        assert_eq!(c.default_overran, 1, "it blew past a $1.00 cap at $2.00");
    }

    /// A mismatched id is the silent failure mode this seam exists to avoid,
    /// so it is asserted to be VISIBLE rather than merely absent.
    #[tokio::test]
    async fn a_mismatched_id_shows_up_as_unpaired_not_as_success() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let _ = gate(home, "task-a", "migrate everything", &limits(None), false).await;
        record_outcome(home, "task-b", None, Duration::from_secs(1), None, true);

        let c = CalibrationLog::calibration(&calibration_dir(home), 2);
        assert_eq!(c.paired, 0);
        assert_eq!(c.unpaired_verdicts, 1);
        assert_eq!(c.unpaired_outcomes, 1);
    }
}
