//! `mur job run-request <path>` (hidden): host a `parallel_jobs` run that a
//! sealed agent asked `mur-daemon` for (#2379).
//!
//! The daemon launches this outside every sandbox, so it reads the router key
//! from disk like a terminal run: the run's channel events, and the
//! write-grant gate's `HitlRequest`, are signed and answerable.
//!
//! It reads the request only from the daemon's own tree (never the shared
//! inbox) and re-verifies it rather than trusting the daemon's verdict, then runs the fan-out under the ids the agent already returned as
//! its handle, and stays alive until the run ends — this process exists only
//! to host it.

use std::path::Path;

use anyhow::{Context, Result};
use mur_common::config::Config;
use mur_common::hitl::Unanswered;

use crate::executor::jobs::{DispatchAs, Job, PresetIds, RunCwd, dispatch_parallel_jobs_as};
use crate::executor::jobs_request::{authorize, verify_request};
use mur_track::jobs_dispatch_request::{
    JOBS_DISPATCH_ADMITTED_FILE, JOBS_DISPATCH_STATE_DIR, JobsDispatchRequest,
    jobs_dispatch_run_dir,
};

pub async fn run(mur_home: &Path, path: &Path) -> Result<()> {
    let req = admitted_request(mur_home, path)?;
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

/// Read the request only from where the daemon puts the one it admitted:
/// `jobs-dispatch/<run_id>/request.yaml`, a tree no sandboxed agent can
/// write, through a no-link read. Any other path is refused, so this child
/// can never be pointed at the shared inbox, where the file could be swapped
/// after admission.
fn admitted_request(mur_home: &Path, path: &Path) -> Result<JobsDispatchRequest> {
    let dir = path.parent().context("request path has no parent")?;
    let run_id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("request path names no run")?;
    let expected = jobs_dispatch_run_dir(mur_home, run_id);
    anyhow::ensure!(
        dir == expected && path.file_name() == Some(JOBS_DISPATCH_ADMITTED_FILE.as_ref()),
        "{} is not a daemon-admitted request ({}/<run_id>/{})",
        path.display(),
        mur_home.join(JOBS_DISPATCH_STATE_DIR).display(),
        JOBS_DISPATCH_ADMITTED_FILE
    );
    let bytes = mur_common::paths::read_unlinked(dir, JOBS_DISPATCH_ADMITTED_FILE)
        .with_context(|| format!("read {}", path.display()))?;
    let req: JobsDispatchRequest =
        serde_yaml_ng::from_slice(&bytes).context("parse admitted request")?;
    anyhow::ensure!(
        req.dispatch.run_id == run_id,
        "admitted request names run {}, not {run_id}",
        req.dispatch.run_id
    );
    Ok(req)
}

#[cfg(test)]
#[path = "job_run_request_tests.rs"]
mod tests;
