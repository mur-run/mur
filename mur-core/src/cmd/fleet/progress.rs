//! Single-source run-progress model for fleet loops (deep-research UX).
//! Pure data + best-effort atomic persistence; consumers render it
//! (loop stdout, `mur deep-research` panel, murmur Panel in Phase 2).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name under `~/.mur/fleets/<name>/`. Kept after the run as the
/// last-run record; overwritten by the next run.
pub const PROGRESS_FILE: &str = ".run_progress.json";
/// An in-flight file whose mtime is older than this is labeled stale
/// (loop probably crashed).
pub const STALE_AFTER_SECS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Probe,
    Research,
    Verify,
    Synthesize,
    Other,
}

impl Phase {
    /// Short lower-case label for log/panel rendering (matches the serde name).
    pub fn label(self) -> &'static str {
        match self {
            Phase::Probe => "probe",
            Phase::Research => "research",
            Phase::Verify => "verify",
            Phase::Synthesize => "synthesize",
            Phase::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepProgress {
    pub id: String,
    pub worker: Option<String>,
    pub phase: Phase,
    pub desc: String,
    pub state: StepState,
    pub cost_usd: Option<f64>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProgress {
    pub schema_version: u32,
    pub run_id: String,
    pub question: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// converged | max-iterations | deadline | budget | stopped | stuck | failed
    pub outcome: Option<String>,
    pub iteration: u32,
    pub model: Option<String>,
    pub budget_usd: Option<f64>,
    pub spend_usd: f64,
    /// Whether this fleet actually costs money (`billing::fleet_billing`).
    /// `spend_usd` is token-count times list price either way, so on a
    /// subscription/local fleet it is an equivalent, not a bill — see
    /// [`fmt_spend`]. `None` on records written before this field existed:
    /// those render as they always did rather than guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billable: Option<bool>,
    pub steps: Vec<StepProgress>,
    /// Path to the saved report, filled in after `save_report` succeeds.
    /// `None` while a terminal, done-set outcome is still writing its
    /// report — see [`ProgressPhase::ReportSaving`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<PathBuf>,
    /// Set by the `cmd_ask` wrapper when preflight fails before the loop
    /// ever runs (outcome `"failed"`); loop-internal failures use `stuck`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Totals {
    pub done: usize,
    pub running: usize,
    pub pending: usize,
    pub failed: usize,
}

/// Keyword heuristic over the router's assignment text. Unclassifiable
/// text is `Other` — classification is display-only and never gates the run.
pub fn classify_phase(assignment: &str) -> Phase {
    let a = assignment.to_lowercase();
    if a.contains("probe") || a.contains("health") {
        Phase::Probe
    } else if a.contains("synthesi") || a.contains("report") {
        Phase::Synthesize
    } else if a.contains("verify") || a.contains("refute") || a.contains("confirm") {
        Phase::Verify
    } else if a.contains("research") || a.contains("search") || a.contains("fetch") {
        Phase::Research
    } else {
        Phase::Other
    }
}

impl RunProgress {
    pub fn totals(&self) -> Totals {
        let mut t = Totals {
            done: 0,
            running: 0,
            pending: 0,
            failed: 0,
        };
        for s in &self.steps {
            match s.state {
                StepState::Done => t.done += 1,
                StepState::Running => t.running += 1,
                StepState::Pending => t.pending += 1,
                StepState::Failed => t.failed += 1,
            }
        }
        t
    }

    /// Best-effort atomic save; errors are logged at debug and swallowed —
    /// the progress file must never affect the run.
    pub fn save(&self, mur_home: &Path, fleet: &str) {
        let res = (|| -> anyhow::Result<()> {
            let path = progress_path(mur_home, fleet);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(e) = res {
            tracing::debug!("run progress save failed (ignored): {e}");
        }
    }
}

pub fn progress_path(mur_home: &Path, fleet: &str) -> PathBuf {
    mur_home.join("fleets").join(fleet).join(PROGRESS_FILE)
}

/// None on missing/corrupt file (a corrupt progress file is not an error
/// condition anywhere). The mtime feeds the panel's staleness label.
pub fn load(mur_home: &Path, fleet: &str) -> Option<(RunProgress, std::time::SystemTime)> {
    let path = progress_path(mur_home, fleet);
    let body = std::fs::read(&path).ok()?;
    let p: RunProgress = serde_json::from_slice(&body).ok()?;
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some((p, mtime))
}

/// A progress record plus the file-mtime age and a computed liveness flag.
/// The single place every reader (panel, `mur_job_status` fallback, murmur
/// ticker) gets `age_secs`/`live` from, so they never disagree.
#[derive(Debug, Clone)]
pub struct ProgressView {
    pub progress: RunProgress,
    pub age_secs: u64,
    /// `true` when the run has not finished and its file was updated
    /// recently enough not to be considered stale/crashed.
    ///
    /// Not yet read outside tests — the `mur_job_status` fallback (Task 4)
    /// and the murmur `/deep-research ask` ticker (Task 5) are its
    /// production consumers.
    #[allow(dead_code)]
    pub live: bool,
}

/// Build a [`ProgressView`] from a loaded record and its file-mtime age.
pub fn view_from(progress: RunProgress, age_secs: u64) -> ProgressView {
    let live = progress.finished_at.is_none() && age_secs <= STALE_AFTER_SECS;
    ProgressView {
        progress,
        age_secs,
        live,
    }
}

/// [`load`] plus [`view_from`] in one call — the common path for readers
/// that only need the view, not the raw `SystemTime`.
pub fn load_view(mur_home: &Path, fleet: &str) -> Option<ProgressView> {
    let (p, mtime) = load(mur_home, fleet)?;
    let age_secs = mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    Some(view_from(p, age_secs))
}

/// Coarse render/poll state derived from an `outcome` string. Unknown
/// strings (including a missing outcome that gets treated as failed by a
/// caller) map to `"failed"` — see [`state_for_outcome`].
///
/// Not yet constructed outside tests — [`progress_phase`] is wired into
/// the `mur_job_status` fallback in Task 4.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPhase {
    Running,
    ReportSaving,
    Done,
    Stopped,
    Blocked,
    Failed,
}

/// Single source of truth for outcome→state, shared by the panel, the
/// `mur_job_status` fallback (Task 4), and the murmur ticker (Task 5).
/// Mirrors `loop_run::outcome_label`'s nine terms plus the wrapper-only
/// `"failed"` term; anything else is treated as failed.
///
/// Not yet called outside tests and [`progress_phase`] — Tasks 4/5 are its
/// production callers.
#[allow(dead_code)]
pub fn state_for_outcome(outcome: &str) -> &'static str {
    match outcome {
        "converged" | "max-iterations" | "deadline" | "budget" | "queue-drained" => "done",
        "stopped" | "commander-killed" => "stopped",
        "awaiting-approval" => "blocked",
        "stuck" | "failed" => "failed",
        _ => "failed",
    }
}

/// Full-phase verdict for a record, distinguishing a terminal, done-set
/// outcome that hasn't had its report path back-filled yet
/// ([`ProgressPhase::ReportSaving`]) from one that has ([`ProgressPhase::Done`]).
///
/// Not yet called outside tests — the `mur_job_status` fallback (Task 4)
/// is its production caller.
#[allow(dead_code)]
pub fn progress_phase(p: &RunProgress) -> ProgressPhase {
    let Some(outcome) = p.outcome.as_deref() else {
        return ProgressPhase::Running;
    };
    if p.finished_at.is_none() {
        return ProgressPhase::Running;
    }
    match state_for_outcome(outcome) {
        "done" if p.artifact_path.is_none() => ProgressPhase::ReportSaving,
        "done" => ProgressPhase::Done,
        "stopped" => ProgressPhase::Stopped,
        "blocked" => ProgressPhase::Blocked,
        _ => ProgressPhase::Failed,
    }
}

/// The money token, told straight. A non-billable fleet still accumulates a
/// figure — tokens times list price — but nobody is charged it, so printing a
/// bare `$7.90` next to "cannot spend" made the two lines contradict each
/// other. Unknown billing keeps the old bare form.
pub fn fmt_spend(spend_usd: f64, billable: Option<bool>) -> String {
    match billable {
        Some(false) => format!("≈${spend_usd:.2} (not billed)"),
        _ => format!("${spend_usd:.2}"),
    }
}

pub fn iteration_summary_line(p: &RunProgress) -> String {
    let t = p.totals();
    format!(
        "iteration {} done: {}✓ {}✗ {} pending · spend {}{} · model {}",
        p.iteration,
        t.done,
        t.failed,
        t.pending,
        fmt_spend(p.spend_usd, p.billable),
        p.budget_usd
            .map(|b| format!("/${b:.2}"))
            .unwrap_or_default(),
        p.model.as_deref().unwrap_or("?"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The banner says a subscription fleet "cannot spend"; a bare `$7.90`
    /// next to it read as a bill. Non-billable runs must mark the figure as
    /// an equivalent, and billable ones must NOT (that one is a real charge).
    #[test]
    fn a_non_billable_run_never_reports_a_bare_dollar_amount() {
        assert_eq!(fmt_spend(7.90, Some(true)), "$7.90");
        assert_eq!(fmt_spend(7.90, None), "$7.90");
        let hedged = fmt_spend(7.90, Some(false));
        assert!(hedged.contains("not billed"), "{hedged}");
        assert!(hedged.contains("7.90"), "{hedged}");

        let mut p = sample();
        p.billable = Some(false);
        p.spend_usd = 7.90;
        let line = iteration_summary_line(&p);
        assert!(line.contains("not billed"), "{line}");
        // The contradiction was "cannot spend" + a bare price token.
        assert!(!line.contains("· spend $"), "{line}");
    }

    #[test]
    fn classify_phase_heuristics() {
        assert_eq!(
            classify_phase("Run a single minimal gateway health probe"),
            Phase::Probe
        );
        assert_eq!(
            classify_phase("Research failure-handling best practices"),
            Phase::Research
        );
        assert_eq!(
            classify_phase("verify s2's claims under correctness lenses"),
            Phase::Verify
        );
        assert_eq!(
            classify_phase("Synthesize s1-s3 findings into a cited report"),
            Phase::Synthesize
        );
        assert_eq!(classify_phase("hello world"), Phase::Other);
    }

    fn sample() -> RunProgress {
        RunProgress {
            schema_version: 1,
            run_id: "r1".into(),
            question: "q".into(),
            started_at: "2026-07-14T00:00:00Z".into(),
            finished_at: None,
            outcome: None,
            iteration: 2,
            model: Some("claude_haiku".into()),
            budget_usd: Some(2.0),
            spend_usd: 0.31,
            billable: None,
            steps: vec![
                StepProgress {
                    id: "s1".into(),
                    worker: Some("dr_worker_1".into()),
                    phase: Phase::Probe,
                    desc: "probe".into(),
                    state: StepState::Done,
                    cost_usd: Some(0.01),
                    started_at: None,
                    ended_at: None,
                },
                StepProgress {
                    id: "s2".into(),
                    worker: Some("dr_worker_2".into()),
                    phase: Phase::Research,
                    desc: "research".into(),
                    state: StepState::Running,
                    cost_usd: None,
                    started_at: None,
                    ended_at: None,
                },
                StepProgress {
                    id: "s3".into(),
                    worker: None,
                    phase: Phase::Verify,
                    desc: "verify".into(),
                    state: StepState::Pending,
                    cost_usd: None,
                    started_at: None,
                    ended_at: None,
                },
            ],
            artifact_path: None,
            error: None,
        }
    }

    #[test]
    fn totals_counts_states() {
        let t = sample().totals();
        assert_eq!((t.done, t.running, t.pending, t.failed), (1, 1, 1, 0));
    }

    #[test]
    fn summary_line_shows_counts_spend_model() {
        let line = iteration_summary_line(&sample());
        assert!(line.contains("iteration 2"));
        assert!(line.contains("1✓"));
        assert!(line.contains("1 pending"));
        assert!(line.contains("$0.31/$2.00"));
        assert!(line.contains("claude_haiku"));
    }

    #[test]
    fn save_load_roundtrip_and_missing_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(tmp.path(), "deep-research").is_none());
        let p = sample();
        p.save(tmp.path(), "deep-research");
        let (loaded, _mtime) = load(tmp.path(), "deep-research").unwrap();
        assert_eq!(loaded.iteration, 2);
        assert_eq!(loaded.steps.len(), 3);
    }

    #[test]
    fn old_json_without_new_fields_loads() {
        let mut value = serde_json::to_value(sample()).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("artifact_path");
        obj.remove("error");
        let loaded: RunProgress = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.artifact_path, None);
        assert_eq!(loaded.error, None);
    }

    #[test]
    fn new_fields_round_trip() {
        let mut p = sample();
        p.artifact_path = Some(PathBuf::from("/tmp/report.md"));
        p.error = Some("boom".into());
        let json = serde_json::to_string(&p).unwrap();
        let loaded: RunProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.artifact_path, Some(PathBuf::from("/tmp/report.md")));
        assert_eq!(loaded.error, Some("boom".into()));
    }

    #[test]
    fn none_fields_are_omitted_on_save() {
        let json = serde_json::to_string(&sample()).unwrap();
        assert!(!json.contains("artifact_path"));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn view_from_computes_age_and_liveness() {
        let live = view_from(sample(), 30);
        assert_eq!(live.age_secs, 30);
        assert!(live.live);

        let stale = view_from(sample(), STALE_AFTER_SECS + 1);
        assert!(!stale.live);

        let mut finished = sample();
        finished.finished_at = Some("2026-07-14T01:00:00Z".into());
        let done = view_from(finished, 5);
        assert!(!done.live);
    }

    #[test]
    fn state_for_outcome_covers_all_nine_plus_failed() {
        let cases: &[(&str, &str)] = &[
            ("converged", "done"),
            ("max-iterations", "done"),
            ("deadline", "done"),
            ("budget", "done"),
            ("queue-drained", "done"),
            ("stopped", "stopped"),
            ("commander-killed", "stopped"),
            ("awaiting-approval", "blocked"),
            ("stuck", "failed"),
            ("failed", "failed"),
        ];
        for (outcome, want) in cases {
            assert_eq!(state_for_outcome(outcome), *want, "outcome {outcome}");
        }
        assert_eq!(state_for_outcome("something-unknown"), "failed");
    }

    #[test]
    fn report_saving_is_detected() {
        let mut p = sample();
        p.finished_at = Some("2026-07-14T01:00:00Z".into());
        p.outcome = Some("converged".into());
        p.artifact_path = None;
        assert_eq!(progress_phase(&p), ProgressPhase::ReportSaving);

        p.artifact_path = Some(PathBuf::from("/tmp/report.md"));
        assert_eq!(progress_phase(&p), ProgressPhase::Done);
    }
}
