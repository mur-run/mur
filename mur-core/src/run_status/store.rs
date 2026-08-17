//! Atomic read/write of `~/.mur/runs/<run_id>/run.json`. Pure I/O — no policy.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

use super::RunState;

/// `<mur_home>/runs`.
pub fn runs_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("runs")
}

/// `<mur_home>/runs/<run_id>/run.json`.
pub fn run_path(mur_home: &Path, run_id: &str) -> PathBuf {
    runs_dir(mur_home).join(run_id).join("run.json")
}

/// Write `run` atomically: serialize to `run.json.tmp`, then rename over
/// `run.json`. A reader therefore never observes a half-written record — the
/// same temp-file-plus-rename discipline `store/yaml.rs` uses.
pub fn save(mur_home: &Path, run: &RunState) -> Result<()> {
    let dir = runs_dir(mur_home).join(&run.run_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let final_path = dir.join("run.json");
    let tmp_path = dir.join("run.json.tmp");
    let body = serde_json::to_vec_pretty(run).context("serialize run state")?;
    {
        let mut f = std::fs::File::create(&tmp_path)
            .with_context(|| format!("create {}", tmp_path.display()))?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename into {}", final_path.display()))?;
    Ok(())
}

/// The `channel.id` sidecar — one line, the channel this run's events live
/// on. Deliberately separate from `run.json`: a corrupt cache must not take
/// the channel index down with it. See `status_of`'s fallback and the module
/// doc's stated limitation.
pub fn save_channel_id(mur_home: &Path, run_id: &str, channel_id: &str) -> Result<()> {
    let dir = runs_dir(mur_home).join(run_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    // Temp-file-plus-rename, same discipline as `save`: a crash mid-write
    // must leave either the previous sidecar or none at all, never a
    // truncated one (a truncated sidecar reads as "no sidecar", so the run
    // silently becomes unrecoverable instead of corrupting).
    let final_path = dir.join("channel.id");
    let tmp_path = dir.join("channel.id.tmp");
    std::fs::write(&tmp_path, format!("{channel_id}\n"))
        .with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename into {}", final_path.display()))
}

pub fn load_channel_id(mur_home: &Path, run_id: &str) -> std::io::Result<Option<String>> {
    let path = runs_dir(mur_home).join(run_id).join("channel.id");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(s) => Ok(Some(s.trim().to_string())),
    }
}

/// Load-modify-save a run record under an exclusive lock, so two independent
/// writers of the same run — the in-process heartbeat ticker and a separate
/// `mur job stop <run_id>` process are the two that exist today — cannot
/// race each other's read-modify-write. `Arc<AtomicBool>`/`JoinHandle`
/// coordination (see `heartbeat::Heartbeat::stop`) only holds within one
/// process; this is the mechanism that also holds across a process boundary.
///
/// `mutate` runs only if a record already exists; returns `Ok(false)`
/// without creating anything when it does not — "nothing to update" must
/// stay a true no-op, not manufacture a run that was never recorded. Do not
/// call `update` again for the same `run_id` from inside `mutate` — the lock
/// is not reentrant and it would deadlock.
pub fn update<F>(mur_home: &Path, run_id: &str, mutate: F) -> Result<bool>
where
    F: FnOnce(&mut RunState),
{
    let dir = runs_dir(mur_home).join(run_id);
    // No directory at all ⇒ definitely no record. Return before creating
    // so much as a lock file for a run that was never saved.
    if !dir.exists() {
        return Ok(false);
    }

    // Serialize concurrent updates (this process's ticker + a separate `mur
    // job stop` process may write the same run) via a SIDECAR lock file —
    // never lock run.json itself. On Windows OS file locks are mandatory
    // (not advisory like flock on macOS/Linux), so holding a lock on the
    // data file blocks our own read/rename of it (os error 33). Locking a
    // separate file gives cross-process mutual exclusion while the data
    // file stays freely readable/writable on every platform. Matches
    // `mur_channel::store::ChannelStore::append_event`.
    let lock_path = dir.join(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    lock.lock_exclusive().context("lock run record")?;

    // The lock guards the whole load-modify-save; released below on every
    // path (Ok or Err) since it runs after this closure regardless of which
    // branch inside it returned.
    let result = (|| -> Result<bool> {
        let path = run_path(mur_home, run_id);
        let mut run = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            Ok(bytes) => serde_json::from_slice::<RunState>(&bytes)
                .with_context(|| format!("parse {}", path.display()))?,
        };
        mutate(&mut run);
        save(mur_home, &run)?;
        Ok(true)
    })();

    FileExt::unlock(&lock).ok();
    result
}

/// Read one run record. `Ok(None)` when the file does not exist — a run that
/// was never recorded is not an error, it is a rebuild candidate.
pub fn load(mur_home: &Path, run_id: &str) -> Result<Option<RunState>> {
    let path = run_path(mur_home, run_id);
    match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
        )),
    }
}

/// Every run id that has a directory under `runs/`. Missing `runs/` yields an
/// empty list: no runs have happened yet, which is not a failure.
pub fn list_ids(mur_home: &Path) -> Result<Vec<String>> {
    let dir = runs_dir(mur_home);
    let entries = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("read_dir {}", dir.display())),
        Ok(entries) => entries,
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            ids.push(name.to_string());
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
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
}
