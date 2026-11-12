//! The daemon's `parallel_jobs` dispatch verdict as a run record (#2379).
//!
//! A sealed agent's `parallel_jobs` returns its handle BEFORE any process
//! exists: the daemon decides later and writes
//! `jobs-dispatch/<run_id>/jobs-dispatch.json` in its own tree. A rejected request
//! never gets a `run.json` or a channel, so without this the handle polls as
//! "no run recorded" and the reason sits unread. Consulted only when there is
//! no run record — once the child records one, that record is the truth.

use std::path::Path;

use anyhow::Context;
use mur_track::jobs_dispatch_request::{
    JOBS_DISPATCH_RECORD_FILE, JOBS_RUN_LOG_FILE, JobsDispatchRecord, JobsDispatchStatus,
};

use super::dispatch::{DISPATCH_STEP, log_tail};
use super::{RUN_SCHEMA, RunKind, RunState, State, StepState};

/// `Ok(None)` when the daemon wrote no jobs verdict for `run_id`. A present
/// but unreadable verdict is an error, never "no such run". A fleet-scoped
/// lookup never finds one: these live in the daemon's own
/// `<mur_home>/jobs-dispatch/<run_id>/` tree only.
pub fn load_in(
    mur_home: &Path,
    fleet: Option<&str>,
    run_id: &str,
) -> anyhow::Result<Option<RunState>> {
    if fleet.is_some() {
        return Ok(None);
    }
    if !mur_common::paths::is_single_segment(run_id) {
        return Ok(None);
    }
    // Only the daemon's own tree, never the id search: a verdict anywhere an
    // agent can write would be a forgery.
    let dir = mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(mur_home, run_id);
    let path = dir.join(JOBS_DISPATCH_RECORD_FILE);
    let bytes = match mur_common::paths::read_unlinked(&dir, JOBS_DISPATCH_RECORD_FILE) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        Ok(b) => b,
    };
    let record: JobsDispatchRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    let log = path.with_file_name(JOBS_RUN_LOG_FILE);
    Ok(Some(to_run_state(record, &log)))
}

fn launched_child_exited(r: &JobsDispatchRecord) -> bool {
    r.status == JobsDispatchStatus::Launched
        && r.pid
            .is_some_and(|pid| pid != 0 && !mur_common::lock_file::pid_alive(pid))
}

fn to_run_state(r: JobsDispatchRecord, log: &Path) -> RunState {
    let exited = launched_child_exited(&r);
    let who = &r.dispatched_by;
    let n = r.jobs;
    let (state, label, error) = match r.status {
        JobsDispatchStatus::Rejected => (
            State::Failed,
            format!("{n} parallel job(s) — rejected by the MUR daemon (requested by {who})"),
            Some(
                r.reason
                    .unwrap_or_else(|| "rejected (no reason recorded)".to_string()),
            ),
        ),
        JobsDispatchStatus::Launched if exited => (
            State::Failed,
            format!(
                "{n} parallel job(s) — launched by the MUR daemon (requested by {who}); \
                 exited before recording its run"
            ),
            Some(log_tail(log)),
        ),
        JobsDispatchStatus::Launched => (
            State::Running,
            format!(
                "{n} parallel job(s) — launched by the MUR daemon (requested by {who}); \
                 waiting for the run to record"
            ),
            None,
        ),
    };
    RunState {
        schema: RUN_SCHEMA,
        run_id: r.run_id,
        channel_id: Some(r.channel_id),
        kind: RunKind::Job,
        label,
        pid: 0,
        started_at: r.decided_at,
        last_heartbeat_at: None,
        state,
        steps: vec![StepState {
            id: DISPATCH_STEP.to_string(),
            member: None,
            state,
            started_at: Some(r.decided_at),
            ended_at: state.is_terminal().then_some(r.decided_at),
            error,
        }],
        blocked_on: None,
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        build_sha: mur_common::build::SHORT_SHA.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, run_id: &str, body: serde_json::Value) {
        let dir = mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(home, run_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(JOBS_DISPATCH_RECORD_FILE), body.to_string()).unwrap();
    }

    #[test]
    fn a_rejection_reads_as_a_failed_job_run_carrying_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "run-1",
            serde_json::json!({"run_id": "run-1", "channel_id": "c", "dispatched_by": "pm",
                "jobs": 2, "status": "rejected", "reason": "target 'x' is not allowed",
                "decided_at": "2026-11-12T00:00:00Z"}),
        );
        let status = crate::run_status::status_of(tmp.path(), "run-1")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Failed);
        assert_eq!(status.run.kind, RunKind::Job);
        assert_eq!(
            status.run.steps[0].error.as_deref(),
            Some("target 'x' is not allowed")
        );
    }

    #[test]
    fn a_launch_waiting_for_its_record_is_running() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "run-2",
            serde_json::json!({"run_id": "run-2", "channel_id": "c", "dispatched_by": "pm",
                "jobs": 1, "status": "launched", "pid": std::process::id(),
                "decided_at": "2026-11-12T00:00:00Z"}),
        );
        let status = crate::run_status::status_of(tmp.path(), "run-2")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Running);
    }

    #[test]
    fn a_fleet_scoped_lookup_never_reads_a_jobs_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "run-3",
            serde_json::json!({"run_id": "run-3", "channel_id": "c", "dispatched_by": "pm",
                "jobs": 1, "status": "rejected", "decided_at": "2026-11-12T00:00:00Z"}),
        );
        assert!(
            load_in(tmp.path(), Some("council"), "run-3")
                .unwrap()
                .is_none()
        );
    }
}
