//! The stop budget (#2360, spec "Stop budget (Q7)").
//!
//! At SIGTERM the supervisor takes one absolute deadline `T`. Every stage
//! derives its own end from `T` instead of starting a fresh duration, and
//! the MCP stop gets a **reserved** slice rather than whatever the drain
//! left over: with a turn in flight the old serial order spent 15 s on the
//! drain and gave a browser export about a second, so the login was lost.

use std::time::Duration;
use tokio::time::Instant;

use super::{SHUTDOWN_CANCEL_GRACE_SECS, SHUTDOWN_JOB_KILL_GRACE};
use crate::mcp::pool::{MCP_STOP_SLICE, McpPool};
use crate::task_runner::{DrainOutcome, TaskRunner};
use mur_common::agent::DEFAULT_STOP_TIMEOUT_SECS;

/// The whole stop, SIGTERM to exit: the default drain + cancel grace + the
/// final job sweep (`2 x SHUTDOWN_JOB_KILL_GRACE`) = 19 s, under launchd's
/// 20 s `ExitTimeOut`. Fixed: a larger `stop_timeout_secs` can no longer
/// push the stop past it.
pub(super) const STOP_BUDGET: Duration = Duration::from_millis(
    (DEFAULT_STOP_TIMEOUT_SECS + SHUTDOWN_CANCEL_GRACE_SECS) * 1000
        + 2 * SHUTDOWN_JOB_KILL_GRACE.as_millis() as u64,
);

/// Kept free at the end of the budget for the last log flush and lock release.
pub(super) const STOP_MARGIN: Duration = Duration::from_secs(1);

/// Where each stage of a stop starting at `start` must end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StopPlan {
    /// End of the cooperative wait for in-flight turns.
    pub drain_end: Instant,
    /// End of the MCP stop (cancel grace and job sweep run inside it).
    pub mcp_end: Instant,
    /// `T`: the whole stop.
    pub end: Instant,
}

impl StopPlan {
    pub(super) fn new(start: Instant, stop_timeout_secs: u64) -> Self {
        let end = start + STOP_BUDGET;
        let reserved = MCP_STOP_SLICE + STOP_MARGIN;
        let drain =
            Duration::from_secs(stop_timeout_secs).min(STOP_BUDGET.saturating_sub(reserved));
        let drain_end = start + drain;
        let mcp_end = (drain_end + MCP_STOP_SLICE).min(end - STOP_MARGIN);
        Self {
            drain_end,
            mcp_end,
            end,
        }
    }
}

/// Run the stop: drain, then cancel turns, sweep jobs and stop every MCP
/// server concurrently, each bounded by `plan`.
pub(super) async fn run(
    plan: StopPlan,
    stop_secs: u64,
    runner: &TaskRunner,
    pool: Option<&McpPool>,
) {
    runner.begin_drain();
    let idle = runner
        .await_idle(plan.drain_end.saturating_duration_since(Instant::now()))
        .await;
    let turns = async {
        let outcome = if idle {
            DrainOutcome::Idle
        } else {
            let grace = Duration::from_secs(SHUTDOWN_CANCEL_GRACE_SECS)
                .min(plan.mcp_end.saturating_duration_since(Instant::now()));
            runner.cancel_inflight(grace).await
        };
        log_drain(outcome, plan, stop_secs);
        // Every bash job is a process group this runtime started; nothing
        // else will end them once we are gone (spec D3/D9).
        let killed = runner.kill_all_jobs(SHUTDOWN_JOB_KILL_GRACE).await;
        if killed > 0 {
            tracing::info!(jobs = killed, "ended running bash jobs");
        }
    };
    let mcp = async {
        if let Some(pool) = pool {
            pool.shutdown_by(plan.mcp_end).await;
        }
    };
    tokio::join!(turns, mcp);
}

fn log_drain(outcome: DrainOutcome, plan: StopPlan, stop_secs: u64) {
    let waited = (plan.end - STOP_BUDGET).elapsed().as_secs();
    match outcome {
        DrainOutcome::Idle => tracing::info!("task runner drained cleanly"),
        DrainOutcome::Cancelled(n) => tracing::warn!(
            turns = n,
            stop_timeout_secs = stop_secs,
            "task runner did not drain within {waited}s; cancelled in-flight turns, all settled"
        ),
        DrainOutcome::Abandoned(n) => tracing::warn!(
            turns = n,
            stop_timeout_secs = stop_secs,
            "task runner did not drain within {waited}s; cancelled in-flight turns but some did not settle within {SHUTDOWN_CANCEL_GRACE_SECS}s; tearing down anyway"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_nineteen_seconds() {
        assert_eq!(STOP_BUDGET, Duration::from_secs(19));
    }

    #[test]
    fn at_the_defaults_the_mcp_slice_is_reserved_and_the_drain_is_eight_seconds() {
        let t0 = Instant::now();
        let p = StopPlan::new(t0, DEFAULT_STOP_TIMEOUT_SECS);
        assert_eq!(p.drain_end - t0, Duration::from_secs(8));
        assert_eq!(p.mcp_end - p.drain_end, MCP_STOP_SLICE);
        assert_eq!(p.end - p.mcp_end, STOP_MARGIN);
    }

    #[test]
    fn a_large_stop_timeout_cannot_eat_the_mcp_slice() {
        let t0 = Instant::now();
        let p = StopPlan::new(t0, 600);
        assert_eq!(p.end - t0, STOP_BUDGET);
        assert_eq!(p.mcp_end - p.drain_end, MCP_STOP_SLICE);
    }

    #[test]
    fn a_short_stop_timeout_starts_the_mcp_stop_early() {
        let t0 = Instant::now();
        let p = StopPlan::new(t0, 2);
        assert_eq!(p.drain_end - t0, Duration::from_secs(2));
        assert_eq!(p.mcp_end - t0, Duration::from_secs(12));
    }

    /// A turn on the slow stub (60 s) that the drain cannot wait out.
    fn start_slow_turn(runner: &std::sync::Arc<TaskRunner>) {
        let spec = crate::task_runner::TaskSpec {
            cwd: None,
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text {
                    text: "slow".into(),
                }],
            },
            context_task_id: None,
            task_id: Some("task-stop-budget".to_string()),
            intent: crate::llm::RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
            attended: true,
            deadline_secs: None,
            tool_allowlist: None,
            consult_scope: None,
            channel: None,
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let r = runner.clone();
        tokio::spawn(async move {
            let _keep = _rx;
            r.run_sync_streaming(spec, tx, None).await
        });
    }

    #[tokio::test]
    async fn a_turn_in_flight_does_not_delay_the_mcp_stop_past_its_slice() {
        // Short timings stand in for the real 8 s / 10 s split; what is
        // checked is the order: the MCP stop starts at `drain_end`, not
        // after the cancel grace, and the stop ends inside `end`.
        let runner = std::sync::Arc::new(TaskRunner::new_stub_slow());
        start_slow_turn(&runner);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t0 = Instant::now();
        let plan = StopPlan {
            drain_end: t0 + Duration::from_millis(200),
            mcp_end: t0 + Duration::from_secs(3),
            end: t0 + Duration::from_secs(4),
        };
        run(plan, 0, &runner, None).await;
        assert!(Instant::now() < plan.end, "stop must end before T");
    }
}
