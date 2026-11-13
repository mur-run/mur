//! Atomic read/write of a run's `run.json` and `sidecar.json`. Pure I/O — no
//! policy.
//!
//! A run directory lives in one of four places (#2316):
//!
//! - a fleet run: `<mur_home>/fleet-state/<fleet>/runs/<run_id>/`, inside the
//!   per-fleet sandbox carve-in, so only an agent allowed to run that fleet
//!   can write it;
//! - a run an agent dispatched (`parallel_jobs`, workflows, from inside the
//!   agent's sandbox): `<mur_home>/agents/<agent>/runs/<run_id>/`, inside
//!   that agent's home, so no other agent can write it (#2320). The agent is
//!   named by `MUR_RUN_OWNER`, which the runtime sets on its children;
//! - a sealed agent's `parallel_jobs` run, hosted by `mur-daemon`'s child
//!   (#2379): `<mur_home>/jobs-dispatch/<run_id>/`, which no sandboxed agent
//!   may write, because that child is unsealed;
//! - every other run (the operator's own `mur workflow run`):
//!   `<mur_home>/runs/<run_id>/`, which no sandboxed agent may write.
//!
//! Writers know which one from the record itself (`kind` + `channel_id`).
//! A reader holding only a run id resolves it with [`run_dir`]:
//! `jobs-dispatch/`, then the fleet trees, then the agent trees, then
//! `runs/`. Anything that TRUSTS a fleet run's files
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
/// `jobs-dispatch/` for a non-fleet run hosted by the daemon's `parallel_jobs`
/// dispatch, the dispatching agent's tree for a run started inside an agent,
/// `runs/` otherwise.
fn dir_for(mur_home: &Path, kind: RunKind, channel_id: Option<&str>, run_id: &str) -> PathBuf {
    dir_for_host(
        mur_home,
        kind,
        channel_id,
        process_is_jobs_dispatch_host(),
        process_run_owner().as_deref(),
        run_id,
    )
}

/// [`dir_for`] with the hosted flag and owner passed in, not read from the
/// env, so `store_owner_tests` can drive the daemon-hosted route.
fn dir_for_host(
    mur_home: &Path,
    kind: RunKind,
    channel_id: Option<&str>,
    hosted: bool,
    owner: Option<&str>,
    run_id: &str,
) -> PathBuf {
    if hosted && fleet_of(kind, channel_id).is_none() {
        return mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(mur_home, run_id);
    }
    dir_for_owner(mur_home, kind, channel_id, owner, run_id)
}

/// The agent this process records runs for: `MUR_RUN_OWNER`, set by the
/// runtime on its children. Unit tests see no owner (#2352): `cargo test`
/// run from an agent shell inherits the variable, and the tests that save
/// through [`dir_for`] expect the operator's `runs/` tree. Owner routing is
/// covered by `store_owner_tests`, which drives [`dir_for_owner`] directly.
fn process_run_owner() -> Option<String> {
    if cfg!(test) {
        None
    } else {
        mur_common::paths::run_owner_from_env()
    }
}

/// Is this process the daemon-launched host of a sealed agent's
/// `parallel_jobs` run (#2379)? Then its records go to the daemon's own
/// tree: this process is unsealed, and the requester can write (and plant
/// links in) its own home. Unit tests never are, for the same reason as
/// [`process_run_owner`].
fn process_is_jobs_dispatch_host() -> bool {
    !cfg!(test)
        && std::env::var_os(mur_track::jobs_dispatch_request::JOBS_DISPATCH_HOSTED_ENV)
            .is_some_and(|v| v == "1")
}

/// [`dir_for`] with the owning agent passed in, not read from the env.
fn dir_for_owner(
    mur_home: &Path,
    kind: RunKind,
    channel_id: Option<&str>,
    owner: Option<&str>,
    run_id: &str,
) -> PathBuf {
    match (fleet_of(kind, channel_id), owner) {
        (Some(fleet), _) => fleet_run_dir(mur_home, fleet, run_id),
        (None, Some(agent)) => mur_common::paths::agent_runs_dir(mur_home, agent).join(run_id),
        (None, None) => runs_dir(mur_home).join(run_id),
    }
}

/// Every agent's runs directory that exists. Invalid names are dropped: a
/// name becomes a path segment.
fn agent_run_roots(mur_home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(mur_home.join(mur_common::paths::AGENTS)) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| mur_common::agent_name::validate_agent_name(n).is_ok())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| mur_common::paths::agent_runs_dir(mur_home, &n))
        .filter(|d| d.is_dir())
        .collect()
}

/// Every directory a run id may be recorded in, in search order:
/// `jobs-dispatch/`, each fleet tree, each agent tree, then `runs/`. Not
/// filtered for existence.
fn candidate_dirs(mur_home: &Path, run_id: &str) -> Vec<PathBuf> {
    // First: no agent can write this tree, so a copy an agent planted in its
    // own home or in a fleet tree it belongs to never shadows the daemon's run.
    std::iter::once(mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(
        mur_home, run_id,
    ))
    .chain(
        fleet_names(mur_home)
            .into_iter()
            .map(|f| fleet_run_dir(mur_home, &f, run_id)),
    )
    .chain(
        agent_run_roots(mur_home)
            .into_iter()
            .map(|r| r.join(run_id)),
    )
    .chain(std::iter::once(runs_dir(mur_home).join(run_id)))
    .collect()
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

/// Resolve the directory of an existing run from its id alone: the first of
/// `jobs-dispatch/`, the fleet trees and the agent trees that has it, else
/// `runs/<run_id>` (which may not exist).
///
/// For reporting (`mur job status`, `mur_job_status`, heartbeats) only.
/// Never for a decision that trusts the run's files — see the module doc.
pub fn run_dir(mur_home: &Path, run_id: &str) -> PathBuf {
    if !mur_common::paths::is_single_segment(run_id) {
        return runs_dir(mur_home).join(run_id);
    }
    candidate_dirs(mur_home, run_id)
        .into_iter()
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

/// Every directory recorded under `run_id`: `jobs-dispatch/`, each fleet tree
/// that has it, each agent tree, then `runs/`. More than one means the id alone cannot say which run is
/// meant — a mutation by id refuses rather than guess.
pub fn run_dirs_of(mur_home: &Path, run_id: &str) -> Vec<PathBuf> {
    if !mur_common::paths::is_single_segment(run_id) {
        return vec![];
    }
    candidate_dirs(mur_home, run_id)
        .into_iter()
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

/// Every recorded run id: each fleet's runs, each agent's, then `runs/`. Missing
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
    for root in agent_run_roots(mur_home) {
        // Same rule: one unlistable agent tree hides only that agent's runs.
        ids.extend(list_dir_ids(&root).unwrap_or_default());
    }
    ids.extend(
        list_dir_ids(&mur_home.join(mur_track::jobs_dispatch_request::JOBS_DISPATCH_STATE_DIR))
            .unwrap_or_default(),
    );
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
#[path = "store_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "store_owner_tests.rs"]
mod owner_tests;
