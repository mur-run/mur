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
    pub steps: Vec<StepProgress>,
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

pub fn iteration_summary_line(p: &RunProgress) -> String {
    let t = p.totals();
    format!(
        "iteration {} done: {}✓ {}✗ {} pending · spend ${:.2}{} · model {}",
        p.iteration,
        t.done,
        t.failed,
        t.pending,
        p.spend_usd,
        p.budget_usd
            .map(|b| format!("/${b:.2}"))
            .unwrap_or_default(),
        p.model.as_deref().unwrap_or("?"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
