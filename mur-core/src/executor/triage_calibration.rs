//! The feedback half of triage: write down what the verdict said, write down
//! what the run actually cost, and later compare the two.
//!
//! `triage.rs` ships thresholds — `CONFIDENCE_FLOOR`, `THIN_BUDGET_USD`,
//! `TERSE_CHARS` — that nobody measured. Its own header says so: "treat the
//! thresholds here as declared guesses, not measurements." Without this module
//! they stay guesses forever and triage becomes exactly the thing it was meant
//! to prevent: a component that speaks with confidence and has nothing
//! underneath.
//!
//! ## What can and cannot be measured
//!
//! Only `Proceed` is falsifiable. The run happened, so its real `cost_usd`,
//! elapsed time and guard stop reason exist and can be checked against the
//! verdict. `Split` / `AskHuman` / `Reject` stopped the run, which means there
//! is **no counterfactual** — we cannot know whether the task would have been
//! fine. Folding those into an accuracy percentage would be inventing data, so
//! `Calibration` counts them separately as `held_back` and never mixes them
//! into `miss_rate`.
//!
//! This is why `miss_rate` is `Option<f64>` and not `f64`: zero proceeds is
//! "nothing has been measured", which is not the same claim as "no misses".
//!
//! ## Shape on disk
//!
//! Two independent append-only lines per task, joined by `task_id` at read
//! time, in `<base>/YYYY-MM-DD.jsonl` via [`mur_common::ledger::Ledger`]. They
//! are separate lines on purpose: the verdict is known at dispatch and the
//! outcome minutes-to-hours later, and a crash in between must not lose the
//! verdict. An unpaired verdict is counted and reported, not silently dropped.

use mur_common::ledger::Ledger;
use std::collections::HashMap;
use std::path::Path;

use super::triage::{Basis, Complexity, Decision, Prefilter, TriageOutcome};

/// Guard stop reasons the runtime reports in `usage.stop_reason`. Kept in sync
/// with `dag.rs:381` — each one means a bound cut the run short, which is
/// precisely the event triage exists to predict.
pub const GUARD_STOPS: &[&str] = &["loop_detected", "stuck", "deadline", "iteration_ceiling"];

/// What triage decided, recorded at dispatch time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VerdictRecord {
    pub task_id: String,
    /// `proceed` | `split` | `ask_human` | `reject`.
    pub decision: String,
    /// `no_rule_fired` | `verdict` | `degraded` — a degraded proceed is a
    /// default, not a judgement, and must not be scored as one.
    pub basis: String,
    pub complexity: Option<Complexity>,
    pub confidence: Option<f64>,
    /// The prefilter rules that fired, so a rule that never predicts anything
    /// can be found and deleted.
    #[serde(default)]
    pub prefilter: Vec<String>,
    pub budget_deadline_secs: Option<u64>,
    pub budget_cost_usd: Option<f64>,
}

/// What the run actually did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutcomeRecord {
    pub task_id: String,
    pub spent_usd: Option<f64>,
    pub elapsed_secs: u64,
    /// `usage.stop_reason` if a guard cut the run short, else `None`.
    pub stop_reason: Option<String>,
    pub success: bool,
}

/// One line in the calibration ledger.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CalibrationEvent {
    Verdict(VerdictRecord),
    Outcome(OutcomeRecord),
}

impl CalibrationEvent {
    fn task_id(&self) -> &str {
        match self {
            Self::Verdict(v) => &v.task_id,
            Self::Outcome(o) => &o.task_id,
        }
    }
}

fn decision_str(d: &Decision) -> &'static str {
    match d {
        Decision::Proceed => "proceed",
        Decision::Split => "split",
        Decision::AskHuman => "ask_human",
        Decision::Reject => "reject",
    }
}

/// Build the dispatch-time record from a real [`TriageOutcome`].
///
/// Note the `basis` split: a `Degraded` proceed is recorded as `degraded`, not
/// as a model verdict. Scoring "the model was down so we ran it" as a correct
/// prediction would make the miss rate look better every time the model broke.
pub fn verdict_record(task_id: &str, o: &TriageOutcome) -> VerdictRecord {
    let (basis, complexity, confidence) = match &o.basis {
        Basis::NoRuleFired => ("no_rule_fired", None, None),
        Basis::Verdict(v) => ("verdict", Some(v.complexity), Some(v.confidence)),
        Basis::Degraded(_) => ("degraded", None, None),
    };
    VerdictRecord {
        task_id: task_id.to_string(),
        decision: decision_str(&o.decision).to_string(),
        basis: basis.to_string(),
        complexity,
        confidence,
        prefilter: match &o.prefilter {
            Prefilter::Skip => Vec::new(),
            Prefilter::Consult(r) => r.iter().map(|s| s.to_string()).collect(),
        },
        budget_deadline_secs: o.limits.deadline.value.map(|d| d.as_secs()),
        budget_cost_usd: o.limits.cost_usd.value,
    }
}

/// Did this run hit the wall triage was supposed to see coming?
///
/// Two independent ways, because a run can exhaust a budget without a guard
/// firing (spend is checked between turns) and a guard can fire well under the
/// dollar cap (wall-clock deadline, doom loop).
pub fn overran(o: &OutcomeRecord, budget_cost_usd: Option<f64>) -> bool {
    if let Some(r) = &o.stop_reason
        && GUARD_STOPS.contains(&r.as_str())
    {
        return true;
    }
    match (o.spent_usd, budget_cost_usd) {
        (Some(spent), Some(cap)) => spent >= cap,
        _ => false,
    }
}

/// The comparison. Every field is a count, not a rate, except the one rate —
/// and that one is optional.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Calibration {
    /// Verdicts that found their outcome.
    pub paired: usize,
    /// Of those, the ones the model actually judged (`basis == "verdict"`) and
    /// let through. The denominator of `miss_rate`.
    pub proceeded: usize,
    /// Of those, the ones that hit a bound anyway. Triage was wrong.
    pub proceeded_overran: usize,
    /// Proceeds that were a fallback, not a judgement: no rule fired, or the
    /// model was unavailable. Tracked so a rising number here explains a
    /// falling `miss_rate` that is not really an improvement.
    pub proceeded_by_default: usize,
    /// Of those defaults, how many overran. Untriaged damage.
    pub default_overran: usize,
    /// `split` / `ask_human` / `reject`. Unfalsifiable by construction.
    pub held_back: usize,
    /// Verdicts with no outcome line — crashed, still running, or a lost
    /// write. Reported so a shrinking sample is visible rather than flattering.
    pub unpaired_verdicts: usize,
    /// Outcomes with no verdict — dispatch paths that skip triage entirely.
    pub unpaired_outcomes: usize,
}

impl Calibration {
    /// Share of judged proceeds that hit a bound. `None` when nothing has been
    /// measured yet — an empty sample is not a perfect score.
    pub fn miss_rate(&self) -> Option<f64> {
        if self.proceeded == 0 {
            return None;
        }
        Some(self.proceeded_overran as f64 / self.proceeded as f64)
    }

    /// One line for a human. Says "not enough data" rather than "0%".
    pub fn summary(&self) -> String {
        match self.miss_rate() {
            None => format!(
                "triage calibration: no judged proceeds yet ({} held back, {} by default, {} unpaired)",
                self.held_back, self.proceeded_by_default, self.unpaired_verdicts
            ),
            Some(r) => format!(
                "triage calibration: {}/{} judged proceeds overran ({:.0}%); \
                 {} held back (no counterfactual), {}/{} defaults overran, {} unpaired",
                self.proceeded_overran,
                self.proceeded,
                r * 100.0,
                self.held_back,
                self.default_overran,
                self.proceeded_by_default,
                self.unpaired_verdicts
            ),
        }
    }
}

/// Join verdicts to outcomes by `task_id` and count. Order-independent: the
/// outcome may be written before the verdict's day file rolls over, and a
/// scan spanning days yields them interleaved.
pub fn calibrate(events: impl IntoIterator<Item = CalibrationEvent>) -> Calibration {
    let mut verdicts: HashMap<String, VerdictRecord> = HashMap::new();
    let mut outcomes: HashMap<String, OutcomeRecord> = HashMap::new();
    for e in events {
        let id = e.task_id().to_string();
        match e {
            CalibrationEvent::Verdict(v) => {
                verdicts.insert(id, v);
            }
            CalibrationEvent::Outcome(o) => {
                outcomes.insert(id, o);
            }
        }
    }

    let mut c = Calibration::default();
    for (id, v) in &verdicts {
        let Some(o) = outcomes.get(id) else {
            c.unpaired_verdicts += 1;
            continue;
        };
        c.paired += 1;
        if v.decision != "proceed" {
            c.held_back += 1;
            continue;
        }
        let over = overran(o, v.budget_cost_usd);
        if v.basis == "verdict" {
            c.proceeded += 1;
            if over {
                c.proceeded_overran += 1;
            }
        } else {
            c.proceeded_by_default += 1;
            if over {
                c.default_overran += 1;
            }
        }
    }
    c.unpaired_outcomes = outcomes
        .keys()
        .filter(|k| !verdicts.contains_key(*k))
        .count();
    c
}

/// The ledger handle. Thin on purpose: `Ledger` already owns rotation, append
/// and the debounced fsync.
pub struct CalibrationLog {
    ledger: Ledger<CalibrationEvent>,
}

impl CalibrationLog {
    pub fn open(base_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            ledger: Ledger::open(base_dir)?,
        })
    }

    /// Record the verdict at dispatch. Errors are returned, not swallowed —
    /// but the caller must treat a failure here as non-fatal, for the same
    /// reason `triage` never returns `Err`: losing the ability to measure must
    /// not become a way to stop the fleet.
    pub fn record_verdict(&mut self, task_id: &str, o: &TriageOutcome) -> anyhow::Result<()> {
        self.ledger
            .append(&CalibrationEvent::Verdict(verdict_record(task_id, o)))
    }

    pub fn record_outcome(&mut self, o: OutcomeRecord) -> anyhow::Result<()> {
        self.ledger.append(&CalibrationEvent::Outcome(o))
    }

    /// Read back the last `days` days and compare.
    pub fn calibration(base_dir: &Path, days: u32) -> Calibration {
        calibrate(
            Ledger::<CalibrationEvent>::scan_days(base_dir, days)
                .into_iter()
                .flatten(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::triage::{RecommendedAction, TriageVerdict};
    use mur_common::limits::{Resolved, ResolvedLimits, Source, Stuck};
    use std::time::Duration;

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

    fn verdict(conf: f64) -> TriageVerdict {
        TriageVerdict {
            complexity: Complexity::M,
            ambiguity: 1,
            dependency_risk: 1,
            recommended_action: RecommendedAction::Proceed,
            proposed_splits: vec![],
            confidence: conf,
            reasons: vec![],
        }
    }

    fn v_rec(id: &str, decision: &str, basis: &str, cap: Option<f64>) -> CalibrationEvent {
        CalibrationEvent::Verdict(VerdictRecord {
            task_id: id.into(),
            decision: decision.into(),
            basis: basis.into(),
            complexity: None,
            confidence: None,
            prefilter: vec![],
            budget_deadline_secs: Some(600),
            budget_cost_usd: cap,
        })
    }

    fn o_rec(id: &str, spent: Option<f64>, stop: Option<&str>) -> CalibrationEvent {
        CalibrationEvent::Outcome(OutcomeRecord {
            task_id: id.into(),
            spent_usd: spent,
            elapsed_secs: 10,
            stop_reason: stop.map(|s| s.into()),
            success: stop.is_none(),
        })
    }

    /// The whole point: a proceed that hit a bound is a miss.
    #[test]
    fn a_proceed_that_overran_is_counted_as_a_miss() {
        let c = calibrate([
            v_rec("t1", "proceed", "verdict", Some(1.0)),
            o_rec("t1", Some(0.1), Some("deadline")),
        ]);
        assert_eq!(c.proceeded, 1);
        assert_eq!(c.proceeded_overran, 1);
        assert_eq!(c.miss_rate(), Some(1.0));
    }

    /// Every guard reason the runtime can report must count, not just the one
    /// that was convenient to test.
    #[test]
    fn every_guard_stop_reason_counts_as_an_overrun() {
        for r in GUARD_STOPS {
            let c = calibrate([
                v_rec("t1", "proceed", "verdict", None),
                o_rec("t1", None, Some(r)),
            ]);
            assert_eq!(c.proceeded_overran, 1, "{r} must count as an overrun");
        }
    }

    /// A clean run under budget is not a miss.
    #[test]
    fn a_proceed_that_stayed_inside_its_budget_is_not_a_miss() {
        let c = calibrate([
            v_rec("t1", "proceed", "verdict", Some(1.0)),
            o_rec("t1", Some(0.2), None),
        ]);
        assert_eq!(c.proceeded, 1);
        assert_eq!(c.proceeded_overran, 0);
        assert_eq!(c.miss_rate(), Some(0.0));
    }

    /// Spending the whole cap is an overrun even with no guard stop: the run
    /// was cut off by money rather than by a timer.
    #[test]
    fn spending_the_cap_is_an_overrun_without_any_guard() {
        let c = calibrate([
            v_rec("t1", "proceed", "verdict", Some(0.50)),
            o_rec("t1", Some(0.50), None),
        ]);
        assert_eq!(c.proceeded_overran, 1);
    }

    /// An unmetered scope cannot overrun on money — only a guard can say so.
    #[test]
    fn no_cost_cap_means_spend_alone_never_counts() {
        let c = calibrate([
            v_rec("t1", "proceed", "verdict", None),
            o_rec("t1", Some(9_999.0), None),
        ]);
        assert_eq!(c.proceeded_overran, 0, "no cap to exceed");
    }

    /// The honesty rule: a held-back task has no counterfactual, so it must
    /// not move the accuracy number in either direction.
    #[test]
    fn held_back_decisions_stay_out_of_the_miss_rate() {
        let c = calibrate([
            v_rec("t1", "split", "verdict", Some(1.0)),
            o_rec("t1", Some(0.0), None),
            v_rec("t2", "ask_human", "verdict", Some(1.0)),
            o_rec("t2", Some(0.0), None),
            v_rec("t3", "reject", "verdict", Some(1.0)),
            o_rec("t3", Some(0.0), None),
        ]);
        assert_eq!(c.held_back, 3);
        assert_eq!(c.proceeded, 0);
        assert_eq!(
            c.miss_rate(),
            None,
            "three held-back tasks are not a 0% miss rate"
        );
    }

    /// An empty sample must not read as a perfect score.
    #[test]
    fn nothing_measured_is_none_not_zero() {
        let c = calibrate([]);
        assert_eq!(c.miss_rate(), None);
        assert!(
            c.summary().contains("no judged proceeds yet"),
            "{}",
            c.summary()
        );
    }

    /// A proceed the model never judged is a default. Counting it as a correct
    /// prediction would make the score improve every time the model went down.
    #[test]
    fn a_degraded_proceed_is_a_default_not_a_judgement() {
        let c = calibrate([
            v_rec("t1", "proceed", "degraded", Some(1.0)),
            o_rec("t1", Some(0.1), Some("stuck")),
        ]);
        assert_eq!(c.proceeded, 0, "the model did not judge this");
        assert_eq!(c.miss_rate(), None);
        assert_eq!(c.proceeded_by_default, 1);
        assert_eq!(c.default_overran, 1, "the damage is still recorded");
    }

    /// Same for a task no rule ever flagged.
    #[test]
    fn a_no_rule_fired_proceed_is_also_a_default() {
        let c = calibrate([
            v_rec("t1", "proceed", "no_rule_fired", Some(1.0)),
            o_rec("t1", Some(0.1), None),
        ]);
        assert_eq!(c.proceeded, 0);
        assert_eq!(c.proceeded_by_default, 1);
        assert_eq!(c.default_overran, 0);
    }

    /// A crash between dispatch and completion must shrink the sample
    /// visibly, not quietly.
    #[test]
    fn a_verdict_with_no_outcome_is_reported_not_dropped() {
        let c = calibrate([
            v_rec("t1", "proceed", "verdict", Some(1.0)),
            o_rec("t1", Some(0.1), None),
            v_rec("t2", "proceed", "verdict", Some(1.0)),
        ]);
        assert_eq!(c.paired, 1);
        assert_eq!(c.unpaired_verdicts, 1);
        assert_eq!(c.proceeded, 1, "the unpaired one is not scored");
    }

    /// Dispatch paths that skip triage are visible too.
    #[test]
    fn an_outcome_with_no_verdict_is_reported() {
        let c = calibrate([o_rec("t9", Some(0.1), None)]);
        assert_eq!(c.unpaired_outcomes, 1);
        assert_eq!(c.paired, 0);
    }

    /// The outcome usually arrives long after the verdict, and a multi-day
    /// scan interleaves them. Order must not matter.
    #[test]
    fn pairing_does_not_depend_on_order() {
        let fwd = calibrate([
            v_rec("t1", "proceed", "verdict", Some(1.0)),
            o_rec("t1", None, Some("stuck")),
        ]);
        let rev = calibrate([
            o_rec("t1", None, Some("stuck")),
            v_rec("t1", "proceed", "verdict", Some(1.0)),
        ]);
        assert_eq!(fwd, rev);
    }

    /// The bridge from a real `TriageOutcome` — the field that matters most is
    /// `basis`, because it decides whether the row is scored at all.
    #[test]
    fn verdict_record_carries_basis_and_budget_from_the_outcome() {
        let o = TriageOutcome {
            decision: Decision::Proceed,
            basis: Basis::Verdict(verdict(0.9)),
            prefilter: Prefilter::Consult(vec!["budget is thin"]),
            limits: limits(Some(0.50)),
        };
        let r = verdict_record("task-7", &o);
        assert_eq!(r.task_id, "task-7");
        assert_eq!(r.decision, "proceed");
        assert_eq!(r.basis, "verdict");
        assert_eq!(r.confidence, Some(0.9));
        assert_eq!(r.budget_cost_usd, Some(0.50));
        assert_eq!(r.budget_deadline_secs, Some(600));
        assert_eq!(r.prefilter, vec!["budget is thin".to_string()]);
    }

    #[test]
    fn a_degraded_outcome_records_no_confidence() {
        let o = TriageOutcome {
            decision: Decision::Proceed,
            basis: Basis::Degraded("triage model unavailable".into()),
            prefilter: Prefilter::Consult(vec!["budget is thin"]),
            limits: limits(None),
        };
        let r = verdict_record("t", &o);
        assert_eq!(r.basis, "degraded");
        assert_eq!(r.confidence, None);
        assert_eq!(r.complexity, None);
    }

    /// End to end on a real file, because the pairing is worthless if the two
    /// halves do not survive a round trip through JSONL.
    #[test]
    fn the_two_halves_survive_a_round_trip_through_the_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = CalibrationLog::open(tmp.path()).unwrap();
        let o = TriageOutcome {
            decision: Decision::Proceed,
            basis: Basis::Verdict(verdict(0.8)),
            prefilter: Prefilter::Consult(vec!["spans more than one crate"]),
            limits: limits(Some(1.0)),
        };
        log.record_verdict("t1", &o).unwrap();
        log.record_outcome(OutcomeRecord {
            task_id: "t1".into(),
            spent_usd: Some(0.3),
            elapsed_secs: 42,
            stop_reason: Some("loop_detected".into()),
            success: false,
        })
        .unwrap();

        let c = CalibrationLog::calibration(tmp.path(), 2);
        assert_eq!(c.paired, 1);
        assert_eq!(c.proceeded, 1);
        assert_eq!(c.proceeded_overran, 1);
        assert_eq!(c.miss_rate(), Some(1.0));
    }
}
