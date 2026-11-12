//! `mur job run-request <path>` (hidden): host a `parallel_jobs` run that a
//! sealed agent asked `mur-daemon` for (#2379).
//!
//! The daemon launches this outside every sandbox, so it reads the router key
//! from disk like a terminal run: the run's channel events, and the
//! write-grant gate's `HitlRequest`, are signed and answerable.
//!
//! It re-reads and re-verifies the request rather than trusting the daemon's
//! verdict, then runs the fan-out under the ids the agent already returned as
//! its handle, and stays alive until the run ends — this process exists only
//! to host it.

use std::path::Path;

use anyhow::{Context, Result};
use mur_common::config::Config;
use mur_common::hitl::Unanswered;

use crate::executor::jobs::{DispatchAs, Job, PresetIds, RunCwd, dispatch_parallel_jobs_as};
use crate::executor::jobs_request::{authorize, read_request, verify_request};

pub async fn run(mur_home: &Path, path: &Path) -> Result<()> {
    let req = read_request(path)?;
    // No freshness check here: the daemon admitted it inside the window, and
    // a slow launch must not strand a handle the agent already returned.
    verify_request(mur_home, &req, None).context("request verification")?;
    let cfg = Config::try_load(&mur_home.join("config.yaml"))
        .map_err(|e| anyhow::anyhow!("config unreadable; refusing to run: {e}"))?;
    authorize(mur_home, &req, &cfg)?;

    let d = req.dispatch;
    let jobs: Vec<Job> = d
        .jobs
        .into_iter()
        .map(|j| Job {
            description: j.description,
            assignee: crate::a2a_dial::canonicalize_agent_name(mur_home, &j.agent),
        })
        .collect();
    let cwd = RunCwd::from_tool_args(d.cwd.as_deref(), d.cwd_inferred);
    let dispatched = dispatch_parallel_jobs_as(
        mur_home,
        &jobs,
        Some(d.max_concurrency as usize),
        // Never `yes` on this path (spec Q3): the request cannot carry it,
        // and an agent may not approve its own write grant.
        false,
        &cwd,
        DispatchAs {
            ids: Some(PresetIds {
                run_id: d.run_id.clone(),
                channel_id: d.channel_id.clone(),
            }),
            // Nobody is blocked on this process, and the agent already holds
            // the handle: wait for the human instead of failing the run.
            unanswered: Unanswered::Wait,
        },
    )
    .await
    .with_context(|| format!("parallel_jobs run {}", d.run_id))?;
    println!(
        "parallel_jobs run {} on channel {} (requested by {})",
        dispatched.run_id, dispatched.channel_id, d.requested_by
    );
    dispatched
        .handle
        .await
        .map_err(|e| anyhow::anyhow!("parallel_jobs task panicked: {e}"))??;
    Ok(())
}

#[cfg(test)]
#[path = "job_run_request_tests.rs"]
mod tests;
