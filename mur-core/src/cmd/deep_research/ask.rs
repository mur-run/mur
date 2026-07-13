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
    // var (same caveat as `provision.rs`'s `grant_egress`).
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }

    let status = collect_status(mur_home, DEFAULT_FLEET_NAME);
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

    // Budget comes from fleet.yaml loop.budget_usd (set by setup); pass None
    // overrides so the existing precedence applies unchanged.
    super::run::cmd_deep_research_run(mur_home, DEFAULT_FLEET_NAME, None, None, None).await?;

    Ok(())
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
}
