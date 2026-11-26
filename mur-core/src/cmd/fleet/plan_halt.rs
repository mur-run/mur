//! #2502: what a run does when the kill-switch lands while the router is
//! still planning. There is no plan and no DAG yet, so the job is closed
//! here, the same way a stopped DAG closes it: `Canceled` for a clean stop,
//! `Failed` when the router refused the cancel and may still be spending.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::fleet::{Job, JobStatus};

use super::loop_run::LoopStop;
use super::plan::PlanHalt;

/// A job stopped while its router planned. Matches the DAG path's wording.
pub(super) const STOPPED_TEXT: &str = "stopped by `mur fleet stop` while the router was planning";

/// A stop whose `tasks/cancel` the router refused.
pub(super) const REFUSED_TEXT: &str = "stopped by `mur fleet stop` while the router was planning, \
     but the router refused the cancel; its planning turn may still be running";

/// Record `halt` on the run's job (if any) and say how the loop ends.
/// The caller saves the job.
pub(super) fn settle(halt: PlanHalt, job: Option<&mut Job>, run_id: &str) -> LoopStop {
    let (status, text, stop) = match &halt {
        PlanHalt::Stopped => (
            JobStatus::Canceled,
            STOPPED_TEXT.to_string(),
            LoopStop::Stopped,
        ),
        PlanHalt::CancelRefused(e) => (
            JobStatus::Failed,
            format!("{REFUSED_TEXT}: {e:#}"),
            LoopStop::IterationFailed,
        ),
    };
    if let Some(job) = job {
        if !run_id.is_empty() {
            job.run_id = Some(run_id.to_string());
        }
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());
        job.status = status;
        job.error = Some(text);
    }
    stop
}

/// End a one-shot `mur fleet run` stopped during planning: close its job and
/// return what the run reports. A refused cancel is an error, a clean stop is
/// not — the same split a stopped DAG makes.
pub(super) fn end_one_shot(
    mur_home: &Path,
    name: &str,
    halt: PlanHalt,
    mut job: Option<Job>,
) -> Result<()> {
    let refused = matches!(halt, PlanHalt::CancelRefused(_));
    // The run id is minted after planning, so none is recorded here.
    settle(halt, job.as_mut(), "");
    if let Some(job) = &job {
        super::jobs::save_job(mur_home, name, job)?;
    }
    if refused {
        bail!("fleet '{name}': {REFUSED_TEXT}");
    }
    println!("── fleet '{name}': {STOPPED_TEXT} ──");
    Ok(())
}

// The socket tests need a unix listener, as the DAG stop tests do.
#[cfg(all(test, unix))]
#[path = "plan_halt_tests.rs"]
mod plan_halt_tests;
