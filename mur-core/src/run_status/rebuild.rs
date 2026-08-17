//! Rebuild a run record from its channel event log.
//!
//! `run.json` is a cache. When it is missing or unparseable, the channel is
//! the source of truth for everything except `last_heartbeat_at`, which stays
//! `None` — a rebuilt run reports `Liveness::Unknown`, never a fabricated beat.

use std::path::Path;

use anyhow::Result;
use mur_common::channel::EventKind;

use super::{RUN_SCHEMA, RunKind, RunState, State, StepState};

/// Derive a `RunState` from `channel_id`'s events. `Ok(None)` when the channel
/// does not exist.
pub fn from_channel(mur_home: &Path, run_id: &str, channel_id: &str) -> Result<Option<RunState>> {
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let events = match svc.load_events(channel_id) {
        Ok(events) => events,
        Err(_) => return Ok(None),
    };
    if events.is_empty() {
        return Ok(None);
    }

    let started_at = events[0].ts;
    let mut state = State::Running;
    let mut steps: Vec<StepState> = Vec::new();

    for ev in &events {
        match ev.kind {
            EventKind::Delegation => {
                // The payload is built by `mur_channel::service::delegation_payload`
                // and carries `target_agent`, `child_task_id`, `parent_channel_id`
                // and `goal` — there is NO `step_id`. The DAG's own step id is
                // not recoverable from the channel, so a rebuilt step is
                // identified by its child task id, which IS unique per
                // delegation. Do not invent a step id.
                if let Some(id) = ev.payload.get("child_task_id").and_then(|v| v.as_str()) {
                    steps.push(StepState {
                        id: id.to_string(),
                        member: ev
                            .payload
                            .get("target_agent")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        state: State::Running,
                        started_at: Some(ev.ts),
                        ended_at: None,
                    });
                }
            }
            EventKind::StateChange => {
                // `ChannelService::transition` writes {"from": .., "to": ..}
                // using `state_str` (mur-channel/src/service.rs:49-60), which
                // is a CLOSED set of eight values. Map all eight — a missing
                // arm silently leaves a finished run reporting `running`,
                // which is the exact defect this module exists to remove.
                // If `state_str` ever gains a variant, this match must gain
                // an arm with it.
                match ev.payload.get("to").and_then(|v| v.as_str()) {
                    Some("completed") => state = State::Done,
                    Some("failed") | Some("rejected") => state = State::Failed,
                    Some("canceled") => state = State::Stopped,
                    // A stale channel was abandoned rather than deliberately
                    // stopped; it did not complete, so it is a failure.
                    Some("stale") => state = State::Failed,
                    Some("input-required") => state = State::Blocked,
                    Some("working") | Some("submitted") => state = State::Running,
                    _ => {}
                }
                if state.is_terminal() {
                    for s in steps.iter_mut().filter(|s| s.ended_at.is_none()) {
                        s.state = state;
                        s.ended_at = Some(ev.ts);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(Some(RunState {
        schema: RUN_SCHEMA,
        run_id: run_id.to_string(),
        channel_id: Some(channel_id.to_string()),
        // Job vs Workflow is unrecoverable from the channel alone: `RunKind`
        // is spawn-time-only metadata never written to the event log, and
        // `jobs.rs:132` calls `create_for_workflow("parallel-jobs")`, so a
        // Job run's channel is shaped exactly like a workflow's — this can
        // misreport a rebuilt Job run as `Workflow`. Fleet IS recoverable:
        // `create_for_fleet` unconditionally sets `id: format!("fleet-{name}")`
        // (mur-channel/src/service.rs:140) and is the only site producing
        // that shape, while `create_for_workflow` mints a uuid v7 that
        // structurally cannot collide with it — so this never misreports
        // Fleet.
        kind: if channel_id.starts_with("fleet-") {
            RunKind::Fleet
        } else {
            RunKind::Workflow
        },
        label: format!("rebuilt from {channel_id}"),
        // pid 0 is NOT a "dead" sentinel. `pid_alive` (mur_common::lock_file)
        // calls `kill(pid, 0)` on Unix, and POSIX defines pid 0 as the
        // caller's own process group, so `pid_alive(0)` is `true`. A rebuilt
        // non-terminal run therefore does NOT take classify()'s `Dead`
        // branch — it falls through to the heartbeat match, which is `None`
        // here, and lands on `Liveness::Unknown`: the honest answer for a
        // record with no known process, not a false "alive" and not a
        // fabricated "dead". NEVER pass a rebuilt record's `pid` to a
        // signalling call: `kill(0, sig)` targets the caller's own process
        // group, not "no process".
        pid: 0,
        started_at,
        last_heartbeat_at: None,
        state,
        steps,
        blocked_on: None,
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        build_sha: mur_common::build::SHORT_SHA.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{Liveness, State, classify};

    /// The reported diagnosis path, automated: with `run.json` deleted, the
    /// channel still knows the run failed. What it cannot know is the
    /// heartbeat — and the rebuilt record must SAY so rather than guess.
    #[test]
    fn rebuild_recovers_state_and_admits_the_heartbeat_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let channel_id = seed_channel_with_a_failed_delegation(mur_home);

        let rebuilt = from_channel(mur_home, "run-x", &channel_id)
            .unwrap()
            .expect("channel exists, so a record must be derivable");

        assert_eq!(rebuilt.state, State::Failed, "channel said failed");
        assert_eq!(
            rebuilt.last_heartbeat_at, None,
            "heartbeat is not recoverable and must not be invented"
        );
        assert_eq!(
            rebuilt.steps.len(),
            1,
            "the rebuild is not reconstructing steps: expected one step from the one delegation"
        );
        assert_eq!(
            rebuilt.steps[0].id, "child-task-1",
            "the rebuild is not reconstructing steps: step id must be the delegation's child_task_id"
        );
        assert_eq!(
            rebuilt.steps[0].member.as_deref(),
            Some("pm"),
            "the rebuild is not reconstructing steps: step member must be the delegation's target_agent"
        );
        assert_eq!(
            rebuilt.kind,
            RunKind::Workflow,
            "a channel minted by create_for_workflow must NOT be misread as Fleet — the derivation must be two-sided, not a function that always returns Fleet"
        );

        let status = classify(rebuilt, chrono::Utc::now(), chrono::Duration::seconds(30));
        assert_eq!(
            status.liveness,
            Liveness::NotApplicable,
            "a failed run reports no liveness"
        );
    }

    #[test]
    fn rebuild_of_an_unknown_channel_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            from_channel(tmp.path(), "run-x", "no-such-channel")
                .unwrap()
                .is_none()
        );
    }

    /// pid 0 is not a dead-process sentinel — POSIX treats it as the
    /// caller's own process group, so `pid_alive(0)` is `true` and
    /// `classify()` falls through to the heartbeat check instead of taking
    /// the `Dead` branch. A rebuilt, still-running record has no heartbeat,
    /// so this must land on `Unknown` — never `Alive` (pid 0 must not read
    /// as healthy) and never `Dead` (it must not read as crashed either).
    /// The other two tests both end in a terminal state, which
    /// short-circuits `classify()` before the pid is ever examined; this is
    /// the only test that exercises the pid-0 path at all.
    #[test]
    fn rebuild_of_a_non_terminal_run_classifies_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let channel_id = seed_channel_still_working(mur_home);

        let rebuilt = from_channel(mur_home, "run-x", &channel_id)
            .unwrap()
            .expect("channel exists, so a record must be derivable");
        assert_eq!(rebuilt.state, State::Running, "channel said working");

        let status = classify(rebuilt, chrono::Utc::now(), chrono::Duration::seconds(30));
        assert_eq!(
            status.liveness,
            Liveness::Unknown,
            "pid 0 is alive per POSIX (caller's own process group), but there is no heartbeat to confirm it — must read Unknown, not Alive"
        );
    }

    /// Helper: write a channel whose events describe a delegation that moved
    /// to `failed`. Built through the REAL `ChannelService` calls the executor
    /// uses, never by hand-writing payloads — so if the event contract changes
    /// this test breaks instead of quietly testing a shape nothing emits.
    fn seed_channel_with_a_failed_delegation(mur_home: &std::path::Path) -> String {
        use mur_common::channel::{ChannelActor, ChannelState};
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_workflow("rebuild-test")
            .expect("create channel")
            .id;
        svc.append_delegation(&id, "pm", "child-task-1", None)
            .expect("append delegation");
        svc.transition(&id, ChannelState::Failed, ChannelActor::System)
            .expect("transition to failed");
        id
    }

    /// Helper: write a channel whose last event moves it to `working` —
    /// still open, non-terminal. Built through the same real
    /// `ChannelService` calls as the failed-delegation helper above.
    fn seed_channel_still_working(mur_home: &std::path::Path) -> String {
        use mur_common::channel::{ChannelActor, ChannelState};
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_workflow("rebuild-test-running")
            .expect("create channel")
            .id;
        svc.append_delegation(&id, "pm", "child-task-2", None)
            .expect("append delegation");
        svc.transition(&id, ChannelState::Working, ChannelActor::System)
            .expect("transition to working");
        id
    }

    /// `create_for_fleet` is the only site that mints a `fleet-{name}`
    /// channel id — `from_channel`'s Fleet derivation is coupled to that
    /// exact format (`mur-channel/src/service.rs:140`), and every other test
    /// in this file seeds through `create_for_workflow`, so without this
    /// test the Fleet branch never executes at all. Calls the real
    /// constructor rather than fabricating a `fleet-…` id by hand, so this
    /// breaks loudly if `create_for_fleet` ever changes its id format
    /// instead of silently drifting out of sync with `from_channel`.
    #[test]
    fn rebuild_of_a_fleet_channel_reports_fleet_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let channel_id = seed_fleet_channel(mur_home);

        let rebuilt = from_channel(mur_home, "run-x", &channel_id)
            .unwrap()
            .expect("channel exists, so a record must be derivable");

        assert_eq!(
            rebuilt.kind,
            RunKind::Fleet,
            "a channel minted by create_for_fleet must rebuild as RunKind::Fleet, not fall through to the Workflow default"
        );
    }

    /// Helper: write a channel through the real `create_for_fleet`
    /// constructor, then append one delegation so the channel has at least
    /// one event (`create_for_fleet` itself writes none — confirmed by
    /// `create_for_fleet_sets_roles_and_id` in mur-channel's own test suite,
    /// which asserts zero events right after creation).
    fn seed_fleet_channel(mur_home: &std::path::Path) -> String {
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let ch = svc
            .create_for_fleet("rebuild-test", "mur", &["pm".to_string()])
            .expect("create fleet channel");
        svc.append_delegation(&ch.id, "pm", "child-task-3", None)
            .expect("append delegation");
        ch.id
    }
}
