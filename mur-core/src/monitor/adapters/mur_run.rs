//! `source.type: mur_run` — reads the unified run status by `run_id`
//! (spec §MVP Adapter → MUR run). Query only: a monitor NEVER re-dispatches
//! the run; a tool that stopped waiting is not evidence the run stopped.

use std::path::{Path, PathBuf};

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

/// Pure mapping from a classified run to an observation.
pub fn map(s: RunStatus) -> Observation {
    match s.state {
        State::Done => Observation::terminal(Outcome::Succeeded, "run state: done"),
        State::Failed => Observation::terminal(Outcome::Failed, "run state: failed"),
        State::Stopped => Observation::terminal(Outcome::Cancelled, "run state: stopped"),
        State::Running | State::Blocked => match s.liveness {
            // The process is gone and nothing wrote a terminal state: we do
            // not know what happened, and saying `failed` would be a guess.
            Liveness::Dead => {
                Observation::unknown("process is dead but no terminal state was recorded")
            }
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
            Ok(Some(s)) => map(s),
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
    use chrono::{TimeZone, Utc};

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
            run: run(state, beat),
        }
    }

    #[test]
    fn terminal_states_map_to_terminal_outcomes() {
        assert_eq!(
            map(status(State::Done, Liveness::NotApplicable, None)).outcome,
            Outcome::Succeeded
        );
        assert_eq!(
            map(status(State::Failed, Liveness::NotApplicable, None)).outcome,
            Outcome::Failed
        );
        assert_eq!(
            map(status(State::Stopped, Liveness::NotApplicable, None)).outcome,
            Outcome::Cancelled
        );
    }

    #[test]
    fn running_is_pending_with_the_heartbeat_as_progress() {
        let b1 = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 10).unwrap();
        let b2 = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 20).unwrap();
        let o1 = map(status(State::Running, Liveness::Alive, Some(b1)));
        let o2 = map(status(State::Running, Liveness::Alive, Some(b2)));
        assert_eq!(o1.outcome, Outcome::Pending);
        assert_ne!(
            o1.progress_token, o2.progress_token,
            "a new heartbeat is progress"
        );
        assert_eq!(
            o1.progress_token,
            map(status(State::Running, Liveness::Stalled, Some(b1))).progress_token,
            "stalled is the deadline evaluator's call, not the adapter's"
        );
        assert_eq!(
            map(status(State::Blocked, Liveness::Alive, Some(b1))).outcome,
            Outcome::Pending
        );
    }

    #[test]
    fn dead_process_without_terminal_record_is_unknown_not_failed() {
        let o = map(status(State::Running, Liveness::Dead, Some(Utc::now())));
        assert_eq!(o.outcome, Outcome::Unknown);
        assert!(o.adapter_error.is_some());
        assert_eq!(
            map(status(State::Running, Liveness::Unknown, None)).outcome,
            Outcome::Pending,
            "a rebuilt record with no heartbeat is still running as far as we know"
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
