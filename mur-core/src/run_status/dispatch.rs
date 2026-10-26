//! The daemon's dispatch verdict as a run record (#2116).
//!
//! A cross-router `fleet_run` returns its handle BEFORE any process exists:
//! the daemon decides later and writes `runs/<run_id>/dispatch.json`. A
//! rejected request never gets a `run.json` or a channel, so without this the
//! handle polls as "no run recorded" and the reason sits unread beside it.
//! Consulted only when there is no run record — once the child records one,
//! that record is the truth.

use std::path::Path;

use anyhow::Context;
use mur_track::fleet_dispatch_request::{
    FLEET_DISPATCH_RECORD_FILE, FleetDispatchRecord, FleetDispatchStatus,
};

use super::{RUN_SCHEMA, RunKind, RunState, State, StepState};

/// Step id carrying the daemon's verdict, so `mur job status` prints the
/// rejection reason on the line where it prints every step's error.
pub const DISPATCH_STEP: &str = "dispatch";

/// `Ok(None)` when the daemon wrote no verdict for `run_id`. A present but
/// unreadable verdict is an error, never "no such run".
pub fn load(mur_home: &Path, run_id: &str) -> anyhow::Result<Option<RunState>> {
    let path = mur_home
        .join(mur_common::paths::RUNS)
        .join(run_id)
        .join(FLEET_DISPATCH_RECORD_FILE);
    let bytes = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        Ok(b) => b,
    };
    let record: FleetDispatchRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(to_run_state(record)))
}

fn to_run_state(r: FleetDispatchRecord) -> RunState {
    let (state, label, error) = match r.status {
        FleetDispatchStatus::Rejected => (
            State::Failed,
            format!(
                "fleet {} — rejected by the MUR daemon (requested by {})",
                r.fleet, r.dispatched_by
            ),
            Some(
                r.reason
                    .unwrap_or_else(|| "rejected (no reason recorded)".to_string()),
            ),
        ),
        // Launched, but the child has not recorded its run yet. Running with
        // no heartbeat classifies as `unknown` liveness — the honest answer.
        FleetDispatchStatus::Launched => (
            State::Running,
            format!(
                "fleet {} — launched by the MUR daemon as {} (requested by {}); waiting for the run to record",
                r.fleet,
                r.router.as_deref().unwrap_or("?"),
                r.dispatched_by
            ),
            None,
        ),
    };
    RunState {
        schema: RUN_SCHEMA,
        run_id: r.run_id,
        channel_id: None,
        kind: RunKind::Fleet,
        label,
        // No process of ours; `last_heartbeat_at: None` keeps `classify` off
        // the pid (see `rebuild`).
        pid: 0,
        started_at: r.decided_at,
        last_heartbeat_at: None,
        state,
        steps: vec![StepState {
            id: DISPATCH_STEP.to_string(),
            member: r.router,
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
        let dir = home.join(mur_common::paths::RUNS).join(run_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FLEET_DISPATCH_RECORD_FILE), body.to_string()).unwrap();
    }

    #[test]
    fn a_rejection_reads_as_a_failed_fleet_run_carrying_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "fleet-council-1",
            serde_json::json!({"run_id": "fleet-council-1", "fleet": "council",
                "dispatched_by": "mur", "status": "rejected", "reason": "not allowed",
                "decided_at": "2026-09-27T00:00:00Z"}),
        );
        let status = crate::run_status::status_of(tmp.path(), "fleet-council-1")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Failed);
        assert_eq!(status.run.kind, RunKind::Fleet);
        assert_eq!(status.run.steps[0].id, DISPATCH_STEP);
        assert_eq!(status.run.steps[0].error.as_deref(), Some("not allowed"));
    }

    #[test]
    fn a_launch_waiting_for_its_record_is_running_with_unknown_liveness() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "fleet-council-2",
            serde_json::json!({"run_id": "fleet-council-2", "fleet": "council",
                "dispatched_by": "mur", "router": "orchestrator", "status": "launched",
                "decided_at": "2026-09-27T00:00:00Z"}),
        );
        let status = crate::run_status::status_of(tmp.path(), "fleet-council-2")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Running);
        assert_eq!(status.liveness, crate::run_status::Liveness::Unknown);
        assert!(!status.abandoned);
    }

    #[test]
    fn an_unreadable_verdict_is_an_error_not_no_such_run() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(mur_common::paths::RUNS).join("fleet-x-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FLEET_DISPATCH_RECORD_FILE), "{").unwrap();
        assert!(crate::run_status::status_of(tmp.path(), "fleet-x-1").is_err());
    }
}
