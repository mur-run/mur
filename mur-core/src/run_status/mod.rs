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

/// A run's state as reported to any surface. `state` is read from disk;
/// `liveness` is computed here and nowhere else.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunStatus {
    pub state: State,
    pub liveness: Liveness,
    pub run: RunState,
}

/// Derive a run's reportable status. THE single derivation point (spec §4).
///
/// `now` and `stale_after` are parameters rather than ambient reads so the
/// table test can address every cell without sleeping.
pub fn classify(run: RunState, now: DateTime<Utc>, stale_after: chrono::Duration) -> RunStatus {
    let liveness = if run.state.is_terminal() {
        Liveness::NotApplicable
    } else if !mur_common::lock_file::pid_alive(run.pid) {
        Liveness::Dead
    } else {
        match run.last_heartbeat_at {
            // Rebuilt from the channel: the heartbeat is not recoverable and
            // must not be invented.
            None => Liveness::Unknown,
            Some(beat) if now.signed_duration_since(beat) <= stale_after => Liveness::Alive,
            Some(_) => Liveness::Stalled,
        }
    };
    RunStatus {
        state: run.state,
        liveness,
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
pub fn status_of(mur_home: &std::path::Path, run_id: &str) -> anyhow::Result<Option<RunStatus>> {
    let Some(record) = store::load(mur_home, run_id)? else {
        return Ok(None);
    };
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    Ok(Some(classify(record, Utc::now(), stale_after(&cfg.runs))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE_AFTER_SECS: i64 = 30;

    fn stale_after() -> chrono::Duration {
        chrono::Duration::seconds(STALE_AFTER_SECS)
    }

    fn run(
        state: State,
        pid: u32,
        heartbeat_age_secs: Option<i64>,
        now: DateTime<Utc>,
    ) -> RunState {
        RunState {
            schema: RUN_SCHEMA,
            run_id: "r".into(),
            channel_id: None,
            kind: RunKind::Job,
            label: "l".into(),
            pid,
            started_at: now - chrono::Duration::seconds(600),
            last_heartbeat_at: heartbeat_age_secs.map(|s| now - chrono::Duration::seconds(s)),
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: "0.0.0-test".into(),
            build_sha: "deadbee".into(),
        }
    }

    /// A pid that is certainly not running: spawn a trivial child, wait for it,
    /// and reuse its reaped pid. Checking a literal pid would be a guess.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let pid = child.id();
        child.wait().expect("reap child");
        pid
    }

    #[test]
    fn every_state_liveness_cell() {
        let now = Utc::now();
        let live = std::process::id();
        let dead = dead_pid();

        // Non-terminal + live process + fresh heartbeat => alive.
        for state in [State::Running, State::Blocked] {
            let s = classify(run(state, live, Some(1), now), now, stale_after());
            assert_eq!(s.state, state);
            assert_eq!(s.liveness, Liveness::Alive, "{state:?} with a fresh beat");
        }

        // Non-terminal + live process + expired heartbeat => stalled.
        for state in [State::Running, State::Blocked] {
            let s = classify(
                run(state, live, Some(STALE_AFTER_SECS + 1), now),
                now,
                stale_after(),
            );
            assert_eq!(s.liveness, Liveness::Stalled, "{state:?} with a dead beat");
        }

        // Non-terminal + no process => dead, whatever the heartbeat said.
        for state in [State::Running, State::Blocked] {
            let s = classify(run(state, dead, Some(1), now), now, stale_after());
            assert_eq!(s.liveness, Liveness::Dead, "{state:?} with no process");
        }

        // Non-terminal + live process + rebuilt (no heartbeat) => unknown.
        let s = classify(run(State::Running, live, None, now), now, stale_after());
        assert_eq!(s.liveness, Liveness::Unknown);

        // Terminal => n/a regardless of process or heartbeat.
        for state in [State::Done, State::Failed, State::Stopped] {
            for pid in [live, dead] {
                for beat in [Some(1), Some(STALE_AFTER_SECS + 1), None] {
                    let s = classify(run(state, pid, beat, now), now, stale_after());
                    assert_eq!(
                        s.liveness,
                        Liveness::NotApplicable,
                        "{state:?} must not report liveness"
                    );
                }
            }
        }
    }

    /// Negative control for the reported defect. A test that only asserts a
    /// live process reports `alive` proves nothing: freezing the heartbeat
    /// while the process stays up MUST flip the verdict.
    #[test]
    fn frozen_heartbeat_flips_running_to_stalled() {
        let now = Utc::now();
        let live = std::process::id();
        let fresh = classify(run(State::Running, live, Some(1), now), now, stale_after());
        let frozen = classify(
            run(State::Running, live, Some(STALE_AFTER_SECS + 1), now),
            now,
            stale_after(),
        );
        assert_eq!(fresh.liveness, Liveness::Alive);
        assert_eq!(frozen.liveness, Liveness::Stalled);
        assert_ne!(
            fresh.liveness, frozen.liveness,
            "heartbeat is not consulted"
        );
    }

    /// Negative control: a killed orchestrator must never keep reporting
    /// `running`/`alive`. `state` stays `running` because nothing wrote a
    /// terminal state — that pair IS what a crash looks like.
    #[test]
    fn killed_orchestrator_reports_dead_not_running() {
        let now = Utc::now();
        let s = classify(
            run(State::Running, dead_pid(), Some(1), now),
            now,
            stale_after(),
        );
        assert_eq!(
            s.state,
            State::Running,
            "no terminal state was ever written"
        );
        assert_eq!(s.liveness, Liveness::Dead);
        assert!(!s.state.is_terminal(), "a crashed run is not finished");
    }

    #[test]
    fn liveness_is_never_persisted() {
        let now = Utc::now();
        let json =
            serde_json::to_string(&run(State::Running, std::process::id(), Some(1), now)).unwrap();
        assert!(
            !json.contains("liveness"),
            "liveness must be derived, never stored: {json}"
        );
    }

    /// `status_of` must load `<mur_home>/config.yaml`, not just `mur_home`
    /// itself — `Config::load_or_default` takes a file path, and silently
    /// returns `Config::default()` on any read failure (including "this
    /// path is a directory"). A wrong path here does not error; it just
    /// makes every `runs:` setting a dead knob a user can change with no
    /// observable effect. Prove the config is actually read by giving it a
    /// `stale_after` far stricter than the default and checking the
    /// classification only that value can produce.
    #[test]
    fn status_of_reads_the_configured_stale_after_not_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();

        // Default stale_after is 10s * 3 = 30s (RunsConfig's defaults).
        // A 5s-old heartbeat reads `Alive` under that default and `Stalled`
        // under this 1s config — so the two paths cannot agree by accident.
        std::fs::write(
            mur_home.join("config.yaml"),
            "runs:\n  heartbeat_interval_secs: 1\n  heartbeat_stale_after_intervals: 1\n",
        )
        .unwrap();

        let now = Utc::now();
        let record = run(State::Running, std::process::id(), Some(5), now);
        store::save(mur_home, &record).unwrap();

        let status = status_of(mur_home, &record.run_id)
            .unwrap()
            .expect("run was just saved");
        assert_eq!(
            status.liveness,
            Liveness::Stalled,
            "status_of computed Alive, which is only reachable via the \
             default 30s stale_after — config.yaml at mur_home is not \
             being read, so the configured 1s stale_after never took effect"
        );
    }
}
