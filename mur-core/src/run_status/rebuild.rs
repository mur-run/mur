//! Rebuild a run record from its channel event log.
//!
//! `run.json` is a cache. When it is missing or unparseable, the channel is
//! the source of truth for everything except `last_heartbeat_at`, which stays
//! `None` — a rebuilt run reports `Liveness::Unknown`, never a fabricated beat.
//!
//! The run boundary is the `run_id` carried inside events, NOT the channel:
//! channels are long-lived and reused (fleet `--loop` iterations, `mur
//! workflow run --channel <existing>`), so folding "the whole channel"
//! attributes other runs' states to this one. A rebuild folds only events
//! with `seq >= sidecar.first_seq` whose payload's `run_id` equals the run
//! being rebuilt; legacy events without a `run_id` are not claimed by anyone.

use std::path::Path;

use anyhow::Result;
use mur_common::channel::{ChannelEvent, EventKind};

use super::{RUN_SCHEMA, RunState, Sidecar, State, StepState};

/// The boundary rule: is `ev` one of `run_id`'s own events? The single
/// implementation of the rule, shared by [`from_channel`] and
/// [`run_tail_state`].
fn matches_run(ev: &ChannelEvent, sidecar: &Sidecar, run_id: &str) -> bool {
    ev.seq >= sidecar.first_seq
        && ev
            .payload
            .get("run_id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| id == run_id)
}

/// Map a channel `StateChange`'s `to` string to the stored run state.
/// `state_str` (mur-channel/src/service.rs) is a CLOSED set of eight values —
/// map all eight; a missing arm silently leaves a finished run reporting
/// `running`, which is the exact defect this module exists to remove. If
/// `state_str` ever gains a variant, this match must gain an arm with it.
fn channel_state_to_run_state(to: &str) -> Option<State> {
    match to {
        "completed" => Some(State::Done),
        "failed" | "rejected" => Some(State::Failed),
        "canceled" => Some(State::Stopped),
        // A stale channel was abandoned rather than deliberately stopped;
        // it did not complete, so it is a failure.
        "stale" => Some(State::Failed),
        "input-required" => Some(State::Blocked),
        "working" | "submitted" => Some(State::Running),
        _ => None,
    }
}

/// The state one of `run_id`'s own events puts the run in, if any: a
/// `StateChange` (mapped through [`channel_state_to_run_state`]) or a fleet
/// pause/resume note. The notes carry the run ids nested under `fleet_run`,
/// not at the top level, so [`matches_run`] cannot see them; they are claimed
/// when either their loop `run_id` or their `iteration_run_id` is this run.
/// Without this a paused run rebuilt from the channel read as `running`,
/// since a pause deliberately writes no `StateChange` (design §5.2).
fn event_state(ev: &ChannelEvent, sidecar: &Sidecar, run_id: &str) -> Option<State> {
    use crate::cmd::fleet::loop_run::pause_events::{
        FleetRunPayload, NoteClassification, classify_note_payload,
    };
    match ev.kind {
        EventKind::StateChange if matches_run(ev, sidecar, run_id) => ev
            .payload
            .get("to")
            .and_then(|v| v.as_str())
            .and_then(channel_state_to_run_state),
        EventKind::Note if ev.seq >= sidecar.first_seq => {
            let NoteClassification::FleetRun(env) = classify_note_payload(&ev.payload) else {
                return None;
            };
            let (state, loop_id, iter_id) = match &env.payload {
                FleetRunPayload::RunPaused {
                    run_id: l,
                    iteration_run_id: i,
                    ..
                } => (State::Paused, l, i),
                FleetRunPayload::RunResumed {
                    run_id: l,
                    iteration_run_id: i,
                    ..
                } => (State::Running, l, i),
            };
            (loop_id == run_id || iter_id == run_id).then_some(state)
        }
        _ => None,
    }
}

/// The last of this run's own state-bearing events and its timestamp.
fn tail(
    events: &[ChannelEvent],
    sidecar: &Sidecar,
    run_id: &str,
) -> Option<(State, chrono::DateTime<chrono::Utc>)> {
    events
        .iter()
        .filter_map(|ev| event_state(ev, sidecar, run_id).map(|s| (s, ev.ts)))
        .next_back()
}

/// The run's tail state per the channel: the LAST of this run's own
/// state-bearing events (see [`event_state`]), mapped to the stored state.
/// `Ok(None)` when the channel does not exist or carries no matching event.
///
/// Shared by [`from_channel`] (the rebuild's state) and `status_of`
/// (reconciliation of a parseable cache with the channel) — one fold
/// implementation, two callers.
pub fn run_tail_state(mur_home: &Path, sidecar: &Sidecar, run_id: &str) -> Result<Option<State>> {
    let svc = mur_channel::ChannelService::open(mur_home)?;
    // A missing channel is `Ok(vec![])` (`load_events` maps NotFound to
    // empty) and folds to `None` below — absence is not an error. A GENUINE
    // I/O fault propagates: collapsing it would make `status_of` report the
    // cache's stale answer as if the channel had nothing newer to say.
    let events = svc.load_events(&sidecar.channel_id)?;
    Ok(tail(&events, sidecar, run_id).map(|(state, _)| state))
}

/// Derive a `RunState` from the sidecar's channel, folding only this run's
/// own events. `Ok(None)` when the channel does not exist or carries no
/// event of this run.
pub fn from_channel(mur_home: &Path, run_id: &str, sidecar: &Sidecar) -> Result<Option<RunState>> {
    let svc = mur_channel::ChannelService::open(mur_home)?;
    // A missing channel is `Ok(vec![])` (`load_events` maps NotFound to
    // empty) and yields `None` below — absence is not an error. A GENUINE
    // I/O fault propagates: collapsing it into `Ok(None)` makes `status_of`
    // pretend the run never existed instead of reporting the failure.
    let events = svc.load_events(&sidecar.channel_id)?;
    let own: Vec<&ChannelEvent> = events
        .iter()
        .filter(|ev| matches_run(ev, sidecar, run_id))
        .collect();
    let Some(first) = own.first() else {
        return Ok(None);
    };
    // This run's first event, not the channel's first-ever timestamp — on a
    // shared channel the two can be arbitrarily far apart.
    let started_at = first.ts;

    let tail = tail(&events, sidecar, run_id);
    let state = tail.map_or(State::Running, |(state, _)| state);

    let mut steps: Vec<StepState> = Vec::new();
    for ev in &own {
        if ev.kind == EventKind::Delegation {
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
                    // A rebuild reconstructs from delegation events, which
                    // carry no outcome — `None` is honest, never a guess.
                    error: None,
                });
            }
        }
    }
    // A terminal run's open steps end with it. A paused run's do too: the
    // pause cancels member turns in flight, and resume starts fresh ones.
    let step_end = if state.is_terminal() {
        Some(state)
    } else if state == State::Paused {
        Some(State::Stopped)
    } else {
        None
    };
    if let Some(step_state) = step_end {
        let ended_at = tail.map(|(_, ts)| ts);
        for s in steps.iter_mut().filter(|s| s.ended_at.is_none()) {
            s.state = step_state;
            s.ended_at = ended_at;
        }
    }

    Ok(Some(RunState {
        schema: RUN_SCHEMA,
        run_id: run_id.to_string(),
        channel_id: Some(sidecar.channel_id.clone()),
        // The kind is a FACT recorded in the sidecar at recording time —
        // never inferred from the channel id's shape. `mur workflow run
        // --channel fleet-dev` records Workflow over a `fleet-…` id, and
        // prefix inference would misreport it as Fleet.
        kind: sidecar.kind,
        label: format!("rebuilt from {}", sidecar.channel_id),
        // pid 0 is NOT a "dead" sentinel, and no liveness verdict depends on
        // it: `classify` checks the absent heartbeat BEFORE the pid, so a
        // rebuilt non-terminal run lands on `Liveness::Unknown` on every
        // platform — the honest answer for a record with no known process.
        // (pid-0 semantics are platform-dependent: `kill(0, …)` targets the
        // caller's own process group on Unix, while `OpenProcess(0, …)`
        // fails on Windows.) NEVER pass a rebuilt record's `pid` to a
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
    use crate::run_status::{Liveness, RunKind, SIDECAR_SCHEMA, classify};
    use mur_common::channel::{ChannelActor, ChannelState};

    /// The sidecar the executor records: facts, not inference.
    fn sidecar(channel_id: &str, kind: RunKind) -> Sidecar {
        Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: channel_id.to_string(),
            kind,
            first_seq: 0,
        }
    }

    /// The reported diagnosis path, automated: with `run.json` deleted, the
    /// channel still knows the run failed. What it cannot know is the
    /// heartbeat — and the rebuilt record must SAY so rather than guess.
    #[test]
    fn rebuild_recovers_state_and_admits_the_heartbeat_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_workflow("rebuild-test")
            .expect("create channel")
            .id;
        svc.append_delegation(&id, "pm", "child-task-1", None, Some("run-x"))
            .expect("append delegation");
        svc.transition(
            &id,
            ChannelState::Failed,
            ChannelActor::System,
            Some("run-x"),
        )
        .expect("transition to failed");

        let rebuilt = from_channel(mur_home, "run-x", &sidecar(&id, RunKind::Workflow))
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
            "the kind must come from the sidecar"
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
            from_channel(
                tmp.path(),
                "run-x",
                &sidecar("no-such-channel", RunKind::Job)
            )
            .unwrap()
            .is_none()
        );
    }

    /// A missing channel yields `None` — absence is not an error. A GENUINE
    /// I/O fault (here: the channel directory replaced by a plain file, so
    /// the events read fails with a non-NotFound error) must PROPAGATE — collapsing it
    /// into `Ok(None)` makes `status_of` pretend the run never existed
    /// instead of reporting the failure.
    #[test]
    fn from_channel_propagates_genuine_channel_read_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("broken-channel").unwrap();
        // Sabotage: the channel dir becomes a file, so reading
        // Portable sabotage: `events.jsonl` becomes a DIRECTORY, so reading it
        // fails with a non-`NotFound` error on every platform (EISDIR on Unix,
        // ERROR_ACCESS_DENIED on Windows). Replacing the channel DIR with a
        // file does NOT work: Windows maps the resulting path error to
        // `NotFound`, which `load_events` legitimately reads as absence, so
        // the fault this test exists to catch would be swallowed there.
        let chan_dir = mur_home.join("channels").join(&ch.id);
        let events = chan_dir.join("events.jsonl");
        let _ = std::fs::remove_file(&events);
        std::fs::create_dir_all(&events).unwrap();

        let err = from_channel(mur_home, "run-x", &sidecar(&ch.id, RunKind::Job))
            .expect_err("a genuine channel read fault must surface as Err, not Ok(None)");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&ch.id),
            "the error must name the channel it failed to read: {msg}"
        );
    }

    /// A rebuilt, still-running record has no heartbeat, so `classify` must
    /// land on `Unknown` — never `Alive` (pid 0 must not read as healthy) and
    /// never `Dead` (it must not read as crashed either). The `Unknown`
    /// verdict comes from the absent-heartbeat check, which precedes the pid
    /// check, so it holds on every platform regardless of pid-0 semantics.
    #[test]
    fn rebuild_of_a_non_terminal_run_classifies_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_workflow("rebuild-test-running")
            .expect("create channel")
            .id;
        svc.append_delegation(&id, "pm", "child-task-2", None, Some("run-x"))
            .expect("append delegation");
        svc.transition(
            &id,
            ChannelState::Working,
            ChannelActor::System,
            Some("run-x"),
        )
        .expect("transition to working");

        let rebuilt = from_channel(mur_home, "run-x", &sidecar(&id, RunKind::Workflow))
            .unwrap()
            .expect("channel exists, so a record must be derivable");
        assert_eq!(rebuilt.state, State::Running, "channel said working");

        let status = classify(rebuilt, chrono::Utc::now(), chrono::Duration::seconds(30));
        assert_eq!(
            status.liveness,
            Liveness::Unknown,
            "there is no heartbeat to confirm the rebuilt record — must read Unknown, not Alive"
        );
    }

    /// The kind is a sidecar fact: a channel minted by `create_for_fleet` (id
    /// `fleet-…`) with a sidecar saying Fleet must rebuild as Fleet — and,
    /// more importantly, a sidecar saying Workflow over that same channel
    /// must rebuild as Workflow. Both directions are pinned below so the
    /// kind derivation cannot regress into inference from the id shape.
    #[test]
    fn rebuild_reports_the_kind_the_sidecar_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_fleet("rebuild-test", "mur", &["pm".to_string()])
            .expect("create fleet channel")
            .id;
        svc.append_delegation(&id, "pm", "child-task-3", None, Some("run-x"))
            .expect("append delegation");

        let fleet = from_channel(mur_home, "run-x", &sidecar(&id, RunKind::Fleet))
            .unwrap()
            .expect("channel exists");
        assert_eq!(
            fleet.kind,
            RunKind::Fleet,
            "a run recorded as Fleet must rebuild as Fleet"
        );
    }

    /// THE regression for the review's channel-naming finding: `mur workflow
    /// run --channel fleet-dev` records Workflow over a `fleet-…` channel id.
    /// The kind is a sidecar fact, so a corrupted cache must rebuild as
    /// Workflow — the deleted `starts_with("fleet-")` inference misreported
    /// it as Fleet.
    #[test]
    fn sidecar_kind_wins_over_the_channel_id_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_fleet("dev", "mur", &["pm".to_string()])
            .expect("create fleet channel")
            .id;
        assert!(
            id.starts_with("fleet-"),
            "precondition: the channel id really has the fleet- shape"
        );
        svc.append_delegation(&id, "pm", "child-task-4", None, Some("run-wf"))
            .expect("append delegation");

        let rebuilt = from_channel(mur_home, "run-wf", &sidecar(&id, RunKind::Workflow))
            .unwrap()
            .expect("channel exists");
        assert_eq!(
            rebuilt.kind,
            RunKind::Workflow,
            "a workflow run attached to a fleet-… channel must rebuild as \
             Workflow — the kind is a recorded fact, never inferred from the id"
        );
    }

    /// THE regression for the review's shared-channel finding: on ONE
    /// channel, run A fails, run B later completes, then A's cache is
    /// corrupted. A must rebuild as Failed with only A's step — never Done,
    /// never B's steps, never the channel's first-ever timestamp as its
    /// start. Legacy events without a run_id are claimed by no one.
    #[test]
    fn rebuild_on_a_shared_channel_claims_only_its_own_run() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_workflow("shared")
            .expect("create channel")
            .id;

        // A legacy event (no run_id) — must be claimed by no one.
        svc.append_delegation(&id, "pm", "child-legacy", None, None)
            .expect("append legacy delegation");

        // Run A: one delegation, then failed.
        svc.append_delegation(&id, "pm", "child-a", None, Some("run-a"))
            .expect("append A delegation");
        svc.transition(
            &id,
            ChannelState::Failed,
            ChannelActor::System,
            Some("run-a"),
        )
        .expect("transition A to failed");

        // Run B: one delegation, then completed.
        svc.append_delegation(&id, "pm", "child-b", None, Some("run-b"))
            .expect("append B delegation");
        svc.transition(
            &id,
            ChannelState::Completed,
            ChannelActor::System,
            Some("run-b"),
        )
        .expect("transition B to completed");

        let a = from_channel(mur_home, "run-a", &sidecar(&id, RunKind::Job))
            .unwrap()
            .expect("A's events exist");
        assert_eq!(
            a.state,
            State::Failed,
            "A must NOT be reported done because B later completed on the same channel"
        );
        assert_eq!(
            a.steps.len(),
            1,
            "A must contain only A's step — not B's, not the legacy event"
        );
        assert_eq!(a.steps[0].id, "child-a");

        // A's start is A's own first event, not the channel's first-ever
        // timestamp (the legacy event).
        let events = svc.load_events(&id).unwrap();
        let a_first = events
            .iter()
            .find(|ev| ev.payload.get("child_task_id").and_then(|v| v.as_str()) == Some("child-a"))
            .expect("A's delegation is in the log");
        assert_eq!(
            a.started_at, a_first.ts,
            "A's start must be A's own first event, not the channel's first-ever timestamp"
        );

        let b = from_channel(mur_home, "run-b", &sidecar(&id, RunKind::Job))
            .unwrap()
            .expect("B's events exist");
        assert_eq!(b.state, State::Done, "B completed");
        assert_eq!(b.steps.len(), 1, "B claims only its own delegation");
        assert_eq!(b.steps[0].id, "child-b");
    }

    /// The seq half of the boundary rule: a sidecar whose `first_seq` is
    /// AFTER this run's events must claim nothing — the run_id half alone
    /// is not enough to bound a fold on a reused channel.
    #[test]
    fn sidecar_first_seq_bounds_the_fold() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create_for_workflow("seq-bound")
            .expect("create channel")
            .id;
        svc.append_delegation(&id, "pm", "child-a", None, Some("run-a"))
            .expect("append delegation");

        let first = svc.load_events(&id).unwrap()[0].seq;

        let after = Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: id.clone(),
            kind: RunKind::Job,
            first_seq: first + 1,
        };
        assert!(
            from_channel(mur_home, "run-a", &after).unwrap().is_none(),
            "a first_seq after the run's only event must claim nothing"
        );

        let at = Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: id.clone(),
            kind: RunKind::Job,
            first_seq: first,
        };
        assert!(
            from_channel(mur_home, "run-a", &at).unwrap().is_some(),
            "a first_seq at the run's own first event must claim it"
        );
    }

    /// A fleet pause/resume note in the shape `pause.rs` / `resume.rs` write.
    fn fleet_run_note(svc: &mur_channel::ChannelService, id: &str, kind: &str, iter_run: &str) {
        let note = serde_json::json!({ "fleet_run": {
            "v": 1, "type": kind, "run_id": "loop", "iteration_run_id": iter_run,
            "iteration": 0, "spent_usd": 0.0, "active_elapsed_ms": 1, "reason": "operator",
        }});
        svc.append(id, ChannelActor::System, EventKind::Note, note, None)
            .expect("append note");
    }

    /// The `resume-smoke` field report: a paused iteration rebuilt from the
    /// channel read `running` with a `running` step, because the pause writes
    /// no `StateChange` and its note nests the run id under `fleet_run`.
    #[test]
    fn rebuild_of_a_paused_iteration_is_paused_with_its_step_ended() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let id = svc
            .create_for_fleet("dev", "mur", &["pm".to_string()])
            .unwrap()
            .id;
        svc.transition(
            &id,
            ChannelState::Working,
            ChannelActor::System,
            Some("loop-0"),
        )
        .unwrap();
        svc.append_delegation(&id, "pm", "ct-1", None, Some("loop-0"))
            .unwrap();
        fleet_run_note(&svc, &id, "run_paused", "loop-0");

        let sc = sidecar(&id, RunKind::Fleet);
        let rebuilt = from_channel(tmp.path(), "loop-0", &sc).unwrap().unwrap();
        assert_eq!(rebuilt.state, State::Paused);
        assert_eq!(
            rebuilt.steps[0].state,
            State::Stopped,
            "the pause ended the turn"
        );
        assert!(rebuilt.steps[0].ended_at.is_some());
        assert_eq!(
            run_tail_state(tmp.path(), &sc, "loop").unwrap(),
            Some(State::Paused)
        );
        let status = classify(rebuilt, chrono::Utc::now(), chrono::Duration::seconds(30));
        assert_eq!(status.liveness, Liveness::NotApplicable);

        // A later resume reopens it; a stop after that ends it.
        fleet_run_note(&svc, &id, "run_resumed", "loop-0");
        assert_eq!(
            run_tail_state(tmp.path(), &sc, "loop-0").unwrap(),
            Some(State::Running)
        );
        svc.transition(
            &id,
            ChannelState::Canceled,
            ChannelActor::System,
            Some("loop-0"),
        )
        .unwrap();
        assert_eq!(
            run_tail_state(tmp.path(), &sc, "loop-0").unwrap(),
            Some(State::Stopped)
        );
    }

    /// Another run's pause note on a shared channel is not this run's.
    #[test]
    fn a_pause_note_for_another_run_is_not_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let id = svc
            .create_for_fleet("dev", "mur", &["pm".to_string()])
            .unwrap()
            .id;
        svc.append_delegation(&id, "pm", "ct-1", None, Some("mine"))
            .unwrap();
        fleet_run_note(&svc, &id, "run_paused", "other-0");
        let rebuilt = from_channel(tmp.path(), "mine", &sidecar(&id, RunKind::Fleet))
            .unwrap()
            .unwrap();
        assert_eq!(rebuilt.state, State::Running);
    }
}
