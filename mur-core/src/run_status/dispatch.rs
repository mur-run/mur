//! The daemon's dispatch verdict as a run record (#2116).
//!
//! A cross-router `fleet_run` returns its handle BEFORE any process exists:
//! the daemon decides later and writes
//! `fleet-state/<fleet>/runs/<run_id>/dispatch.json`. A
//! rejected request never gets a `run.json` or a channel, so without this the
//! handle polls as "no run recorded" and the reason sits unread beside it.
//! Consulted only when there is no run record — once the child records one,
//! that record is the truth.

use std::path::Path;

use anyhow::Context;
use mur_track::fleet_dispatch_request::{
    FLEET_DISPATCH_RECORD_FILE, FLEET_RUN_LOG_FILE, FleetDispatchRecord, FleetDispatchStatus,
};

use super::{RUN_SCHEMA, RunKind, RunState, State, StepState};

/// Step id carrying the daemon's verdict, so `mur job status` prints the
/// rejection reason on the line where it prints every step's error.
pub const DISPATCH_STEP: &str = "dispatch";

/// `Ok(None)` when the daemon wrote no verdict for `run_id`. A present but
/// unreadable verdict is an error, never "no such run".
pub fn load(mur_home: &Path, run_id: &str) -> anyhow::Result<Option<RunState>> {
    load_in(mur_home, None, run_id)
}

/// [`load`] from the directory `store::run_dir_in` resolves.
pub fn load_in(
    mur_home: &Path,
    fleet: Option<&str>,
    run_id: &str,
) -> anyhow::Result<Option<RunState>> {
    // Resolved like any run directory: the daemon writes it into the
    // fleet's tree (#2316); a verdict from an older daemon sits in `runs/`.
    let path = super::store::run_dir_in(mur_home, fleet, run_id).join(FLEET_DISPATCH_RECORD_FILE);
    let bytes = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        Ok(b) => b,
    };
    let record: FleetDispatchRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    let log = path.with_file_name(FLEET_RUN_LOG_FILE);
    Ok(Some(to_run_state(record, &log)))
}

/// How many trailing log lines a dead launch carries as its step error —
/// enough for an `Error:` line and its `Caused by:` chain.
const LOG_TAIL_LINES: usize = 12;

/// A launch whose child is gone and never recorded its run: nothing will
/// record one now (#2345). Only a recorded pid can say so — a record from an
/// older daemon has none and keeps the old `running` + `unknown` answer.
fn launched_child_exited(r: &FleetDispatchRecord) -> bool {
    r.status == FleetDispatchStatus::Launched
        && r.pid
            .is_some_and(|pid| pid != 0 && !mur_common::lock_file::pid_alive(pid))
}

fn log_tail(log: &Path) -> String {
    let where_ = format!("see {}", log.display());
    match std::fs::read_to_string(log) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n");
            if tail.is_empty() {
                format!("the run exited before recording itself; its log is empty ({where_})")
            } else {
                format!("the run exited before recording itself ({where_}):\n{tail}")
            }
        }
        Err(e) => format!("the run exited before recording itself; log unreadable: {e} ({where_})"),
    }
}

fn to_run_state(r: FleetDispatchRecord, log: &Path) -> RunState {
    let exited = launched_child_exited(&r);
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
        // Launched, and the child is gone without a record: it failed before
        // it could say so. Its log holds the only error (#2345).
        FleetDispatchStatus::Launched if exited => (
            State::Failed,
            format!(
                "fleet {} — launched by the MUR daemon as {} (requested by {}); exited before recording its run",
                r.fleet,
                r.router.as_deref().unwrap_or("?"),
                r.dispatched_by
            ),
            Some(log_tail(log)),
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

    /// Where `mur-daemon` writes the verdict: the fleet's own tree.
    fn write(home: &Path, run_id: &str, body: serde_json::Value) {
        let dir = mur_common::paths::fleet_run_dir(home, "council", run_id);
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

    /// #2345: the child died before recording its run (e.g. delegation
    /// blocked before dispatch). The handle must stop saying `running` and
    /// surface the log's last lines, where the only real error lives.
    #[cfg(unix)]
    #[test]
    fn a_launched_child_that_exited_without_a_record_is_failed_with_its_log_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        write(
            tmp.path(),
            "fleet-council-3",
            serde_json::json!({"run_id": "fleet-council-3", "fleet": "council",
                "dispatched_by": "mur", "router": "orchestrator", "status": "launched",
                "pid": dead_pid, "decided_at": "2026-09-27T00:00:00Z"}),
        );
        let dir = mur_common::paths::fleet_run_dir(tmp.path(), "council", "fleet-council-3");
        std::fs::write(
            dir.join(FLEET_RUN_LOG_FILE),
            "starting\nError: fleet 'council': delegation blocked before dispatch:\n  needs approval, then re-run.\n",
        )
        .unwrap();
        let status = crate::run_status::status_of(tmp.path(), "fleet-council-3")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Failed);
        let err = status.run.steps[0].error.as_deref().unwrap();
        assert!(err.contains("delegation blocked before dispatch"), "{err}");
        assert!(err.contains("then re-run"), "{err}");
        assert!(err.contains(FLEET_RUN_LOG_FILE), "points at the log: {err}");
    }

    /// #2345 review: the child records its run, completes, and exits
    /// between the caller's absence check and the dead-pid check. The
    /// record it wrote must win over the synthetic failure.
    #[cfg(unix)]
    #[test]
    fn a_record_published_after_the_absence_check_beats_the_dead_pid_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = "fleet-council-5";
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        write(
            tmp.path(),
            run_id,
            serde_json::json!({"run_id": run_id, "fleet": "council",
                "dispatched_by": "mur", "router": "orchestrator", "status": "launched",
                "pid": dead_pid, "decided_at": "2026-09-27T00:00:00Z"}),
        );
        let dir = mur_common::paths::fleet_run_dir(tmp.path(), "council", run_id);
        assert!(!dir.join("run.json").exists(), "absence is the premise");
        let now = chrono::Utc::now();
        let completed = RunState {
            schema: RUN_SCHEMA,
            run_id: run_id.to_string(),
            channel_id: None,
            kind: RunKind::Fleet,
            label: "fleet council".to_string(),
            pid: dead_pid,
            started_at: now,
            last_heartbeat_at: Some(now),
            state: State::Done,
            steps: vec![],
            blocked_on: None,
            binary_version: "0.0.0-test".to_string(),
            build_sha: "deadbee".to_string(),
        };
        // The window: the absence check has already run; the child now
        // publishes its record, then the verdict sees its pid dead.
        let record = crate::run_status::unrecorded(tmp.path(), None, run_id, |home, fleet, id| {
            std::fs::write(
                dir.join("run.json"),
                serde_json::to_vec(&completed).unwrap(),
            )
            .unwrap();
            load_in(home, fleet, id)
        })
        .unwrap()
        .unwrap();
        assert_eq!(record.state, State::Done);
    }

    /// The same record while the child is alive stays `running`.
    #[test]
    fn a_launched_child_still_alive_is_running() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "fleet-council-4",
            serde_json::json!({"run_id": "fleet-council-4", "fleet": "council",
                "dispatched_by": "mur", "router": "orchestrator", "status": "launched",
                "pid": std::process::id(), "decided_at": "2026-09-27T00:00:00Z"}),
        );
        let status = crate::run_status::status_of(tmp.path(), "fleet-council-4")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Running);
    }

    #[test]
    fn an_unreadable_verdict_is_an_error_not_no_such_run() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = mur_common::paths::fleet_run_dir(tmp.path(), "x", "fleet-x-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FLEET_DISPATCH_RECORD_FILE), "{").unwrap();
        assert!(crate::run_status::status_of(tmp.path(), "fleet-x-1").is_err());
    }

    /// A verdict an older daemon left in `runs/` still reports.
    #[test]
    fn a_legacy_verdict_in_runs_is_still_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join(mur_common::paths::RUNS)
            .join("fleet-council-9");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(FLEET_DISPATCH_RECORD_FILE),
            serde_json::json!({"run_id": "fleet-council-9", "fleet": "council",
                "dispatched_by": "mur", "status": "rejected", "reason": "old",
                "decided_at": "2026-09-27T00:00:00Z"})
            .to_string(),
        )
        .unwrap();
        let status = crate::run_status::status_of(tmp.path(), "fleet-council-9")
            .unwrap()
            .unwrap();
        assert_eq!(status.state, State::Failed);
    }
}
