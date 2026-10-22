//! Pause/resume design §5.2: a paused run writes no terminal channel state and
//! records `State::Paused`, never `Stopped`/`Done`.

use super::run::{finalize_run, terminal_channel_state};
use super::*;

/// The load-bearing rule: a `canceled` (or any terminal) for a paused run
/// would let the first reconcile cancel a job that is meant to resume.
#[test]
fn a_paused_outcome_writes_no_terminal_channel_state() {
    assert_eq!(terminal_channel_state(RunOutcome::Paused), None);
    // Blocked is the other non-terminal outcome and must stay that way.
    assert_eq!(terminal_channel_state(RunOutcome::Blocked), None);
}

/// Negative control: every other outcome still closes the channel, so the
/// test above is not satisfied by a function that always returns `None`.
#[test]
fn every_other_outcome_still_closes_the_channel() {
    assert_eq!(
        terminal_channel_state(RunOutcome::Done),
        Some(ChannelState::Completed)
    );
    assert_eq!(
        terminal_channel_state(RunOutcome::Failed),
        Some(ChannelState::Failed)
    );
    assert_eq!(
        terminal_channel_state(RunOutcome::Stopped),
        Some(ChannelState::Canceled)
    );
}

#[tokio::test]
async fn finalize_run_records_a_paused_outcome_as_state_paused() {
    use crate::run_status::{RunKind, RunState, State, store};
    let tmp = tempfile::TempDir::new().unwrap();
    let record = RunState {
        schema: crate::run_status::RUN_SCHEMA,
        run_id: "r".into(),
        channel_id: None,
        kind: RunKind::Fleet,
        label: "l".into(),
        pid: std::process::id(),
        started_at: chrono::Utc::now(),
        last_heartbeat_at: Some(chrono::Utc::now()),
        state: State::Running,
        steps: vec![],
        blocked_on: None,
        binary_version: "0.0.0-test".into(),
        build_sha: "deadbee".into(),
    };
    store::save(tmp.path(), &record).unwrap();
    let mut hb = None;
    finalize_run(tmp.path(), "r", true, &mut hb, RunOutcome::Paused, 0).await;
    let after = store::load(tmp.path(), "r").unwrap().unwrap();
    assert_eq!(after.state, State::Paused);
    assert!(!after.state.is_terminal());
}
