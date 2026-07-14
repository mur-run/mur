//! Bare `mur deep-research` status panel (read-only).

use std::path::Path;

use super::status::{DEFAULT_FLEET_NAME, DeepResearchStatus, collect_status};
use crate::cmd::fleet::progress::{Phase, RunProgress, STALE_AFTER_SECS, StepState};

pub fn render_panel(s: &DeepResearchStatus, progress: Option<(RunProgress, u64)>) -> String {
    if s.workers.is_empty() {
        return "Deep research is not set up yet.\n  Run `mur deep-research setup` to configure workers, model, budget and egress.\n".to_string();
    }
    let mut out = String::from("Deep research status\n");
    out.push_str(&format!(
        "  model: {}\n",
        s.model.as_deref().unwrap_or("(none — run setup)")
    ));
    out.push_str(&format!(
        "  fleet: {}\n",
        if s.fleet_exists {
            DEFAULT_FLEET_NAME
        } else {
            "(missing — run setup)"
        }
    ));
    for w in &s.workers {
        out.push_str(&format!(
            "  {} — {}, egress {}\n",
            w.name,
            if w.running { "running" } else { "stopped" },
            if w.egress_granted {
                "granted"
            } else {
                "NOT granted"
            },
        ));
    }
    if let Some((p, age)) = progress {
        out.push_str(&render_progress(&p, age));
    }
    out.push_str("\nRun research with: mur deep-research \"<your question>\"\n");
    out
}

/// Render the in-flight (or last) run block from the progress file. Pure over
/// `(progress, file-mtime-age-secs)`; `age` only drives the staleness warning.
pub fn render_progress(p: &RunProgress, mtime_age_secs: u64) -> String {
    // Finished run → one recap line.
    if let Some(ended) = &p.finished_at {
        return format!(
            "\nlast run: {} · ${:.2} · {} iteration{} · {ended}\n",
            p.outcome.as_deref().unwrap_or("?"),
            p.spend_usd,
            p.iteration,
            if p.iteration == 1 { "" } else { "s" },
        );
    }

    // In-flight run block.
    let mut out = String::from("\nRun in progress\n");
    let q: String = p.question.chars().take(80).collect();
    out.push_str(&format!("  {q}\n"));
    out.push_str(&format!("  iteration {}\n", p.iteration));

    // Per-phase done/total counts (only phases actually present).
    let mut parts = Vec::new();
    for ph in [
        Phase::Probe,
        Phase::Research,
        Phase::Verify,
        Phase::Synthesize,
        Phase::Other,
    ] {
        let total = p.steps.iter().filter(|s| s.phase == ph).count();
        if total == 0 {
            continue;
        }
        let done = p
            .steps
            .iter()
            .filter(|s| s.phase == ph && s.state == StepState::Done)
            .count();
        parts.push(format!("{} {done}/{total}", ph.label()));
    }
    if !parts.is_empty() {
        out.push_str(&format!("  {}\n", parts.join(" · ")));
    }

    // Currently-running steps: `⏳ s2 research dr_worker_2 (42s)`.
    for s in p.steps.iter().filter(|s| s.state == StepState::Running) {
        let worker = s.worker.as_deref().unwrap_or("");
        let el = elapsed_secs(s.started_at.as_deref())
            .map(|n| format!(" ({n}s)"))
            .unwrap_or_default();
        out.push_str(&format!("  ⏳ {} {} {worker}{el}\n", s.id, s.phase.label()));
    }

    out.push_str(&format!(
        "  spend ${:.2}{}\n",
        p.spend_usd,
        p.budget_usd
            .map(|b| format!("/${b:.2}"))
            .unwrap_or_default(),
    ));

    // Total elapsed since the run started (best-effort; omitted if unparseable).
    if let Some(n) = elapsed_secs(Some(&p.started_at)) {
        out.push_str(&format!("  elapsed {}\n", fmt_elapsed(n)));
    }

    if mtime_age_secs > STALE_AFTER_SECS {
        out.push_str(&format!(
            "  ⚠ no update for {} min — run may have crashed (mur fleet stop/start {DEFAULT_FLEET_NAME})\n",
            mtime_age_secs / 60,
        ));
    }
    out
}

/// Whole seconds from an RFC3339 timestamp to now; None if unparseable or in
/// the future. Best-effort — a bad timestamp just omits the elapsed hint.
fn elapsed_secs(started: Option<&str>) -> Option<i64> {
    let start = chrono::DateTime::parse_from_rfc3339(started?).ok()?;
    let secs = (chrono::Utc::now() - start.with_timezone(&chrono::Utc)).num_seconds();
    (secs >= 0).then_some(secs)
}

/// Compact `Nm Ns` / `Ns` elapsed label.
fn fmt_elapsed(secs: i64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub fn cmd_panel(mur_home: &Path) -> anyhow::Result<()> {
    let progress = crate::cmd::fleet::progress::load(mur_home, DEFAULT_FLEET_NAME)
        .map(|(p, mtime)| (p, mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0)));
    print!(
        "{}",
        render_panel(&collect_status(mur_home, DEFAULT_FLEET_NAME), progress)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deep_research::status::{DeepResearchStatus, WorkerStatus};
    use crate::cmd::fleet::progress::StepProgress;

    #[test]
    fn panel_unconfigured_points_at_setup() {
        let s = DeepResearchStatus {
            workers: vec![],
            fleet_exists: false,
            model: None,
        };
        let out = render_panel(&s, None);
        assert!(out.contains("mur deep-research setup"));
    }

    #[test]
    fn panel_lists_workers_and_egress() {
        let s = DeepResearchStatus {
            workers: vec![WorkerStatus {
                name: "dr_worker_1".into(),
                running: true,
                egress_granted: true,
            }],
            fleet_exists: true,
            model: Some("claude_haiku".into()),
        };
        let out = render_panel(&s, None);
        assert!(out.contains("dr_worker_1"));
        assert!(out.contains("running"));
        assert!(out.contains("claude_haiku"));
    }

    /// `finished`: Some((outcome, finished_at)) → finished run; None → in-flight
    /// with 1 done / 1 running / 1 pending, iteration 2, spend $0.31.
    fn progress_fixture(finished: Option<(&str, &str)>) -> RunProgress {
        RunProgress {
            schema_version: 1,
            run_id: "r1".into(),
            question: "compare local LLM runtimes".into(),
            started_at: "2026-07-14T00:00:00Z".into(),
            finished_at: finished.map(|(_, f)| f.into()),
            outcome: finished.map(|(o, _)| o.into()),
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
    fn panel_shows_in_flight_run_block() {
        let p = progress_fixture(None);
        let out = render_progress(&p, 30);
        assert!(out.contains("Run in progress"));
        assert!(out.contains("iteration 2"));
        assert!(out.contains("$0.31"));
        assert!(out.contains("⏳ s2 research dr_worker_2"));
        assert!(out.contains("elapsed "));
        assert!(!out.contains("crashed"));
    }

    #[test]
    fn panel_marks_stale_run() {
        let p = progress_fixture(None);
        let out = render_progress(&p, STALE_AFTER_SECS + 1);
        assert!(out.contains("run may have crashed"));
    }

    #[test]
    fn panel_shows_last_run_line_when_finished() {
        let p = progress_fixture(Some(("converged", "2026-07-14T01:00:00Z")));
        let out = render_progress(&p, 10_000);
        assert!(out.contains("last run: converged"));
        assert!(out.contains("2 iterations"));
        assert!(out.contains("2026-07-14T01:00:00Z"));
        assert!(!out.contains("crashed"));
        assert!(!out.contains("Run in progress"));
    }
}
