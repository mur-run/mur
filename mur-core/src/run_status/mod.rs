//! Run status: the one place a job / fleet / workflow run's state is derived.
//!
//! `~/.mur/runs/<run_id>/run.json` is a CACHE, not a source of truth — every
//! field except `last_heartbeat_at` is derivable from the run's channel event
//! log (see `rebuild`). When the two disagree, the channel wins and the
//! record is re-derived from it in memory — the cache file is never written
//! back (a write-back would have to decide what a re-derived heartbeat means,
//! which is deliberately left unanswered). This mirrors
//! `mur_common::channel::Channel`, whose own doc comment calls it "a cache of
//! state derivable from the event log".
//!
//! The events the executor writes carry the run's `run_id`, and the
//! `sidecar.json` rebuild index records the channel, kind, and first event
//! seq — so a rebuild folds only THIS run's events even on a long-lived
//! shared channel. Known limitation: if the whole run directory
//! (`runs/<run_id>/`) is deleted, the run is still unrecoverable by
//! `mur job *` even though its channel still exists — the sidecar that
//! indexes the channel lives inside that directory, and without it there is
//! no way to know which channel to fold.

pub mod abandon;
pub mod heartbeat;
pub mod rebuild;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version of `run.json`. Bump when a field's meaning changes.
pub const RUN_SCHEMA: u32 = 1;

/// Schema version of `sidecar.json`. Bump when a field's meaning changes.
pub const SIDECAR_SCHEMA: u32 = 1;

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

/// The rebuild index for one run, stored beside `run.json` as
/// `sidecar.json` and deliberately separate from it so a corrupt cache
/// cannot take the index down with it.
///
/// Every field is a FACT the executor knows at recording time — the channel
/// the run executed over, the run kind the caller passed in, and the channel
/// event sequence number at which this run's first event lands. Nothing in
/// it is inferred: inference is how one run's terminal state gets attributed
/// to another (see `rebuild`'s run-boundary rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    pub schema: u32,
    pub channel_id: String,
    pub kind: RunKind,
    pub first_seq: u64,
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
    /// Why a terminal step ended the way it did. Set on failure; a `Done`
    /// step has nothing to explain, and a retry clears the prior attempt's.
    ///
    /// Without it the record answers "what" and never "why": six failed
    /// steps once carried six bare `failed`s, and the only way to the reason
    /// was grepping agent stderr in /tmp (fleet develop-rust, 2026-09-09).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

/// A run's state as reported to any surface. `state` is read from disk;
/// `liveness` is computed here and nowhere else.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunStatus {
    pub state: State,
    pub liveness: Liveness,
    /// The process died without recording a terminal state, and its last
    /// heartbeat is past `abandon::grace` — nothing will ever record one.
    /// Derived here (never stored) so every surface reports the same verdict
    /// for the same run; `state` keeps saying what the record says.
    pub abandoned: bool,
    pub run: RunState,
}

/// Derive a run's reportable status. THE single derivation point (spec §4).
///
/// `now` and `stale_after` are parameters rather than ambient reads so the
/// table test can address every cell without sleeping.
pub fn classify(run: RunState, now: DateTime<Utc>, stale_after: chrono::Duration) -> RunStatus {
    // The arm order for non-terminal runs is load-bearing (spec §3):
    // absent heartbeat → `unknown` comes BEFORE the pid check. A rebuilt
    // record carries pid 0, and pid-0 liveness is platform-dependent
    // (`kill(0, …)` targets the caller's own process group on Unix, so it
    // reads *alive*; `OpenProcess(0, …)` fails on Windows, so it reads
    // *dead*). Checking the absent heartbeat first makes `unknown` the
    // answer on every platform.
    let liveness = if run.state.is_terminal() {
        Liveness::NotApplicable
    } else {
        match run.last_heartbeat_at {
            // Rebuilt from the channel: the heartbeat is not recoverable and
            // must not be invented.
            None => Liveness::Unknown,
            Some(_) if !mur_common::lock_file::pid_alive(run.pid) => Liveness::Dead,
            Some(beat) if now.signed_duration_since(beat) <= stale_after => Liveness::Alive,
            Some(_) => Liveness::Stalled,
        }
    };
    let abandoned = abandon::is_abandoned(
        liveness,
        run.last_heartbeat_at,
        now,
        abandon::grace(stale_after),
    );
    RunStatus {
        state: run.state,
        liveness,
        abandoned,
        run,
    }
}

/// The heartbeat age past which a live process counts as `stalled`.
///
/// Derived here, once, so no surface recomputes `interval × intervals` and
/// drifts from the others — the same class of bug as two renderers disagreeing
/// about one fact.
pub fn stale_after(cfg: &mur_common::config::RunsConfig) -> chrono::Duration {
    chrono::Duration::seconds(
        (cfg.heartbeat_interval_secs * u64::from(cfg.heartbeat_stale_after_intervals)) as i64,
    )
}

/// Load a run and classify it against the configured staleness threshold and
/// the current clock. `Ok(None)` when no such run was recorded.
///
/// Every surface calls THIS, not `classify` directly: it is the only place the
/// config load, the clock read, and the derivation are assembled, so no caller
/// can assemble them differently. `classify` stays pure so the table test can
/// address every cell without a clock or a config file.
/// A run id a caller may mint (`mur fleet run --run-id`): one path segment
/// under `~/.mur/runs/`, so letters, digits, `-` and `_` only.
pub fn valid_run_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn status_of(mur_home: &std::path::Path, run_id: &str) -> anyhow::Result<Option<RunStatus>> {
    let loaded = store::load(mur_home, run_id);
    let record = match loaded {
        Ok(Some(record)) => Some(record),
        Ok(None) => rebuild_for(mur_home, run_id)?,
        // Rebuild first; re-propagate the cache error only when there is no
        // rebuild candidate — a run that has a channel to rebuild from must
        // not be hidden by a parse failure. A genuine rebuild I/O error is
        // also reported (chained onto the cache error), never collapsed into
        // "no run recorded".
        Err(e) => match rebuild_for(mur_home, run_id) {
            Ok(Some(record)) => Some(record),
            Ok(None) => return Err(e),
            Err(rebuild_err) => {
                return Err(e.context(format!(
                    "rebuild from the channel also failed: {rebuild_err:#}"
                )));
            }
        },
    };
    let Some(mut record) = record else {
        return Ok(None);
    };
    // Reconciliation (spec §2): the channel wins even when the cache
    // parses. A parseable cache is accepted for what it says only while the
    // channel has nothing newer to say — this closes the sequence "channel
    // Completed succeeds, terminal run.json write fails, cache says running
    // forever", which otherwise reports `running` + `dead` after exit. Only
    // a non-terminal cache is consulted, and only a terminal channel state
    // overrides it; the cache's heartbeat is retained (it is still real).
    //
    // A failed read is warned, never silent: the write path already warns
    // when a sidecar cannot be recorded, and the read path must match — a
    // corrupt sidecar silently disabling reconciliation is exactly the kind
    // of observability gap an operator cannot see.
    if !record.state.is_terminal() {
        match store::load_sidecar(mur_home, run_id) {
            Ok(Some(sidecar)) => match rebuild::run_tail_state(mur_home, &sidecar, run_id) {
                Ok(Some(channel_state))
                    if channel_state.is_terminal() && record.state != channel_state =>
                {
                    record.state = channel_state;
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    run_id,
                    %error,
                    "reconcile: reading the channel's tail state failed; \
                     reconciliation skipped"
                ),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(
                run_id,
                %error,
                "reconcile: reading sidecar.json failed; reconciliation skipped"
            ),
        }
    }
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    Ok(Some(classify(record, Utc::now(), stale_after(&cfg.runs))))
}

/// Re-derive the record from the channel via the `sidecar.json` index, in
/// memory only — nothing is written back to the cache. `Ok(None)` when the
/// sidecar is absent (a run that was never recorded, or whose whole directory
/// was deleted — the documented limitation) or the channel no longer exists.
/// A genuine I/O fault PROPAGATES — from the sidecar read as much as from the
/// channel read: `status_of` must report it instead of pretending the run
/// never existed. `load_sidecar` already answers `Ok(None)` for an absent
/// sidecar, so `?` here costs the absent case nothing.
fn rebuild_for(mur_home: &std::path::Path, run_id: &str) -> anyhow::Result<Option<RunState>> {
    let Some(sidecar) = store::load_sidecar(mur_home, run_id)? else {
        return Ok(None);
    };
    rebuild::from_channel(mur_home, run_id, &sidecar)
}

#[cfg(test)]
mod tests;
