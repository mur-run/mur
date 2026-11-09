//! Atomic read/write of a run's `run.json` and `sidecar.json`. Pure I/O — no
//! policy.
//!
//! A run directory lives in one of two places (#2316):
//!
//! - a fleet run: `<mur_home>/fleet-state/<fleet>/runs/<run_id>/`, inside the
//!   per-fleet sandbox carve-in, so only an agent allowed to run that fleet
//!   can write it;
//! - every other run (`parallel_jobs`, workflows): `<mur_home>/runs/<run_id>/`.
//!
//! Writers know which one from the record itself (`kind` + `channel_id`).
//! A reader holding only a run id resolves it with [`run_dir`]: the fleet
//! trees first, then `runs/`. Anything that TRUSTS a fleet run's files
//! (resume, pause, checkpoints) must not use that fallback — it reads
//! [`fleet_run_dir`] directly, because `runs/` is writable by every sandboxed
//! agent.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

use super::{RunKind, RunState, Sidecar};

const RUN_FILE: &str = "run.json";
const SIDECAR_FILE: &str = "sidecar.json";

/// `<mur_home>/runs` — non-fleet runs only.
pub fn runs_dir(mur_home: &Path) -> PathBuf {
    // Shared with the sandbox carve-in — see `mur_common::paths`.
    mur_home.join(mur_common::paths::RUNS)
}

/// `<mur_home>/fleet-state/<fleet>/runs/<run_id>`.
pub fn fleet_run_dir(mur_home: &Path, fleet: &str, run_id: &str) -> PathBuf {
    mur_common::paths::fleet_run_dir(mur_home, fleet, run_id)
}

/// The fleet a record belongs to: a `RunKind::Fleet` record on a
/// `fleet-<name>` channel. `None` for every other run.
fn fleet_of(kind: RunKind, channel_id: Option<&str>) -> Option<&str> {
    (kind == RunKind::Fleet)
        .then_some(channel_id)
        .flatten()
        .and_then(mur_common::fleet::fleet_name_from_channel_id)
}

/// Where a writer records this run: its fleet's tree for a fleet run,
/// `runs/` otherwise.
fn dir_for(mur_home: &Path, kind: RunKind, channel_id: Option<&str>, run_id: &str) -> PathBuf {
    match fleet_of(kind, channel_id) {
        Some(fleet) => fleet_run_dir(mur_home, fleet, run_id),
        None => runs_dir(mur_home).join(run_id),
    }
}

/// The directory a writer records this run in, fixed once when the record is
/// created. Later writes for the same run (heartbeats, step progress, the
/// terminal state) go to THIS directory via [`update_dir`] instead of
/// searching by id, which another fleet's planted copy could win.
pub fn record_dir(
    mur_home: &Path,
    kind: RunKind,
    channel_id: Option<&str>,
    run_id: &str,
) -> PathBuf {
    dir_for(mur_home, kind, channel_id, run_id)
}

/// [`update`] of the record in `dir` (from [`record_dir`]), no id search.
pub fn update_dir<F>(dir: &Path, mutate: F) -> Result<bool>
where
    F: FnOnce(&mut RunState),
{
    update_in(dir, mutate)
}

/// Every fleet name that may own run directories: what `fleet-state/` lists,
/// plus every defined fleet. Inside a sandbox `fleet-state/` itself may not
/// be listable (only its per-fleet children are granted), while `fleets/`
/// is readable — so both are consulted. Invalid names are dropped: a name
/// becomes a path segment.
fn fleet_names(mur_home: &Path) -> Vec<String> {
    let mut names: Vec<String> = [mur_common::paths::FLEET_STATE, mur_common::paths::FLEETS]
        .iter()
        .filter_map(|d| std::fs::read_dir(mur_home.join(d)).ok())
        .flat_map(|it| it.flatten())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| mur_common::fleet::valid_fleet_name(n))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Resolve the directory of an existing run from its id alone: the first
/// fleet whose tree has it, else `runs/<run_id>` (which may not exist).
///
/// For reporting (`mur job status`, `mur_job_status`, heartbeats) only.
/// Never for a decision that trusts the run's files — see the module doc.
pub fn run_dir(mur_home: &Path, run_id: &str) -> PathBuf {
    if !mur_common::paths::is_single_segment(run_id) {
        return runs_dir(mur_home).join(run_id);
    }
    fleet_names(mur_home)
        .into_iter()
        .map(|f| fleet_run_dir(mur_home, &f, run_id))
        .find(|d| d.is_dir())
        .unwrap_or_else(|| runs_dir(mur_home).join(run_id))
}

/// [`run_dir`], or only `fleet`'s tree when the caller knows the fleet. A
/// caller that knows it must pass it: an agent of fleet `a` can create
/// `fleet-state/a/runs/<b's run id>`, and the unscoped search, in name
/// order, would show that copy as fleet `b`'s run.
pub fn run_dir_in(mur_home: &Path, fleet: Option<&str>, run_id: &str) -> PathBuf {
    match fleet {
        Some(f) => fleet_run_dir(mur_home, f, run_id),
        None => run_dir(mur_home, run_id),
    }
}

/// Every directory recorded under `run_id`: each fleet tree that has it,
/// then `runs/`. More than one means the id alone cannot say which run is
/// meant — a mutation by id refuses rather than guess.
pub fn run_dirs_of(mur_home: &Path, run_id: &str) -> Vec<PathBuf> {
    if !mur_common::paths::is_single_segment(run_id) {
        return vec![];
    }
    fleet_names(mur_home)
        .into_iter()
        .map(|f| fleet_run_dir(mur_home, &f, run_id))
        .chain(std::iter::once(runs_dir(mur_home).join(run_id)))
        .filter(|d| d.is_dir())
        .collect()
}

/// `run.json` of an existing run, resolved by [`run_dir`].
pub fn run_path(mur_home: &Path, run_id: &str) -> PathBuf {
    run_dir(mur_home, run_id).join(RUN_FILE)
}

/// Write `run` atomically: serialize to `run.json.tmp`, then rename over
/// `run.json`. A reader therefore never observes a half-written record — the
/// same temp-file-plus-rename discipline `store/yaml.rs` uses.
pub fn save(mur_home: &Path, run: &RunState) -> Result<()> {
    let dir = dir_for(mur_home, run.kind, run.channel_id.as_deref(), &run.run_id);
    save_in(&dir, run)
}

fn save_in(dir: &Path, run: &RunState) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let final_path = dir.join(RUN_FILE);
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

/// The `sidecar.json` rebuild index — the channel, kind, and first event seq
/// of this run's events, all facts known at recording time. Deliberately
/// separate from `run.json`: a corrupt cache must not take the index down
/// with it. See `status_of`'s fallback and `rebuild`'s boundary rule.
pub fn save_sidecar(mur_home: &Path, run_id: &str, sidecar: &Sidecar) -> Result<()> {
    let dir = dir_for(mur_home, sidecar.kind, Some(&sidecar.channel_id), run_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    // Temp-file-plus-rename, same discipline as `save`: a crash mid-write
    // must leave either the previous sidecar or none at all, never a
    // truncated one (a truncated sidecar reads as "no sidecar", so the run
    // silently becomes unrecoverable instead of corrupting).
    let final_path = dir.join(SIDECAR_FILE);
    let tmp_path = dir.join("sidecar.json.tmp");
    let body = serde_json::to_vec_pretty(sidecar).context("serialize sidecar")?;
    {
        let mut f = std::fs::File::create(&tmp_path)
            .with_context(|| format!("create {}", tmp_path.display()))?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename into {}", final_path.display()))
}

/// Read the sidecar. `Ok(None)` when it does not exist — like a missing
/// `run.json`, a never-recorded run is not an error, it is simply not
/// rebuildable.
pub fn load_sidecar(mur_home: &Path, run_id: &str) -> Result<Option<Sidecar>> {
    load_sidecar_in(mur_home, None, run_id)
}

/// [`load_sidecar`] from the directory [`run_dir_in`] resolves.
pub fn load_sidecar_in(
    mur_home: &Path,
    fleet: Option<&str>,
    run_id: &str,
) -> Result<Option<Sidecar>> {
    let path = run_dir_in(mur_home, fleet, run_id).join(SIDECAR_FILE);
    match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
        )),
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
    update_in(&run_dir(mur_home, run_id), mutate)
}

/// [`update`] of a fleet run, read from that fleet's tree only.
///
/// Like [`load_in_fleet`], a linked `run.json` is refused rather than read.
pub fn update_in_fleet<F>(mur_home: &Path, fleet: &str, run_id: &str, mutate: F) -> Result<bool>
where
    F: FnOnce(&mut RunState),
{
    update_at(
        &fleet_run_dir(mur_home, fleet, run_id),
        ReadMode::Trusted,
        mutate,
    )
}

/// How a record is read: as found, or only when it is not a link
/// ([`mur_common::paths::read_unlinked`]).
#[derive(Clone, Copy)]
enum ReadMode {
    AsFound,
    Trusted,
}

fn read_record(dir: &Path, how: ReadMode) -> std::io::Result<Vec<u8>> {
    match how {
        ReadMode::AsFound => std::fs::read(dir.join(RUN_FILE)),
        ReadMode::Trusted => mur_common::paths::read_unlinked(dir, RUN_FILE),
    }
}

fn update_in<F>(dir: &Path, mutate: F) -> Result<bool>
where
    F: FnOnce(&mut RunState),
{
    update_at(dir, ReadMode::AsFound, mutate)
}

fn update_at<F>(dir: &Path, how: ReadMode, mutate: F) -> Result<bool>
where
    F: FnOnce(&mut RunState),
{
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
        let path = dir.join(RUN_FILE);
        let mut run = match read_record(dir, how) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            Ok(bytes) => serde_json::from_slice::<RunState>(&bytes)
                .with_context(|| format!("parse {}", path.display()))?,
        };
        mutate(&mut run);
        // Back where it was read from: a mutation never moves a record.
        save_in(dir, &run)?;
        Ok(true)
    })();

    FileExt::unlock(&lock).ok();
    result
}

/// Read one run record. `Ok(None)` when the file does not exist — a run that
/// was never recorded is not an error, it is a rebuild candidate.
pub fn load(mur_home: &Path, run_id: &str) -> Result<Option<RunState>> {
    load_from(&run_path(mur_home, run_id))
}

/// [`load`] from the directory [`run_dir_in`] resolves. For reporting; a
/// decision that trusts the record uses [`load_in_fleet`].
pub fn load_scoped(mur_home: &Path, fleet: Option<&str>, run_id: &str) -> Result<Option<RunState>> {
    load_from(&run_dir_in(mur_home, fleet, run_id).join(RUN_FILE))
}

/// [`load`] of a fleet run, from that fleet's tree only — for every decision
/// that trusts the record (resume, pause). Never falls back to `runs/`, and
/// refuses a `run.json` that is a link (symlink or hard link), which would
/// read the fleet's trusted path out of a tree every agent can write.
pub fn load_in_fleet(mur_home: &Path, fleet: &str, run_id: &str) -> Result<Option<RunState>> {
    let dir = fleet_run_dir(mur_home, fleet, run_id);
    parse_record(&dir.join(RUN_FILE), read_record(&dir, ReadMode::Trusted))
}

fn load_from(path: &Path) -> Result<Option<RunState>> {
    parse_record(path, std::fs::read(path))
}

fn parse_record(path: &Path, read: std::io::Result<Vec<u8>>) -> Result<Option<RunState>> {
    match read {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
        )),
    }
}

/// Every recorded run id: each fleet's runs, then `runs/`. Missing
/// directories yield nothing: no runs have happened yet, which is not a
/// failure. Failing to list `runs/` itself is still an error.
pub fn list_ids(mur_home: &Path) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for fleet in fleet_names(mur_home) {
        // A fleet tree that cannot be listed (sandbox) hides only that
        // fleet's runs, never the whole list.
        ids.extend(
            list_dir_ids(&mur_common::paths::fleet_runs_dir(mur_home, &fleet)).unwrap_or_default(),
        );
    }
    ids.extend(list_dir_ids(&runs_dir(mur_home))?);
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// The run ids of one fleet, from its tree only.
pub fn list_fleet_ids(mur_home: &Path, fleet: &str) -> Result<Vec<String>> {
    list_dir_ids(&mur_common::paths::fleet_runs_dir(mur_home, fleet))
}

fn list_dir_ids(dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
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
}
