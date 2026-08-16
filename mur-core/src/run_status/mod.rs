//! Run status: the one place a job / fleet / workflow run's state is derived.
//!
//! `~/.mur/runs/<run_id>/run.json` is a CACHE, not a source of truth — every
//! field except `last_heartbeat_at` is derivable from the run's channel event
//! log (see `rebuild`). When the two disagree, the channel wins and the cache
//! is rebuilt. This mirrors `mur_common::channel::Channel`, whose own doc
//! comment calls it "a cache of state derivable from the event log".

pub mod heartbeat;
pub mod rebuild;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version of `run.json`. Bump when a field's meaning changes.
pub const RUN_SCHEMA: u32 = 1;

/// Which entry point produced this run. All three go through `execute_dag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    Job,
    Fleet,
    Workflow,
}

/// The semantic state. STORED — written by the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Running,
    Blocked,
    Done,
    Failed,
    Stopped,
}

impl State {
    /// True when the run has finished and no process is expected to remain.
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Stopped)
    }
}

/// Whether the run is actually progressing. DERIVED — never stored.
///
/// Persisting this would recreate the lying-cache failure this module exists
/// to remove: a stale `running` on disk is exactly what made a dead
/// delegation look healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Liveness {
    /// Process up, heartbeat fresh.
    Alive,
    /// Process up, heartbeat expired — the run is not moving. This is the
    /// state that previously had no name and cost a long manual investigation.
    Stalled,
    /// Process gone. Paired with a non-terminal `State`, this is a crash.
    Dead,
    /// Process up, but the record was rebuilt from the channel and carries no
    /// heartbeat. Reporting this is required; synthesizing one is forbidden.
    Unknown,
    /// The run finished. A finished run's absent process is not a fault.
    #[serde(rename = "n/a")]
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Set while a run waits on a human decision. Plan B populates this; Plan A
/// only carries and renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedOn {
    pub hitl_id: String,
    pub summary: String,
    pub since: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub schema: u32,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    pub kind: RunKind,
    pub label: String,
    /// PID of the orchestrator process (the one inside `execute_dag`), not of
    /// any delegated agent.
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    /// The ONLY field that cannot be rebuilt from the channel. `None` means
    /// "rebuilt" and yields `Liveness::Unknown`, never a guess.
    #[serde(default)]
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub state: State,
    #[serde(default)]
    pub steps: Vec<StepState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<BlockedOn>,
    pub binary_version: String,
    pub build_sha: String,
}
