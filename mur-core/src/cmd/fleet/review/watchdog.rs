//! #1758: preempt a member turn that is still running.
//!
//! The loop checks its limits only between sends, so without this a single
//! turn that keeps its A2A stream alive (a long tool, a model that keeps
//! streaming heartbeats) is unbounded. A [`TurnWatch`] travels with the
//! send; the transport polls it from inside the streaming dial and, when it
//! trips, cancels the member's task and fails the send with [`Preempted`].

use std::path::Path;
use std::time::Duration;

use mur_common::limits::Stuck;

use crate::a2a_dial::{DialMode, DialPoll, PollState, dial_method};
use crate::cmd::fleet::control;

use super::constants::WATCHDOG_POLL_INTERVAL;

/// Which limit preempted the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidTurn {
    Deadline,
    Stuck,
    Stopped,
}

/// The limits one in-flight turn is held to. Built by the loop right before
/// the send, from what is left of the session's budget.
#[derive(Debug, Clone, Copy)]
pub struct TurnWatch<'a> {
    pub mur_home: &'a Path,
    pub fleet_name: &'a str,
    /// Execution time the session has left when the turn starts.
    pub deadline_left: Duration,
    pub stuck: Stuck,
}

impl TurnWatch<'_> {
    /// The limit `state` has crossed, if any. The kill-switch wins: an
    /// operator who ran `mur fleet stop` should see "stopped", whatever
    /// else also expired.
    pub fn tripped(&self, state: &PollState<'_>) -> Option<MidTurn> {
        if control::is_stopped(self.mur_home, self.fleet_name) {
            return Some(MidTurn::Stopped);
        }
        if state.elapsed >= self.deadline_left {
            return Some(MidTurn::Deadline);
        }
        if let Stuck::After(window) = self.stuck
            && state.idle >= window
        {
            return Some(MidTurn::Stuck);
        }
        None
    }

    /// Run `dial` with this watch polled every [`WATCHDOG_POLL_INTERVAL`].
    /// On a trip, best-effort `tasks/cancel` for the member's task, then
    /// fail with [`Preempted`].
    pub fn guard<T>(
        &self,
        member: &str,
        dial: impl FnOnce(DialPoll<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut check = |state: &PollState<'_>| {
            let why = self.tripped(state)?;
            if let Some(task_id) = state.task_id {
                cancel(self.mur_home, member, task_id);
            }
            Some(anyhow::Error::new(Preempted {
                member: member.to_string(),
                why,
            }))
        };
        dial(DialPoll {
            every: WATCHDOG_POLL_INTERVAL,
            check: &mut check,
        })
    }
}

/// A send the watchdog cut short. Not a transport fault: never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preempted {
    pub member: String,
    pub why: MidTurn,
}

impl std::fmt::Display for Preempted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}'s turn preempted: {:?}", self.member, self.why)
    }
}

impl std::error::Error for Preempted {}

/// Ask the runtime to stop working (and spending) on `task_id`. A failure
/// is logged, not raised: the session is stopping either way.
fn cancel(mur_home: &Path, member: &str, task_id: &str) {
    if let Err(e) = dial_method(
        mur_home,
        member,
        "tasks/cancel",
        serde_json::json!({ "id": task_id }),
        DialMode::RequireRunning,
    ) {
        tracing::warn!(member, task_id, error = %e, "review: tasks/cancel after preemption failed");
    }
}

#[cfg(test)]
#[path = "watchdog_tests.rs"]
mod watchdog_tests;
