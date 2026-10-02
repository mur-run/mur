//! `source.type: mur_run` — reads the unified run status by `run_id`
//! (spec §MVP Adapter → MUR run). Query only: a monitor NEVER re-dispatches
//! the run; a tool that stopped waiting is not evidence the run stopped.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::spec::SourceType;
use mur_monitor::state::Outcome;

use crate::run_status::{self, Liveness, RunStatus, State};

pub struct MurRunAdapter {
    mur_home: PathBuf,
}

impl MurRunAdapter {
    pub fn new(mur_home: &Path) -> Self {
        Self {
            mur_home: mur_home.to_path_buf(),
        }
    }
}

/// The heartbeat age past which a dead, never-settled run is `abandoned`.
/// Scales with `run_status::stale_after`, so a user who slows the heartbeat
/// (`runs:` in config.yaml) also widens this window — they cannot drift.
pub fn abandon_grace(cfg: &mur_common::config::RunsConfig) -> chrono::Duration {
    run_status::abandon::grace(run_status::stale_after(cfg))
}

/// Pure mapping from a classified run to an observation. `now` and
/// `abandon_grace` are parameters (not ambient reads) so the tests address
/// both sides of the window without a clock or a config file.
pub fn map(s: RunStatus, now: DateTime<Utc>, abandon_grace: chrono::Duration) -> Observation {
    match s.state {
        State::Done => Observation::terminal(Outcome::Succeeded, "run state: done"),
        State::Failed => Observation::terminal(Outcome::Failed, "run state: failed"),
        State::Stopped => Observation::terminal(Outcome::Cancelled, "run state: stopped"),
        State::Running | State::Blocked => match s.liveness {
            // The process is gone and nothing wrote a terminal state: we do
            // not know what happened, and saying `failed` would be a guess.
            // Within the grace window that stays `unknown` (a result may yet
            // reconcile from the channel); past it, nothing will ever write
            // one, so the run settles as `abandoned` — terminal, so the
            // monitor stops instead of polling a corpse forever (#1622), yet
            // still not `failed`. `Liveness::Dead` always carries a beat
            // (`classify` answers `Unknown` when there is none).
            Liveness::Dead => match s.run.last_heartbeat_at {
                Some(beat)
                    if run_status::abandon::is_abandoned(
                        s.liveness,
                        Some(beat),
                        now,
                        abandon_grace,
                    ) =>
                {
                    Observation::terminal(
                        Outcome::Abandoned,
                        format!(
                            "abandoned: process is dead and no terminal state was recorded; \
                             last heartbeat {} is older than the {}s grace",
                            beat.to_rfc3339(),
                            abandon_grace.num_seconds()
                        ),
                    )
                }
                _ => Observation::unknown("process is dead but no terminal state was recorded"),
            },
            _ => {
                let beat = s
                    .run
                    .last_heartbeat_at
                    .map(|b| b.to_rfc3339())
                    .unwrap_or_else(|| "-".into());
                Observation::pending(
                    format!("{:?}:{beat}:{}", s.state, s.run.steps.len()),
                    format!(
                        "run state: {:?}, liveness: {:?}, steps: {}",
                        s.state,
                        s.liveness,
                        s.run.steps.len()
                    ),
                )
            }
        },
    }
}

impl SourceAdapter for MurRunAdapter {
    fn source_type(&self) -> SourceType {
        SourceType::MurRun
    }

    fn validate_reference(&self, reference: &str) -> Result<(), String> {
        if run_status::valid_run_id(reference) {
            Ok(())
        } else {
            Err("run id: letters, digits, `-` and `_` only, at most 96 chars".into())
        }
    }

    fn observe(&self, reference: &str, _credential_ref: Option<&str>) -> Observation {
        match run_status::status_of(&self.mur_home, reference) {
            Ok(Some(s)) => {
                let cfg =
                    mur_common::config::Config::load_or_default(&self.mur_home.join("config.yaml"));
                map(s, Utc::now(), abandon_grace(&cfg.runs))
            }
            Ok(None) => {
                Observation::unknown("no run record: not started yet, or recorded on another host")
            }
            Err(e) => Observation::unknown(format!("run record unreadable: {e:#}")),
        }
        .redacted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{Liveness, RunKind, RunState, RunStatus, State};
    use chrono::TimeZone;

    fn run(state: State, beat: Option<chrono::DateTime<Utc>>) -> RunState {
        RunState {
            schema: 1,
            run_id: "run-1".into(),
            channel_id: None,
            kind: RunKind::Fleet,
            label: "deep-research".into(),
            pid: 0,
            started_at: Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap(),
            last_heartbeat_at: beat,
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: String::new(),
            build_sha: String::new(),
        }
    }
    fn status(state: State, liveness: Liveness, beat: Option<chrono::DateTime<Utc>>) -> RunStatus {
        RunStatus {
            state,
            liveness,
            abandoned: false,
            run: run(state, beat),
        }
    }

    fn grace() -> chrono::Duration {
        chrono::Duration::minutes(15)
    }
    fn at(s: RunStatus) -> Observation {
        map(s, Utc::now(), grace())
    }

    #[test]
    fn terminal_states_map_to_terminal_outcomes() {
        assert_eq!(
            at(status(State::Done, Liveness::NotApplicable, None)).outcome,
            Outcome::Succeeded
        );
        assert_eq!(
            at(status(State::Failed, Liveness::NotApplicable, None)).outcome,
            Outcome::Failed
        );
        assert_eq!(
            at(status(State::Stopped, Liveness::NotApplicable, None)).outcome,
            Outcome::Cancelled
        );
    }

    #[test]
    fn running_is_pending_with_the_heartbeat_as_progress() {
        let b1 = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 10).unwrap();
        let b2 = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 20).unwrap();
        let o1 = at(status(State::Running, Liveness::Alive, Some(b1)));
        let o2 = at(status(State::Running, Liveness::Alive, Some(b2)));
        assert_eq!(o1.outcome, Outcome::Pending);
        assert_ne!(
            o1.progress_token, o2.progress_token,
            "a new heartbeat is progress"
        );
        assert_eq!(
            o1.progress_token,
            at(status(State::Running, Liveness::Stalled, Some(b1))).progress_token,
            "stalled is the deadline evaluator's call, not the adapter's"
        );
        assert_eq!(
            at(status(State::Blocked, Liveness::Alive, Some(b1))).outcome,
            Outcome::Pending
        );
    }

    #[test]
    fn dead_process_without_terminal_record_is_unknown_not_failed() {
        let now = Utc::now();
        let o = map(
            status(State::Running, Liveness::Dead, Some(now)),
            now,
            grace(),
        );
        assert_eq!(o.outcome, Outcome::Unknown);
        assert!(o.adapter_error.is_some());
        assert_eq!(
            map(
                status(State::Running, Liveness::Unknown, None),
                now,
                grace()
            )
            .outcome,
            Outcome::Pending,
            "a rebuilt record with no heartbeat is still running as far as we know"
        );
    }

    /// #1622: a dead process within the grace window may still be about to
    /// have its terminal state reconciled from the channel — stay unknown.
    #[test]
    fn dead_process_within_grace_stays_unknown() {
        let beat = Utc.with_ymd_and_hms(2026, 9, 30, 8, 42, 48).unwrap();
        let now = beat + grace();
        for state in [State::Running, State::Blocked] {
            let o = map(status(state, Liveness::Dead, Some(beat)), now, grace());
            assert_eq!(
                o.outcome,
                Outcome::Unknown,
                "{state:?} at exactly the grace"
            );
        }
    }

    /// #1622: past the grace window the run will never record a result.
    /// Settle it as `abandoned` — terminal, so the monitor completes — and
    /// never as `failed`, which would be a guess about what happened.
    #[test]
    fn dead_process_past_grace_settles_as_abandoned_not_failed() {
        let beat = Utc.with_ymd_and_hms(2026, 9, 30, 8, 42, 48).unwrap();
        let now = beat + grace() + chrono::Duration::seconds(1);
        for state in [State::Running, State::Blocked] {
            let o = map(status(state, Liveness::Dead, Some(beat)), now, grace());
            assert_eq!(o.outcome, Outcome::Abandoned, "{state:?}");
            assert!(o.outcome.is_terminal());
            assert!(o.evidence.contains("abandoned"), "{}", o.evidence);
        }
    }

    /// A live process with an expired heartbeat is the deadline evaluator's
    /// call (stalled), never an adapter-side settlement — however old.
    #[test]
    fn stalled_or_unknown_liveness_never_becomes_abandoned() {
        let beat = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let now = beat + chrono::Duration::days(30);
        assert_eq!(
            map(
                status(State::Running, Liveness::Stalled, Some(beat)),
                now,
                grace()
            )
            .outcome,
            Outcome::Pending
        );
        assert_eq!(
            map(
                status(State::Running, Liveness::Unknown, None),
                now,
                grace()
            )
            .outcome,
            Outcome::Pending
        );
    }

    #[test]
    fn observe_reads_the_run_record_and_missing_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        let a = MurRunAdapter::new(d.path());
        assert_eq!(a.observe("nope", None).outcome, Outcome::Unknown);
        crate::run_status::store::save(d.path(), &run(State::Done, None)).unwrap();
        assert_eq!(a.observe("run-1", None).outcome, Outcome::Succeeded);
        assert!(a.validate_reference("run-1").is_ok());
        assert!(a.validate_reference("bad id!").is_err());
    }
}
