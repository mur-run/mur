//! #2036: the launching scope's kill-switch, honoured inside a DAG run.
//!
//! The fleet loop used to check `mur fleet stop` only between iterations, and
//! the one-shot run only before it started. A run already inside
//! `execute_dag` ran every step to completion — billed — and could even end
//! `done`. The executor knows nothing about fleets, so the caller hands it a
//! [`StopSignal`]; the executor polls it at every step boundary and, while a
//! delegated member turn is in flight, every [`STOP_POLL_INTERVAL`]. A trip
//! mid-turn sends `tasks/cancel` for the member's task, as the review
//! watchdog does (#1758).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::DagExecOptions;

/// "Has the operator asked this run to stop?" MUST be cheap: it is polled
/// once a second for every in-flight delegate.
pub type StopSignal = Arc<dyn Fn() -> bool + Send + Sync>;

/// How often an in-flight delegate re-reads the stop signal. Matches the
/// review watchdog's cadence, so a stop takes effect within about a second
/// on both paths.
pub(crate) const STOP_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What a stopped step says about itself, in the step event and its output.
pub(crate) const STOPPED_REASON: &str = "stopped: the kill-switch was engaged (`mur fleet stop`)";

/// What a step halted by a pause says about itself.
pub const PAUSED_REASON: &str =
    "paused: `mur fleet pause` halted this run at a safe point; it resumes from here";

/// Why a run was halted from outside. `Stop` is the kill-switch and ends the
/// run for good; `Pause` halts it the same way but leaves it resumable, so the
/// executor must close the channel differently (pause/resume design §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReason {
    Stop,
    Pause,
}

/// "Should this run halt, and why?" Same cost contract as [`StopSignal`]: it
/// is polled once a second per in-flight delegate. Stop MUST be reported ahead
/// of pause when both are set.
pub type HaltSignal = Arc<dyn Fn() -> Option<HaltReason> + Send + Sync>;

/// Why the run should halt right now, if it should. The plain `stop` signal
/// is always a [`HaltReason::Stop`] and is read first, so a stop outranks a
/// pause whichever signal carried it.
pub(super) fn halt_reason(opts: &DagExecOptions<'_>) -> Option<HaltReason> {
    if opts.stop.as_ref().is_some_and(|s| s()) {
        return Some(HaltReason::Stop);
    }
    opts.halt.as_ref().and_then(|h| h())
}

/// True when either signal says the run should halt.
pub(super) fn stop_requested(opts: &DagExecOptions<'_>) -> bool {
    halt_reason(opts).is_some()
}

/// What a halted step reports, matched to why it halted.
pub(super) fn halted_reason_text(opts: &DagExecOptions<'_>) -> &'static str {
    match halt_reason(opts) {
        Some(HaltReason::Pause) => PAUSED_REASON,
        _ => STOPPED_REASON,
    }
}

/// Both signals folded into the one boolean an in-flight delegate polls.
pub(super) fn watch_signal(opts: &DagExecOptions<'_>) -> Option<StopSignal> {
    if opts.stop.is_none() && opts.halt.is_none() {
        return None;
    }
    let (stop, halt) = (opts.stop.clone(), opts.halt.clone());
    Some(Arc::new(move || {
        stop.as_ref().is_some_and(|s| s()) || halt.as_ref().is_some_and(|h| h().is_some())
    }))
}

/// How a watched delegate dial ended.
pub(super) enum DelegateDial {
    /// The member answered (or the dial failed) before any stop.
    Replied(Result<Value>),
    /// The stop signal tripped first; `tasks/cancel` has been sent.
    Stopped,
}

/// Dial `channel/delegate` on a blocking thread, polling `stop` while it is
/// in flight. On a trip, ask the member to cancel `task_id` and return at
/// once — the abandoned dial ends on its own when the member answers the
/// cancel (or its io timeout fires).
///
/// The dial always runs on the blocking pool, stop signal or not: it is a
/// synchronous socket read that can last many minutes, and it used to sit on
/// an async worker thread for all of them.
pub(super) async fn dial_delegate_watched(
    mur_home: &Path,
    member: &str,
    task_id: &str,
    params: Value,
    stop: Option<&StopSignal>,
) -> DelegateDial {
    let home = mur_home.to_path_buf();
    let target = member.to_string();
    let mut dial = tokio::task::spawn_blocking(move || {
        crate::a2a_dial::dial_method(
            &home,
            &target,
            "channel/delegate",
            params,
            crate::a2a_dial::DialMode::RequireRunning,
        )
    });
    let joined = |r: std::result::Result<Result<Value>, tokio::task::JoinError>| {
        r.unwrap_or_else(|e| Err(anyhow!("delegate dial task failed: {e}")))
    };
    let Some(stop) = stop else {
        return DelegateDial::Replied(joined(dial.await));
    };
    let mut tick = tokio::time::interval(STOP_POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            r = &mut dial => return DelegateDial::Replied(joined(r)),
            _ = tick.tick() => {
                if stop() {
                    cancel_member_task(mur_home, member, task_id).await;
                    return DelegateDial::Stopped;
                }
            }
        }
    }
}

/// Ask the member's runtime to stop working (and spending) on `task_id`.
/// The runtime registers a `run_sync` turn under the caller's `ct-…` id, so
/// the id we minted for the delegation is the one it cancels by. A failure is
/// logged, not raised: the run is stopping either way.
async fn cancel_member_task(mur_home: &Path, member: &str, task_id: &str) {
    let home = mur_home.to_path_buf();
    let (m, id) = (member.to_string(), task_id.to_string());
    let res = tokio::task::spawn_blocking(move || {
        crate::a2a_dial::dial_method(
            &home,
            &m,
            "tasks/cancel",
            serde_json::json!({ "id": id }),
            crate::a2a_dial::DialMode::RequireRunning,
        )
    })
    .await;
    match res {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(member, task_id, %error, "kill-switch: tasks/cancel failed");
        }
        Err(error) => {
            tracing::warn!(member, task_id, %error, "kill-switch: tasks/cancel task failed");
        }
    }
}
