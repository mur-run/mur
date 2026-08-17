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
    let loaded = store::load(mur_home, run_id);
    let record = match loaded {
        Ok(Some(record)) => Some(record),
        Ok(None) => rebuild_for(mur_home, run_id),
        // Rebuild first; re-propagate the cache error only when there is no
        // rebuild candidate — a run that has a channel to rebuild from must
        // not be hidden by a parse failure. The `?` is deliberately on the
        // `.ok_or(e).map(Some)` result, NOT `.or(loaded?)`: `loaded?` would
        // be evaluated EAGERLY as `.or`'s argument, and its `return` would
        // propagate the error before `.or` ever saw the rebuild candidate.
        Err(e) => rebuild_for(mur_home, run_id).ok_or(e).map(Some)?,
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
    if !record.state.is_terminal()
        && let Ok(Some(sidecar)) = store::load_sidecar(mur_home, run_id)
        && let Ok(Some(channel_state)) = rebuild::run_tail_state(mur_home, &sidecar, run_id)
        && channel_state.is_terminal()
        && record.state != channel_state
    {
        record.state = channel_state;
    }
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    Ok(Some(classify(record, Utc::now(), stale_after(&cfg.runs))))
}

/// Re-derive the record from the channel via the `sidecar.json` index, in
/// memory only — nothing is written back to the cache. `None` when the
/// sidecar is absent (a run that was never recorded, or whose whole directory
/// was deleted — the documented limitation) or the channel no longer exists.
fn rebuild_for(mur_home: &std::path::Path, run_id: &str) -> Option<RunState> {
    let sidecar = store::load_sidecar(mur_home, run_id).ok().flatten()?;
    rebuild::from_channel(mur_home, run_id, &sidecar)
        .ok()
        .flatten()
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
    ///
    /// NEVER call external utilities that may not exist on the target
    /// platform: `true` does not ship with Windows (a CI workspace gate
    /// without true.exe would panic before testing classification), so the
    /// helper is cfg'd — `true` on Unix, the always-present `cmd` on
    /// Windows.
    fn dead_pid() -> u32 {
        #[cfg(unix)]
        {
            let mut child = std::process::Command::new("true")
                .spawn()
                .expect("spawn `true`");
            let pid = child.id();
            child.wait().expect("reap child");
            pid
        }
        #[cfg(windows)]
        {
            // `cmd /C exit 1` returns immediately and the child is
            // definitely dead by the time its pid is reused. `cmd` is
            // guaranteed to exist on every Windows install.
            let mut child = std::process::Command::new("cmd")
                .args(["/C", "exit 1"])
                .spawn()
                .expect("spawn cmd");
            let pid = child.id();
            child.wait().expect("reap child");
            pid
        }
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

        // Non-terminal + dead process + rebuilt (no heartbeat) => unknown,
        // NOT dead: the absent-heartbeat check precedes the pid check, so a
        // pid whose liveness is platform-dependent (0 on Windows reads
        // dead via a failing OpenProcess) can never pick the verdict.
        let s = classify(run(State::Running, dead, None, now), now, stale_after());
        assert_eq!(
            s.liveness,
            Liveness::Unknown,
            "absent heartbeat must win over a dead pid"
        );

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

    /// `status_of` must not pretend a run never existed when the channel it
    /// could rebuild from is genuinely unreadable: with run.json missing and
    /// the channel read faulting, the error must surface — not Ok(None).
    #[test]
    fn status_of_reports_a_genuine_channel_fault_instead_of_none() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("faulty-channel").unwrap();
        let dir = store::runs_dir(mur_home).join("run-x");
        std::fs::create_dir_all(&dir).unwrap();
        store::save_sidecar(
            mur_home,
            "run-x",
            &Sidecar {
                schema: SIDECAR_SCHEMA,
                channel_id: ch.id.clone(),
                kind: RunKind::Job,
                first_seq: 0,
            },
        )
        .unwrap();
        // No run.json -> the rebuild path is the only route; sabotage it.
        let chan_dir = mur_home.join("channels").join(&ch.id);
        std::fs::remove_dir_all(&chan_dir).unwrap();
        std::fs::write(&chan_dir, b"i am a file").unwrap();

        let err = status_of(mur_home, "run-x")
            .expect_err("a genuine channel read fault must surface as an error, not Ok(None)");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&ch.id),
            "the error must name the channel: {msg}"
        );
    }

    /// A corrupt run.json must not take the run down with it: the
    /// sidecar.json index survives the cache, and status_of must fall back to
    /// a rebuilt record that honestly reports an unknown heartbeat.
    #[test]
    fn status_of_rebuilds_from_the_channel_when_the_cache_is_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();

        // A channel with a finished (failed) run's worth of events, written
        // the way the executor writes them — run_id stamped on each payload.
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("corrupt-cache").unwrap();
        svc.append_delegation(&ch.id, "pm", "child-1", None, Some("run-c"))
            .unwrap();
        svc.transition(
            &ch.id,
            mur_common::channel::ChannelState::Failed,
            mur_common::channel::ChannelActor::System,
            Some("run-c"),
        )
        .unwrap();

        // The run directory with the sidecar but a GARBLED run.json.
        let dir = store::runs_dir(mur_home).join("run-c");
        std::fs::create_dir_all(&dir).unwrap();
        store::save_sidecar(
            mur_home,
            "run-c",
            &Sidecar {
                schema: SIDECAR_SCHEMA,
                channel_id: ch.id.clone(),
                kind: RunKind::Workflow,
                first_seq: 0,
            },
        )
        .unwrap();
        std::fs::write(dir.join("run.json"), b"{ this is not json").unwrap();

        let status = status_of(mur_home, "run-c")
            .unwrap()
            .expect("a corrupt cache must fall back to the channel, not return None");
        assert_eq!(
            status.state,
            State::Failed,
            "rebuilt state must come from the channel"
        );
        assert_eq!(
            status.liveness,
            Liveness::NotApplicable,
            "a finished rebuilt run reports no liveness"
        );
        assert!(
            status.run.last_heartbeat_at.is_none(),
            "rebuilt heartbeat must be unknown"
        );
    }

    /// THE regression for the review's parseable-cache finding: the channel's
    /// Completed transition succeeded but the terminal run.json write failed,
    /// so the cache still says `running` with a fresh heartbeat and a live
    /// pid. `status_of` must report the channel's `done` — not `running` +
    /// whatever the pid says — and the heartbeat it reports must be the
    /// cache's real one, not a fabricated value.
    #[test]
    fn status_of_reconciles_a_parseable_running_cache_with_the_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();

        // The channel's authoritative tail: this run completed.
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("reconcile").unwrap();
        svc.append_delegation(&ch.id, "pm", "child-1", None, Some("run-r"))
            .unwrap();
        svc.transition(
            &ch.id,
            mur_common::channel::ChannelState::Completed,
            mur_common::channel::ChannelActor::System,
            Some("run-r"),
        )
        .unwrap();

        // The cache: still running, with a real (fresh) heartbeat and live
        // pid — the exact shape of "channel Completed succeeded, terminal
        // run.json write failed".
        let now = Utc::now();
        let mut record = run(State::Running, std::process::id(), Some(1), now);
        record.run_id = "run-r".into();
        record.channel_id = Some(ch.id.clone());
        let cached_heartbeat = record.last_heartbeat_at;
        store::save(mur_home, &record).unwrap();
        store::save_sidecar(
            mur_home,
            "run-r",
            &Sidecar {
                schema: SIDECAR_SCHEMA,
                channel_id: ch.id.clone(),
                kind: RunKind::Workflow,
                first_seq: 0,
            },
        )
        .unwrap();

        let status = status_of(mur_home, "run-r")
            .unwrap()
            .expect("the run was just recorded");
        assert_eq!(
            status.state,
            State::Done,
            "the channel wins over a parseable cache that still says running"
        );
        assert_eq!(
            status.liveness,
            Liveness::NotApplicable,
            "a finished run reports no liveness"
        );
        assert_eq!(
            status.run.last_heartbeat_at, cached_heartbeat,
            "the cache's real heartbeat is retained, never fabricated"
        );
    }

    /// Reconciliation must be bounded to the run: another run's terminal
    /// state on the same channel (different run_id) must not override this
    /// run's still-running cache.
    #[test]
    fn status_of_reconciliation_ignores_other_runs_on_the_same_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();

        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("reconcile-shared").unwrap();
        // Run B completed on the shared channel; run R is still running.
        svc.append_delegation(&ch.id, "pm", "child-b", None, Some("run-b"))
            .unwrap();
        svc.transition(
            &ch.id,
            mur_common::channel::ChannelState::Completed,
            mur_common::channel::ChannelActor::System,
            Some("run-b"),
        )
        .unwrap();

        let now = Utc::now();
        let mut record = run(State::Running, std::process::id(), Some(1), now);
        record.run_id = "run-r".into();
        store::save(mur_home, &record).unwrap();
        store::save_sidecar(
            mur_home,
            "run-r",
            &Sidecar {
                schema: SIDECAR_SCHEMA,
                channel_id: ch.id.clone(),
                kind: RunKind::Workflow,
                first_seq: 0,
            },
        )
        .unwrap();

        let status = status_of(mur_home, "run-r").unwrap().expect("recorded run");
        assert_eq!(
            status.state,
            State::Running,
            "B's completion on the same channel must not end R"
        );
        assert_eq!(
            status.liveness,
            Liveness::Alive,
            "R is live and healthy; B's state must not leak into its liveness"
        );
    }
}
