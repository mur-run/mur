//! #2320 / #2321: a run an agent dispatches is recorded in that agent's home,
//! not in the `runs/` tree every allowlisted agent could write.
//!
//! These drive `dir_for_owner` directly instead of setting `MUR_RUN_OWNER`:
//! the env is process-global and tests run in parallel.

use super::*;
use crate::run_status::{RUN_SCHEMA, RunKind, RunState, State};

fn record(run_id: &str, kind: RunKind, channel_id: Option<&str>) -> RunState {
    RunState {
        schema: RUN_SCHEMA,
        run_id: run_id.to_string(),
        channel_id: channel_id.map(str::to_string),
        kind,
        label: "l".into(),
        pid: std::process::id(),
        started_at: chrono::Utc::now(),
        last_heartbeat_at: None,
        state: State::Running,
        steps: vec![],
        blocked_on: None,
        binary_version: "0".into(),
        build_sha: "x".into(),
    }
}

/// Record `run` where a process running for `owner` would.
fn save_as(h: &Path, owner: Option<&str>, run: &RunState) -> PathBuf {
    let dir = dir_for_owner(h, run.kind, run.channel_id.as_deref(), owner, &run.run_id);
    save_in(&dir, run).unwrap();
    dir
}

#[test]
fn an_agent_job_run_is_recorded_in_the_agent_home() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let dir = save_as(h, Some("bob"), &record("run-1", RunKind::Job, Some("c")));
    assert_eq!(
        dir,
        mur_common::paths::agent_runs_dir(h, "bob").join("run-1")
    );
    assert!(!runs_dir(h).join("run-1").exists());
}

#[test]
fn a_workflow_without_an_owner_stays_in_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let dir = save_as(h, None, &record("run-2", RunKind::Workflow, None));
    assert_eq!(dir, runs_dir(h).join("run-2"));
}

/// The owner never moves a fleet run out of its fleet's carve-in: resume and
/// pause read it there and only there.
#[test]
fn a_fleet_run_ignores_the_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let dir = save_as(
        h,
        Some("bob"),
        &record("fleet-x-1", RunKind::Fleet, Some("fleet-x")),
    );
    assert_eq!(dir, fleet_run_dir(h, "x", "fleet-x-1"));
}

#[test]
fn readers_find_an_agent_run_by_id() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    save_as(h, Some("bob"), &record("run-3", RunKind::Job, Some("c")));
    assert_eq!(
        run_dir(h, "run-3"),
        mur_common::paths::agent_runs_dir(h, "bob").join("run-3")
    );
    assert_eq!(load(h, "run-3").unwrap().unwrap().run_id, "run-3");
    assert!(list_ids(h).unwrap().contains(&"run-3".to_string()));
    let stopped = update(h, "run-3", |r| r.state = State::Stopped).unwrap();
    assert!(stopped);
    assert_eq!(load(h, "run-3").unwrap().unwrap().state, State::Stopped);
}

/// Another agent that plants a copy of the id in its own tree does not get to
/// answer for it: the id is ambiguous and mutations by id refuse.
#[test]
fn a_planted_copy_in_another_agent_tree_is_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    save_as(h, Some("bob"), &record("run-4", RunKind::Job, Some("c")));
    save_as(h, Some("eve"), &record("run-4", RunKind::Job, Some("c")));
    assert_eq!(run_dirs_of(h, "run-4").len(), 2);
    assert!(crate::run_status::status_of(h, "run-4").is_err());
}

/// A directory under `agents/` whose name is not a valid agent name is never
/// searched: the name would become a path segment.
#[test]
fn an_invalid_agent_dir_name_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let bad = h.join(mur_common::paths::AGENTS).join(".hidden");
    let dir = bad.join(mur_common::paths::RUNS).join("run-5");
    std::fs::create_dir_all(&dir).unwrap();
    save_in(&dir, &record("run-5", RunKind::Job, None)).unwrap();
    assert!(run_dirs_of(h, "run-5").is_empty());
    assert!(!list_ids(h).unwrap().contains(&"run-5".to_string()));
}

/// #2352: an inherited `MUR_RUN_OWNER` (an agent shell running `cargo test`)
/// must not reroute the records other tests save into `runs/`.
#[test]
fn unit_tests_ignore_an_inherited_run_owner() {
    assert_eq!(process_run_owner(), None);
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    assert_eq!(
        dir_for(h, RunKind::Job, Some("c"), "run-1"),
        runs_dir(h).join("run-1")
    );
}
