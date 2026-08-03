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
        if !w.running {
            plan.push(PreflightAction::StartWorker(w.name.clone()));
        }
        // Unconditional idempotent re-pin: cheaper than drift detection and
        // covers the known gateway-binary-swap failure mode.
        plan.push(PreflightAction::RepinGateway(w.name.clone()));
    }
    Ok(plan)
}

pub async fn cmd_ask(mur_home: &Path, question: &str) -> Result<()> {
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
    fn stopped_worker_planned_for_start_and_repin_always() {
        let s = DeepResearchStatus {
            workers: vec![
                worker("dr_worker_1", false, true),
                worker("dr_worker_2", true, true),
            ],
            fleet_exists: true,
            model: Some("m".into()),
        };
        let plan = plan_preflight(&s).unwrap();
        assert!(
            plan.iter()
                .any(|a| matches!(a, PreflightAction::StartWorker(n) if n == "dr_worker_1"))
        );
        assert!(
            !plan
                .iter()
                .any(|a| matches!(a, PreflightAction::StartWorker(n) if n == "dr_worker_2"))
        );
        // One re-pin per worker (idempotent, covers binary-swap drift):
        assert_eq!(
            plan.iter()
                .filter(|a| matches!(a, PreflightAction::RepinGateway(_)))
                .count(),
            2
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
}
