//! `mur deep-research "question"` — preflight + safe auto-repair + run.
//!
//! Auto-repair is LIMITED to starting workers and re-pinning the gateway.
//! Egress/grants are never touched here (explicit consent lives in setup).

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};

use super::status::{DEFAULT_FLEET_NAME, DeepResearchStatus, collect_status, is_agent_running};

#[derive(Debug)]
pub enum PreflightAction {
    StartWorker(String),
    RepinGateway(String),
}

/// Filter a full prefix-scanned status down to just the fleet's current
/// members. Setup's own use of `collect_status` (skip-if-granted + surplus
/// detection) must see the FULL prefix scan — this filtering only happens
/// at the `cmd_ask` call site, so `plan_preflight` itself stays pure over
/// whatever status it's handed.
pub fn scope_to_members(s: DeepResearchStatus, members: &[String]) -> DeepResearchStatus {
    DeepResearchStatus {
        workers: s
            .workers
            .into_iter()
            .filter(|w| members.iter().any(|m| m == &w.name))
            .collect(),
        fleet_exists: s.fleet_exists,
        model: s.model,
    }
}

pub fn plan_preflight(s: &DeepResearchStatus) -> Result<Vec<PreflightAction>> {
    if s.workers.is_empty() {
        bail!("no deep-research workers found — run `mur deep-research setup` first");
    }
    if let Some(w) = s.workers.iter().find(|w| !w.egress_granted) {
        bail!(
            "worker {} has no audited egress grant — run `mur deep-research setup` \
             (egress is an explicit consent step; it is never granted automatically)",
            w.name
        );
    }
    if !s.fleet_exists {
        bail!("fleet '{DEFAULT_FLEET_NAME}' missing — run `mur deep-research setup`");
    }
    let mut plan = Vec::new();
    for w in &s.workers {
        // A running worker has already proved its gateway pin against ITS
        // OWN PATH. Re-pinning it from this process records whatever THIS
        // process resolves, which on a dual-install box is a different copy
        // (Hub-launched concierge: /opt/homebrew/bin first; launchd worker:
        // ~/.local/bin first) — and the worker refuses to start next time
        // (B0 rule 6). It would also need to write a sibling agent's profile,
        // which the sandbox rightly denies. Only a stopped worker — the shape
        // of a post-upgrade pin refusal — is healed: pin first, because the
        // runtime reads its profile once, at start.
        if w.running {
            continue;
        }
        plan.push(PreflightAction::RepinGateway(w.name.clone()));
        plan.push(PreflightAction::StartWorker(w.name.clone()));
    }
    Ok(plan)
}

/// Write a minimal failed [`RunProgress`] record for `run_id`/`question` —
/// used when preflight fails before the loop ever runs (points A–G) so a
/// poller with only the run id still finds a terminal, explained record.
///
/// If a progress file already exists for a DIFFERENT `run_id`, it is
/// overwritten: the file is last-run storage by design (`loop_run.rs`
/// stamps the terminal state onto the same path), so "last run" is always
/// what a fresh caller should see. If it exists for the SAME `run_id`,
/// only `outcome`/`error`/`finished_at` are set — other fields (e.g. a
/// partially-populated `iteration`/`steps`) are preserved.
fn record_preflight_failure(mur_home: &Path, run_id: &str, question: &str, error_summary: &str) {
    let existing = crate::cmd::fleet::progress::load(mur_home, DEFAULT_FLEET_NAME)
        .map(|(p, _)| p)
        .filter(|p| p.run_id == run_id);
    let mut progress = existing.unwrap_or_else(|| crate::cmd::fleet::progress::RunProgress {
        schema_version: 1,
        run_id: run_id.to_string(),
        question: question.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        outcome: None,
        iteration: 0,
        model: None,
        budget_usd: None,
        spend_usd: 0.0,
        steps: vec![],
        artifact_path: None,
        error: None,
    });
    progress.outcome = Some("failed".to_string());
    progress.error = Some(error_summary.to_string());
    progress.finished_at = Some(chrono::Utc::now().to_rfc3339());
    progress.save(mur_home, DEFAULT_FLEET_NAME);
}

/// Back-fill `artifact_path` onto the progress record for `run_id` after
/// [`save_report`] succeeds, so `mur_job_status`'s fallback (Task 4) can
/// tell "report being saved" from "done, here's the path" — see
/// `ProgressPhase::ReportSaving` in `cmd/fleet/progress.rs`. A no-op when
/// the current record belongs to a different run.
fn backfill_artifact_path(mur_home: &Path, run_id: &str, path: &std::path::Path) {
    let Some((mut progress, _)) = crate::cmd::fleet::progress::load(mur_home, DEFAULT_FLEET_NAME)
    else {
        return;
    };
    if progress.run_id != run_id {
        return;
    }
    progress.artifact_path = Some(path.to_path_buf());
    progress.save(mur_home, DEFAULT_FLEET_NAME);
}

/// Resolve this invocation's run id: an externally-supplied `MUR_RUN_ID`
/// (e.g. set by the `fleet_run` MCP tool before spawning this process) when
/// present and non-empty, else a freshly minted v7 uuid — mirrors
/// `loop_run::resolve_run_id_from` (`mur_common::fleet::RUN_ID_ENV` is the
/// shared join key both honour).
fn resolve_run_id() -> String {
    match std::env::var(mur_common::fleet::RUN_ID_ENV).ok() {
        Some(v) if !v.is_empty() => v,
        _ => uuid::Uuid::now_v7().to_string(),
    }
}

pub async fn cmd_ask(mur_home: &Path, question: &str, run_id: Option<String>) -> Result<()> {
    let run_id = run_id.unwrap_or_else(resolve_run_id);
    // Same `set_var` block as `MUR_HOME` below (single-shot CLI process) —
    // so the loop this process spawns (Task 2, `loop_run.rs:420`) sees the
    // same id, whether it was handed to us or we just minted it.
    unsafe {
        std::env::set_var(mur_common::fleet::RUN_ID_ENV, &run_id);
    }
    // Read by both `fleet_run` (piped stdout) and a terminal user; must be
    // the first line so a poller can grab it without waiting on preflight.
    println!("run_id: {run_id}");

    match ask_inner(mur_home, question, &run_id).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Only preflight failures (points A–G) land here without a
            // progress file already carrying a loop-written outcome — a
            // loop-internal failure (`ask_inner`'s call into
            // `cmd_deep_research_run`) has already had its terminal state
            // stamped by `run_guarded` (`loop_run.rs:719-727`), so recording
            // over it would clobber the loop's own explanation.
            let already_terminal = crate::cmd::fleet::progress::load(mur_home, DEFAULT_FLEET_NAME)
                .map(|(p, _)| p.run_id == run_id && p.outcome.is_some())
                .unwrap_or(false);
            if !already_terminal {
                record_preflight_failure(mur_home, &run_id, question, &e.to_string());
            }
            Err(e)
        }
    }
}

async fn ask_inner(mur_home: &Path, question: &str, run_id: &str) -> Result<()> {
    // `cmd_start`/`cmd_mcp_pin` resolve their home via the `MUR_HOME` env
    // var (same caveat as `provision.rs`'s `grant_egress`). Process-lifetime
    // set_var is intentional here: `mur deep-research "<question>"` is a
    // single-shot CLI invocation, not a long-lived multi-threaded process
    // (mirrors provision.rs's `# Concurrency` note).
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }

    // Load the fleet FIRST and scope the preflight to its current members —
    // a prefix scan alone would restart surplus (stopped, dropped-from-
    // members) workers left over from a setup count-shrink, and a stray
    // non-member `dr_worker_*` agent provisioned without egress would make
    // every smart run bail forever.
    let fleet =
        crate::cmd::fleet::store::load_fleet(mur_home, DEFAULT_FLEET_NAME).map_err(|_| {
            anyhow::anyhow!("deep research is not set up yet — run `mur deep-research setup` first")
        })?;
    let status = scope_to_members(collect_status(mur_home, DEFAULT_FLEET_NAME), &fleet.members);
    let mut started: Vec<String> = Vec::new();
    for action in plan_preflight(&status)? {
        match action {
            PreflightAction::StartWorker(name) => {
                println!("starting worker {name} …");
                crate::cmd::agent::start::cmd_start(&name)?;
                started.push(name);
            }
            PreflightAction::RepinGateway(name) => {
                crate::cmd::agent_mcp_pin::cmd_mcp_pin(
                    &name,
                    super::provision::GATEWAY_MCP_NAME,
                    true, // force
                    true, // no_probe / non-interactive
                    None,
                    None,
                    None,
                )?;
            }
        }
    }

    // Give freshly-started workers a beat to bind their unix socket before
    // the run loop tries to dial them.
    for name in &started {
        let mut waited = Duration::ZERO;
        let step = Duration::from_millis(250);
        let timeout = Duration::from_secs(10);
        while !is_agent_running(mur_home, name) {
            if waited >= timeout {
                bail!("worker {name} did not come up within {timeout:?} after start");
            }
            std::thread::sleep(step);
            waited += step;
        }
    }

    // The question becomes the fleet goal; the existing run loop reads it.
    let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, DEFAULT_FLEET_NAME)?;
    fleet.goal = question.to_string();
    crate::cmd::fleet::store::save_fleet(mur_home, &fleet)?;

    // Baseline seq so only THIS run's events are considered for the report.
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let baseline_seq = svc
        .load_events(&fleet.channel_id)
        .ok()
        .and_then(|evs| evs.last().map(|e| e.seq))
        .unwrap_or(0);

    // Budget comes from fleet.yaml loop.budget_usd (set by setup); pass None
    // overrides so the existing precedence applies unchanged.
    super::run::cmd_deep_research_run(mur_home, DEFAULT_FLEET_NAME, None, None, None).await?;

    // Persist the synthesized report so the answer outlives the console
    // scrollback — and so a sandboxed caller (fleet_run tool) gets a file
    // path in the output instead of needing its own filesystem write grants.
    // Best-effort: a run that produced no report (guard-stopped) saves nothing.
    if let Ok(events) = svc.load_events(&fleet.channel_id)
        && let Some(report) = extract_report(&events, &fleet, baseline_seq)
        && let Ok(path) = save_report(mur_home, question, &report)
    {
        backfill_artifact_path(mur_home, run_id, &path);
        println!("Report: {}", path.display());
    }

    Ok(())
}

/// Pull the report text out of this run's channel events: the last
/// Agent-authored event containing the convergence marker as an own-line
/// sentinel (matching `channel_has_marker` semantics), falling back to the
/// last Agent-authored text of the run. Marker lines are stripped from the
/// saved text.
fn extract_report(
    events: &[mur_common::channel::ChannelEvent],
    fleet: &mur_common::fleet::Fleet,
    baseline_seq: u64,
) -> Option<String> {
    use mur_common::channel::ChannelActor;
    let marker = fleet
        .loop_cfg
        .as_ref()
        .and_then(|l| crate::cmd::fleet::done_policy::done_marker(&l.done_when));
    let agent_texts = events
        .iter()
        .filter(|e| e.seq > baseline_seq && matches!(e.actor, ChannelActor::Agent { .. }));
    let mut best: Option<&str> = None;
    let mut last: Option<&str> = None;
    for e in agent_texts {
        if let Some(t) = e.payload.get("text").and_then(|t| t.as_str()) {
            last = Some(t);
            if let Some(m) = marker
                && t.lines().any(|line| line.trim() == m)
            {
                best = Some(t);
            }
        }
    }
    let text = best.or(last)?;
    let cleaned: String = match marker {
        Some(m) => text
            .lines()
            .filter(|line| line.trim() != m)
            .collect::<Vec<_>>()
            .join("\n"),
        None => text.to_string(),
    };
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Write the report under `<mur_home>/artifacts/deep-research/` as
/// `<utc-timestamp>-<question-slug>.md` and return the path.
fn save_report(mur_home: &Path, question: &str, report: &str) -> Result<std::path::PathBuf> {
    let dir = mur_home.join("artifacts").join("deep-research");
    std::fs::create_dir_all(&dir)?;
    let slug: String = question
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(48)
        .collect();
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("{ts}-{slug}.md"));
    std::fs::write(&path, format!("# {question}\n\n{report}\n"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deep_research::status::{DeepResearchStatus, WorkerStatus};

    fn worker(name: &str, running: bool, egress: bool) -> WorkerStatus {
        WorkerStatus {
            name: name.into(),
            running,
            egress_granted: egress,
        }
    }

    #[test]
    fn no_workers_errors_pointing_at_setup() {
        let s = DeepResearchStatus {
            workers: vec![],
            fleet_exists: false,
            model: None,
        };
        let err = plan_preflight(&s).unwrap_err().to_string();
        assert!(err.contains("mur deep-research setup"));
    }

    #[test]
    fn missing_egress_errors_and_never_plans_a_grant() {
        let s = DeepResearchStatus {
            workers: vec![worker("dr_worker_1", true, false)],
            fleet_exists: true,
            model: Some("m".into()),
        };
        let err = plan_preflight(&s).unwrap_err().to_string();
        assert!(err.contains("egress"));
        assert!(err.contains("setup"));
    }

    #[test]
    fn only_stopped_workers_are_repinned_then_started() {
        let s = DeepResearchStatus {
            workers: vec![
                worker("dr_worker_1", false, true),
                worker("dr_worker_2", true, true),
            ],
            fleet_exists: true,
            model: Some("m".into()),
        };
        let plan = plan_preflight(&s).unwrap();
        // The running worker has proved its own pin; touching it from a
        // process with a different PATH is how a worker gets bricked.
        assert!(
            !plan.iter().any(
                |a| matches!(a, PreflightAction::StartWorker(n) | PreflightAction::RepinGateway(n) if n == "dr_worker_2")
            )
        );
        // The stopped one is healed: pin first (the runtime reads the
        // profile once, at start), then start.
        let names: Vec<&str> = plan
            .iter()
            .map(|a| match a {
                PreflightAction::RepinGateway(_) => "repin",
                PreflightAction::StartWorker(_) => "start",
            })
            .collect();
        assert_eq!(names, ["repin", "start"]);
        assert!(
            plan.iter().all(
                |a| matches!(a, PreflightAction::StartWorker(n) | PreflightAction::RepinGateway(n) if n == "dr_worker_1")
            )
        );
    }

    #[test]
    fn non_member_prefix_matched_worker_is_excluded_and_never_bails() {
        // A stray `dr_worker_extra` (stopped, no egress) prefix-matches but
        // is NOT a fleet member — it must be filtered out before
        // `plan_preflight` runs, so it neither gets a StartWorker/RepinGateway
        // plan entry nor causes the missing-egress bail.
        let s = DeepResearchStatus {
            workers: vec![
                worker("dr_worker_1", true, true),
                worker("dr_worker_extra", false, false),
            ],
            fleet_exists: true,
            model: Some("m".into()),
        };
        let members = vec!["dr_worker_1".to_string()];
        let scoped = scope_to_members(s, &members);
        assert_eq!(scoped.workers.len(), 1);
        let plan = plan_preflight(&scoped).unwrap();
        assert!(
            !plan
                .iter()
                .any(|a| matches!(a, PreflightAction::StartWorker(n) | PreflightAction::RepinGateway(n) if n == "dr_worker_extra"))
        );
    }

    #[test]
    fn record_preflight_failure_writes_minimal_record() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME).is_none());

        record_preflight_failure(
            tmp.path(),
            "run-1",
            "why is the sky blue",
            "no workers found",
        );

        let (p, _) =
            crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME).expect("written");
        assert_eq!(p.run_id, "run-1");
        assert_eq!(p.question, "why is the sky blue");
        assert_eq!(p.outcome.as_deref(), Some("failed"));
        assert!(p.error.as_deref().unwrap().contains("no workers found"));
        assert!(p.finished_at.is_some());
        assert_eq!(p.iteration, 0);
        assert!(p.steps.is_empty());
    }

    #[test]
    fn record_preflight_failure_updates_same_id_only() {
        let tmp = tempfile::tempdir().unwrap();

        // Existing record for the SAME run id, not yet terminal (as if a
        // partial write happened before preflight failed) — only
        // outcome/error/finished_at should change; everything else (e.g.
        // `iteration`, `steps`) is preserved.
        let mut existing = sample_progress("run-1", "original question");
        existing.iteration = 3;
        existing.save(tmp.path(), DEFAULT_FLEET_NAME);

        record_preflight_failure(tmp.path(), "run-1", "original question", "boom");

        let (p, _) = crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME).unwrap();
        assert_eq!(p.outcome.as_deref(), Some("failed"));
        assert_eq!(p.error.as_deref(), Some("boom"));
        assert!(p.finished_at.is_some());
        assert_eq!(p.iteration, 3, "unrelated fields must be preserved");

        // A DIFFERENT run id with a loop-written terminal outcome already on
        // disk: the file is last-run storage, so a fresh minimal record for
        // the new id overwrites it (recommended behaviour per the handoff).
        let mut other = sample_progress("run-OLD", "old question");
        other.outcome = Some("converged".to_string());
        other.finished_at = Some("2026-01-01T00:00:00Z".to_string());
        other.save(tmp.path(), DEFAULT_FLEET_NAME);

        record_preflight_failure(tmp.path(), "run-2", "new question", "setup missing");

        let (p2, _) = crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME).unwrap();
        assert_eq!(p2.run_id, "run-2");
        assert_eq!(p2.question, "new question");
        assert_eq!(p2.outcome.as_deref(), Some("failed"));
        assert_eq!(p2.iteration, 0);
    }

    #[test]
    fn backfill_artifact_path_reloads_and_saves() {
        let tmp = tempfile::tempdir().unwrap();

        let mut existing = sample_progress("run-1", "q");
        existing.finished_at = Some("2026-01-01T00:00:00Z".to_string());
        existing.outcome = Some("converged".to_string());
        existing.save(tmp.path(), DEFAULT_FLEET_NAME);

        let path = std::path::PathBuf::from("/tmp/report.md");
        backfill_artifact_path(tmp.path(), "run-1", &path);

        let (p, _) = crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME).unwrap();
        assert_eq!(p.artifact_path, Some(path));

        // A file whose run_id differs is left alone.
        let mut other = sample_progress("run-OTHER", "q2");
        other.finished_at = Some("2026-01-01T00:00:00Z".to_string());
        other.outcome = Some("converged".to_string());
        other.save(tmp.path(), DEFAULT_FLEET_NAME);

        backfill_artifact_path(
            tmp.path(),
            "run-1",
            &std::path::PathBuf::from("/tmp/should-not-apply.md"),
        );

        let (p2, _) = crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME).unwrap();
        assert_eq!(p2.run_id, "run-OTHER");
        assert_eq!(p2.artifact_path, None);
    }

    fn sample_progress(run_id: &str, question: &str) -> crate::cmd::fleet::progress::RunProgress {
        crate::cmd::fleet::progress::RunProgress {
            schema_version: 1,
            run_id: run_id.to_string(),
            question: question.to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: None,
            outcome: None,
            iteration: 0,
            model: None,
            budget_usd: None,
            spend_usd: 0.0,
            steps: vec![],
            artifact_path: None,
            error: None,
        }
    }
}
