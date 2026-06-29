//! `mur fleet run` — one iteration: fan the goal out to each member over the
//! shared channel via the existing DAG executor (delegation), then print replies.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::channel::ChannelActor;
use mur_common::fleet::{Job, JobStatus};
use mur_common::parallel::ParallelConfig;
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use super::store;

/// Phase 1 "plan": one parallel delegate-step per member (or per track if parallel config present),
/// each handed the goal (with approach injection for tracks).
/// (Phase 2 replaces this with a router-produced DAG.)
pub fn build_fleet_procedure(
    goal: &str,
    members: &[String],
    parallel: Option<&ParallelConfig>,
) -> Result<Procedure> {
    let steps = if let Some(cfg) = parallel {
        if members.is_empty() {
            bail!("parallel fleet requires at least one member");
        }
        // Parallel tracks: one step per track with approach injection into goal
        cfg.tracks
            .iter()
            .enumerate()
            .map(|(i, track_cfg)| {
                let injected_goal = format!("{goal}\n\nApproach: {}", track_cfg.approach);
                let member = members
                    .get(i % members.len())
                    .cloned()
                    .expect("members guard ensures non-empty");
                ProcedureStep {
                    description: format!("{}: {injected_goal}", track_cfg.name),
                    intent: Some(injected_goal),
                    delegate_to: Some(member),
                    id: Some(track_cfg.name.clone()),
                    ..Default::default()
                }
            })
            .collect()
    } else {
        // Regular fleet: one step per member with same goal
        members
            .iter()
            .map(|m| ProcedureStep {
                description: format!("{m}: {goal}"),
                intent: Some(goal.to_string()),
                delegate_to: Some(m.clone()),
                id: Some(m.clone()),
                ..Default::default()
            })
            .collect()
    };
    Ok(Procedure {
        variables: vec![],
        steps,
    })
}

/// Pure goal resolver: explicit arg > oldest queued job > standing goal.
/// Marks the chosen job Running (if a job was selected) before returning.
/// Returns (resolved_goal, active_job_or_none).
pub fn resolve_run_goal(
    mur_home: &Path,
    name: &str,
    job_arg: Option<String>,
    standing_goal: &str,
) -> Result<(String, Option<Job>)> {
    let mut active: Option<Job> = if let Some(text) = job_arg {
        // Explicit arg: enqueue and persist (one-shot, marked Done on finish).
        let job = super::jobs::enqueue_job(mur_home, name, &text, "cli")?;
        Some(job)
    } else {
        super::jobs::next_queued(mur_home, name)?
    };
    let goal = active
        .as_ref()
        .map(|j| j.text.clone())
        .unwrap_or_else(|| standing_goal.to_string());
    if let Some(job) = active.as_mut() {
        job.status = JobStatus::Running;
        job.started_at = Some(chrono::Utc::now().to_rfc3339());
        super::jobs::save_job(mur_home, name, job)?;
    }
    Ok((goal, active))
}

pub async fn cmd_fleet_run(mur_home: &Path, name: &str, job_arg: Option<String>) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        bail!("fleet '{name}' has no members");
    }
    if super::control::is_stopped(mur_home, name) {
        bail!(
            "fleet '{name}' is stopped (kill-switch). Run `mur fleet start {name}` to re-enable."
        );
    }
    let (goal, mut active_job) = resolve_run_goal(mur_home, name, job_arg, &fleet.goal)?;
    if goal.is_empty() {
        bail!(
            "fleet '{name}' has no goal and no queued job; pass one: mur fleet run {name} \"<job>\""
        );
    }
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let events = svc.load_events(&fleet.channel_id)?;
    // Cursor BEFORE this run so the reply tail prints only THIS run's events.
    let since = events.last().map(|e| e.seq).unwrap_or(0);
    // Plan with the resolved goal (overrides fleet.goal for this run).
    let planning_fleet = mur_common::fleet::Fleet {
        goal: goal.clone(),
        ..fleet.clone()
    };
    // Router plans which members do what (with deps); falls back to broadcast-to-all.
    let proc = super::plan::plan_via_router(mur_home, &planning_fleet, &events)
        .unwrap_or_else(|| {
            build_fleet_procedure(&goal, &fleet.members, fleet.parallel.as_ref())
                .expect("members validated by caller guard")
        });
    let run_id = format!("run-{}", uuid::Uuid::now_v7());
    let opts = crate::executor::dag::DagExecOptions {
        // Fail-closed default: do NOT blanket-approve. Fleet delegation steps carry
        // no risk tier today (member runtimes gate their own tools), but a future
        // router-emitted DAG with risk steps must fail-closed, never auto-approve
        // unattended. See the best-practice audit (OWASP Agentic ASI06).
        yes: false,
        channel_id: Some(fleet.channel_id.clone()),
        run_id: run_id.clone(),
        ..Default::default()
    };
    // skill_name here is a readable run-history label; channel routing still uses opts.channel_id.
    let exec_result =
        crate::executor::dag::execute_dag(mur_home, &format!("fleet:{}", fleet.name), &proc, &opts)
            .await;
    // Stamp job terminal before surfacing any error.
    if let Some(job) = active_job.as_mut() {
        job.run_id = Some(run_id);
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());
        match &exec_result {
            Ok(out) => {
                job.status = JobStatus::Done;
                job.result = out.output_text.clone().filter(|t| !t.is_empty());
            }
            Err(e) => {
                job.status = JobStatus::Failed;
                job.error = Some(format!("{e:#}"));
            }
        }
        super::jobs::save_job(mur_home, name, job)?;
    }
    let out = exec_result?;
    if let Some(t) = out.output_text.filter(|t| !t.is_empty()) {
        println!("{t}");
    }

    // Tail agent-authored replies from THIS run (seq > pre-run cursor), peer-writes-own.
    // Note: prints payload["text"]; confirm the exact reply payload shape in the live Harness test.
    for ev in svc
        .load_events(&fleet.channel_id)?
        .into_iter()
        .filter(|e| e.seq > since)
    {
        if let (ChannelActor::Agent { id }, Some(text)) = (
            &ev.actor,
            ev.payload
                .get("text")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty()),
        ) {
            println!("[{id}] {text}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fleet_procedure_one_delegate_step_per_member() {
        let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()], None).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("qa"));
        assert_eq!(p.steps[0].intent.as_deref(), Some("ship it"));
        assert!(p.steps[0].depends_on.is_empty()); // parallel rank 0
    }

    #[test]
    fn parallel_goal_injects_approach() {
        let cfg = ParallelConfig {
            mode: mur_common::parallel::ParallelMode::Speculative,
            tracks: vec![
                mur_common::parallel::TrackConfig {
                    name: "track1".to_string(),
                    approach: "functional style".to_string(),
                    model: None,
                },
                mur_common::parallel::TrackConfig {
                    name: "track2".to_string(),
                    approach: "imperative style".to_string(),
                    model: None,
                },
            ],
            judge: mur_common::parallel::JudgeConfig {
                model: "test-model".to_string(),
                rubric: mur_common::parallel::Rubric::default(),
            },
            pre_filter: vec![],
        };
        let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()], Some(&cfg)).unwrap();
        assert_eq!(p.steps.len(), 2);
        // First track should have functional style approach injected
        assert_eq!(p.steps[0].id.as_deref(), Some("track1"));
        let intent0 = p.steps[0].intent.as_deref().unwrap();
        assert!(intent0.contains("ship it"));
        assert!(intent0.contains("functional style"));
        assert!(intent0.contains("Approach:"));
        // Second track should have imperative style approach injected
        assert_eq!(p.steps[1].id.as_deref(), Some("track2"));
        let intent1 = p.steps[1].intent.as_deref().unwrap();
        assert!(intent1.contains("ship it"));
        assert!(intent1.contains("imperative style"));
        assert!(intent1.contains("Approach:"));
    }

    #[test]
    fn resolve_goal_prefers_arg_then_queued_then_standing() {
        // pure resolution helper — no execution
        use mur_common::fleet::JobStatus;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("standing".into()),
            None,
        )
        .unwrap();

        // no arg, empty queue → standing goal
        let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
        assert_eq!(goal, "standing");
        assert!(job.is_none());

        // queued job → job's text, marked running
        super::super::jobs::enqueue_job(home, "dev", "queued-work", "cli").unwrap();
        let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
        assert_eq!(goal, "queued-work");
        assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);

        // explicit arg jumps ahead of queue and is persisted
        let (goal, job) = resolve_run_goal(home, "dev", Some("urgent".into()), "standing").unwrap();
        assert_eq!(goal, "urgent");
        assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);
        // arg job must be persisted (source=="cli")
        let all_jobs = super::super::jobs::list_jobs(home, "dev").unwrap();
        assert!(
            all_jobs.iter().any(|j| j.text == "urgent"),
            "arg job should be persisted"
        );
    }
}
