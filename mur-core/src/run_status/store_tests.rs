use super::*;
use crate::run_status::{RUN_SCHEMA, RunKind, RunState, State, StepState};

fn sample(run_id: &str) -> RunState {
    RunState {
        schema: RUN_SCHEMA,
        run_id: run_id.to_string(),
        channel_id: Some("chan-1".into()),
        kind: RunKind::Job,
        label: "fan out 3 jobs".into(),
        pid: std::process::id(),
        started_at: chrono::Utc::now(),
        last_heartbeat_at: None,
        state: State::Running,
        steps: vec![],
        blocked_on: None,
        binary_version: "0.0.0-test".into(),
        build_sha: "deadbee".into(),
    }
}

/// A fleet's trusted read never follows a link out of its tree (#2316).
#[cfg(unix)]
#[test]
fn a_linked_fleet_record_is_refused_by_the_trusted_readers() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let mut run = sample("fleet-dev-1");
    run.kind = RunKind::Fleet;
    run.channel_id = Some("fleet-dev".into());
    let forged = h.join("runs").join("forged");
    std::fs::create_dir_all(&forged).unwrap();
    std::fs::write(forged.join(RUN_FILE), serde_json::to_vec(&run).unwrap()).unwrap();
    let dir = fleet_run_dir(h, "dev", "fleet-dev-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(forged.join(RUN_FILE), dir.join(RUN_FILE)).unwrap();

    assert!(load_in_fleet(h, "dev", "fleet-dev-1").is_err());
    assert!(update_in_fleet(h, "dev", "fleet-dev-1", |_| {}).is_err());
}

/// Another fleet's agent can create a copy of B's run id in its own tree;
/// a scoped read sees only B's, and the id-only list names both.
#[test]
fn a_copy_in_another_fleets_tree_does_not_shadow_a_scoped_read() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let mut real = sample("fleet-b-1");
    real.kind = RunKind::Fleet;
    real.channel_id = Some("fleet-b".into());
    save(h, &real).unwrap();
    let mut forged = real.clone();
    forged.state = State::Done;
    save_in(&fleet_run_dir(h, "a", "fleet-b-1"), &forged).unwrap();

    let scoped = load_scoped(h, Some("b"), "fleet-b-1").unwrap().unwrap();
    assert_eq!(scoped.state, State::Running);
    assert_eq!(run_dirs_of(h, "fleet-b-1").len(), 2);
}

#[test]
fn save_then_load_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let run = sample("run-a");
    save(tmp.path(), &run).unwrap();
    let back = load(tmp.path(), "run-a").unwrap().expect("run.json exists");
    assert_eq!(back.run_id, "run-a");
    assert_eq!(back.state, State::Running);
    assert_eq!(back.kind, RunKind::Job);
}

#[test]
fn load_missing_run_is_none_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load(tmp.path(), "nope").unwrap().is_none());
}

#[test]
fn sidecar_round_trips_every_recorded_fact() {
    use crate::run_status::{SIDECAR_SCHEMA, Sidecar};

    let tmp = tempfile::tempdir().unwrap();
    let sidecar = Sidecar {
        schema: SIDECAR_SCHEMA,
        channel_id: "chan-1".into(),
        kind: RunKind::Fleet,
        first_seq: 42,
    };
    save_sidecar(tmp.path(), "run-a", &sidecar).unwrap();
    let back = load_sidecar(tmp.path(), "run-a")
        .unwrap()
        .expect("sidecar exists");
    assert_eq!(
        back, sidecar,
        "every recorded fact must survive the round trip"
    );
    assert_eq!(back.kind, RunKind::Fleet);
    assert_eq!(back.first_seq, 42);
}

#[test]
fn load_sidecar_missing_is_none_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load_sidecar(tmp.path(), "nope").unwrap().is_none());
}

#[test]
fn save_is_atomic_under_concurrent_reads() {
    use std::thread;

    // A payload with thousands of steps is tens of KB pretty-printed —
    // large enough that writing it is not a single-page operation, so a
    // non-atomic save (write straight into `run.json`, no temp file, no
    // rename) has a real window in which a concurrent reader can observe
    // a truncated or partially-written file. A tiny payload can pass
    // this test by luck even against a direct write; this one can't.
    fn payload(run_id: &str, tag: &str) -> RunState {
        let mut run = sample(run_id);
        run.label = tag.to_string();
        run.steps = (0..3000)
            .map(|i| StepState {
                id: format!("{tag}-{i}"),
                member: None,
                state: State::Running,
                started_at: None,
                ended_at: None,
                error: None,
            })
            .collect();
        run
    }

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().to_path_buf();
    let run_id = "run-race";
    let payload_a = payload(run_id, "a");
    let payload_b = payload(run_id, "b");

    let writer_home = mur_home.clone();
    let iterations = 200;
    let writer = thread::spawn(move || {
        for i in 0..iterations {
            let p = if i % 2 == 0 { &payload_a } else { &payload_b };
            save(&writer_home, p).unwrap();
        }
    });

    // Read concurrently while the writer is still looping. Every read
    // must either see nothing yet, or a complete, internally-consistent
    // record — never a parse failure, and never a record that mixes both
    // payloads together.
    let mut successful_loads = 0usize;
    while !writer.is_finished() {
        match load(&mur_home, run_id) {
            Ok(None) => {}
            Ok(Some(run)) => {
                assert!(
                    run.steps
                        .iter()
                        .all(|s| s.id.starts_with(run.label.as_str())),
                    "load() returned a run.json whose steps don't all match \
                     its own label ({}) — save() tore two different writes \
                     together, which temp-file-plus-rename is supposed to \
                     make impossible",
                    run.label
                );
                successful_loads += 1;
            }
            Err(e) => panic!(
                "load() failed to parse run.json while save() was \
                 concurrently writing it — this is exactly the \
                 half-written record that temp-file-plus-rename exists \
                 to prevent: {e:#}"
            ),
        }
    }
    writer.join().unwrap();

    // No leftover run.json.tmp after the dust settles, even under load.
    let entries: Vec<_> = std::fs::read_dir(runs_dir(&mur_home).join(run_id))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec!["run.json".to_string()],
        "temp file left behind after concurrent saves"
    );

    // One more read after the writer is done, so the final state is
    // checked too.
    match load(&mur_home, run_id) {
        Ok(Some(_)) => successful_loads += 1,
        Ok(None) => panic!("run.json missing after the writer finished"),
        Err(e) => panic!("final load() failed to parse run.json: {e:#}"),
    }

    assert!(
        successful_loads >= 10,
        "only {successful_loads} concurrent loads observed a complete \
         record — the reader loop raced past without exercising the \
         writer, which proves nothing; increase iterations or payload size"
    );
}

#[test]
fn update_on_missing_run_is_noop_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().to_path_buf();
    let mut called = false;

    let applied = update(&mur_home, "ghost", |run| {
        called = true;
        run.state = State::Done;
    })
    .unwrap();

    assert!(!applied, "update on a missing run must report Ok(false)");
    assert!(
        !called,
        "mutate must not run when there is no record to mutate"
    );
    assert!(
        load(&mur_home, "ghost").unwrap().is_none(),
        "update must not manufacture a run that was never recorded"
    );
    assert!(
        !runs_dir(&mur_home).join("ghost").exists(),
        "update must not create a directory for a run that doesn't exist"
    );
}

#[test]
fn update_applies_and_persists_the_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    save(tmp.path(), &sample("run-a")).unwrap();

    let applied = update(tmp.path(), "run-a", |run| {
        run.state = State::Done;
    })
    .unwrap();

    assert!(applied, "update on an existing run must report Ok(true)");
    let after = load(tmp.path(), "run-a").unwrap().unwrap();
    assert_eq!(after.state, State::Done);
}

/// Directly proves the concurrency guarantee `update` exists for: many
/// independent writers — real OS threads, so genuinely preemptible on
/// every platform, not tokio's single-threaded-by-default test flavor —
/// each append one uniquely-identified step. Without mutual exclusion, a
/// classic read-modify-write race drops updates whenever two threads
/// read the same "before" snapshot and the second writer's save()
/// overwrites the first writer's addition. If the lock genuinely
/// excludes, all of them land; if it doesn't, this reliably loses some.
#[test]
fn concurrent_updates_all_land_with_no_lost_writes() {
    use std::thread;

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().to_path_buf();
    let run_id = "run-concurrent";
    save(&mur_home, &sample(run_id)).unwrap();

    let writers = 20;
    let handles: Vec<_> = (0..writers)
        .map(|i| {
            let home = mur_home.clone();
            thread::spawn(move || {
                let applied = update(&home, run_id, |run| {
                    run.steps.push(StepState {
                        id: format!("step-{i}"),
                        member: None,
                        state: State::Running,
                        started_at: None,
                        ended_at: None,
                        error: None,
                    });
                })
                .unwrap();
                assert!(applied, "update on an existing run must apply");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let after = load(&mur_home, run_id)
        .unwrap()
        .expect("record still there");
    let mut ids: Vec<_> = after.steps.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    let mut expected: Vec<_> = (0..writers).map(|i| format!("step-{i}")).collect();
    expected.sort();
    assert_eq!(
        after.steps.len(),
        writers,
        "expected exactly {writers} steps, one per concurrent update — a \
         lost update means two writers raced past the lock instead of \
         serializing through it"
    );
    assert_eq!(ids, expected, "every writer's step id must survive");

    // No leftover .lock cruft mid-run either — best-effort sanity check,
    // not load-bearing for correctness.
    assert!(
        runs_dir(&mur_home).join(run_id).join(".lock").exists(),
        "lock file should exist (created on first update)"
    );
}

#[test]
fn list_ids_returns_every_saved_run() {
    let tmp = tempfile::tempdir().unwrap();
    save(tmp.path(), &sample("run-a")).unwrap();
    save(tmp.path(), &sample("run-b")).unwrap();
    let mut ids = list_ids(tmp.path()).unwrap();
    ids.sort();
    assert_eq!(ids, vec!["run-a".to_string(), "run-b".to_string()]);
}

#[test]
fn list_ids_on_missing_runs_dir_is_empty_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(list_ids(tmp.path()).unwrap().is_empty());
}
