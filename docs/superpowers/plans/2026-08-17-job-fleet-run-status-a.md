# Job / Fleet Run Status — Plan A (observability) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a running job, fleet, or workflow answer "what are you doing right now, and are you actually alive?" — from the CLI and from an agent.

**Architecture:** `execute_dag` (the single substrate behind `parallel_jobs`, `fleet run`, and `workflow run`) writes a per-run cache at `~/.mur/runs/<run_id>/run.json` and ticks a heartbeat into it. `mur_core::run_status::classify()` is the one place run state is derived, returning two independent axes — a stored semantic `State` and a computed `Liveness`. CLI, MCP, and (Plan B) the Hub Panel all render that one output. The cache is rebuildable from the channel event log; the heartbeat is the only field that cannot be rebuilt, and a rebuilt run says so rather than inventing one.

**Tech Stack:** Rust edition 2024, `serde`/`serde_json`, `chrono`, `tokio`, `clap`, `mur_common::lock_file::pid_alive`, `mur_channel::ChannelService`.

**Spec:** `docs/superpowers/specs/2026-08-17-job-fleet-run-status-design.md`

## Global Constraints

- Rust edition 2024. `let` chains are stable (`if let … && let …`).
- **No hardcoded values.** Heartbeat interval and staleness threshold are config keys, never literals at call sites (CLAUDE.md rule 1).
- **Single source file ≤ 800 lines** (CLAUDE.md rule 4). Every file created here is far below it; keep it that way.
- **`classify()` is the only derivation of run state.** Any surface computing its own is a design violation (spec §4).
- **The heartbeat must never be synthesized.** A rebuilt run reports `Liveness::Unknown` (spec §2).
- JSON writes are atomic: temp file + rename, matching `store/yaml.rs`.
- Build/test env for this repo: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Tests run under `cargo nextest`, never bare `cargo test` (bare `cargo test` false-fails ~7 tests). `RUST_MIN_STACK` is already set by the repo's `.cargo/config.toml`.
  - On a disk-constrained machine, prepend `CARGO_TARGET_DIR=<repo>/target` to share the main checkout's build artifacts.
- Brand name in any user-visible string is uppercase **MUR** (CLAUDE.md rule 7). CLI command names stay lowercase.

---

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `mur-core/src/run_status/mod.rs` | Public types (`RunState`, `State`, `Liveness`, `RunStatus`, `RunKind`) and `classify()`. The only derivation point. |
| `mur-core/src/run_status/store.rs` | `runs_dir()`, `run_path()`, atomic `save()`, `load()`, `list_ids()`. Pure I/O, no policy. |
| `mur-core/src/run_status/heartbeat.rs` | The background ticker that stamps `last_heartbeat_at`. |
| `mur-core/src/run_status/rebuild.rs` | Fold `ChannelEvent`s into a `RunState` when `run.json` is missing. |
| `mur-core/src/cmd/job.rs` | `mur job list` / `status` / `stop`. Rendering only. |

**Modified:**

| File | Change |
|---|---|
| `mur-common/src/config.rs` | Add `RunsConfig` section (`heartbeat_interval_secs`, `heartbeat_stale_after_intervals`). |
| `mur-core/src/lib.rs` | `pub mod run_status;` |
| `mur-core/src/executor/dag.rs` | Write the run record at start, spawn the heartbeat, stamp the terminal state. |
| `mur-core/src/executor/jobs.rs` | Add `run_kind` + `run_label`. Its `run_id` is already correct — do not touch it. |
| `mur-core/src/cmd/fleet/run.rs` | Add `run_kind` + `run_label`. `run_id` already correct. |
| `mur-core/src/cmd/fleet/loop_run.rs` | Add `run_kind` + `run_label`. `run_id` already correct, and its uuid nonce is load-bearing — the unattended guarded loop. |
| `mur-core/src/cmd/workflow.rs` | Add `run_kind` + `run_label`. `run_id` already correct. |
| `mur-core/src/cli/mod.rs` | `Job { action: JobAction }` command. |
| `mur-core/src/dispatch.rs` | Dispatch `Commands::Job`. |
| `mur-mcp-server/src/tools.rs` | `mur_job_status` tool definition + dispatch arm. |

---

## Task 1: Run state types and atomic store

**Files:**
- Create: `mur-core/src/run_status/mod.rs`
- Create: `mur-core/src/run_status/store.rs`
- Modify: `mur-core/src/lib.rs`
- Test: inline `#[cfg(test)]` in `mur-core/src/run_status/store.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `run_status::{RunState, StepState, BlockedOn, State, Liveness, RunKind, RUN_SCHEMA}`; `run_status::store::{runs_dir, run_path, save, load, list_ids}`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/run_status/store.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{RunKind, RunState, State, RUN_SCHEMA};

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
    fn save_leaves_no_temp_file_behind() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &sample("run-b")).unwrap();
        let entries: Vec<_> = std::fs::read_dir(runs_dir(tmp.path()).join("run-b"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["run.json".to_string()], "temp file left behind");
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
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::store
```

Expected: FAIL to compile — `run_status` module does not exist.

- [ ] **Step 3: Write the types**

Create `mur-core/src/run_status/mod.rs`:

```rust
//! Run status: the one place a job / fleet / workflow run's state is derived.
//!
//! `~/.mur/runs/<run_id>/run.json` is a CACHE, not a source of truth — every
//! field except `last_heartbeat_at` is derivable from the run's channel event
//! log (see `rebuild`). When the two disagree, the channel wins and the cache
//! is rebuilt. This mirrors `mur_common::channel::Channel`, whose own doc
//! comment calls it "a cache of state derivable from the event log".

pub mod heartbeat;
pub mod rebuild;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version of `run.json`. Bump when a field's meaning changes.
pub const RUN_SCHEMA: u32 = 1;

/// Which entry point produced this run. All three go through `execute_dag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    Job,
    Fleet,
    Workflow,
}

/// The semantic state. STORED — written by the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Running,
    Blocked,
    Done,
    Failed,
    Stopped,
}

impl State {
    /// True when the run has finished and no process is expected to remain.
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Stopped)
    }
}

/// Whether the run is actually progressing. DERIVED — never stored.
///
/// Persisting this would recreate the lying-cache failure this module exists
/// to remove: a stale `running` on disk is exactly what made a dead
/// delegation look healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Liveness {
    /// Process up, heartbeat fresh.
    Alive,
    /// Process up, heartbeat expired — the run is not moving. This is the
    /// state that previously had no name and cost a long manual investigation.
    Stalled,
    /// Process gone. Paired with a non-terminal `State`, this is a crash.
    Dead,
    /// Process up, but the record was rebuilt from the channel and carries no
    /// heartbeat. Reporting this is required; synthesizing one is forbidden.
    Unknown,
    /// The run finished. A finished run's absent process is not a fault.
    #[serde(rename = "n/a")]
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Set while a run waits on a human decision. Plan B populates this; Plan A
/// only carries and renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedOn {
    pub hitl_id: String,
    pub summary: String,
    pub since: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub schema: u32,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    pub kind: RunKind,
    pub label: String,
    /// PID of the orchestrator process (the one inside `execute_dag`), not of
    /// any delegated agent.
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    /// The ONLY field that cannot be rebuilt from the channel. `None` means
    /// "rebuilt" and yields `Liveness::Unknown`, never a guess.
    #[serde(default)]
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub state: State,
    #[serde(default)]
    pub steps: Vec<StepState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<BlockedOn>,
    pub binary_version: String,
    pub build_sha: String,
}
```

Add to `mur-core/src/lib.rs`, in alphabetical position among the existing `pub mod` lines:

```rust
pub mod run_status;
```

- [ ] **Step 4: Write the store**

Replace the contents of `mur-core/src/run_status/store.rs` with this, keeping the test module from Step 1 at the bottom:

```rust
//! Atomic read/write of `~/.mur/runs/<run_id>/run.json`. Pure I/O — no policy.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
```

Create empty placeholder modules so `mod.rs` compiles — `mur-core/src/run_status/heartbeat.rs` and `mur-core/src/run_status/rebuild.rs`, each containing only:

```rust
// Filled in by a later task.
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::store
```

Expected: PASS, 5 tests.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/run_status/ mur-core/src/lib.rs
git commit -m "feat(run-status): run state types and atomic run.json store"
```

---

## Task 2: `classify()` — two axes, never flattened

**Files:**
- Modify: `mur-core/src/run_status/mod.rs`
- Modify: `mur-common/src/config.rs`
- Test: inline `#[cfg(test)]` in `mur-core/src/run_status/mod.rs`

**Interfaces:**
- Consumes: `run_status::{RunState, State, Liveness}` (Task 1).
- Produces: `run_status::RunStatus { state: State, liveness: Liveness, run: RunState }`; `run_status::classify(run: RunState, now: DateTime<Utc>, stale_after: chrono::Duration) -> RunStatus`; `run_status::stale_after(cfg: &RunsConfig) -> chrono::Duration`; `run_status::status_of(mur_home: &Path, run_id: &str) -> anyhow::Result<Option<RunStatus>>`; `mur_common::config::RunsConfig { heartbeat_interval_secs: u64, heartbeat_stale_after_intervals: u32 }` reachable as `config.runs`.

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/run_status/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const STALE_AFTER_SECS: i64 = 30;

    fn stale_after() -> chrono::Duration {
        chrono::Duration::seconds(STALE_AFTER_SECS)
    }

    fn run(state: State, pid: u32, heartbeat_age_secs: Option<i64>, now: DateTime<Utc>) -> RunState {
        RunState {
            schema: RUN_SCHEMA,
            run_id: "r".into(),
            channel_id: None,
            kind: RunKind::Job,
            label: "l".into(),
            pid,
            started_at: now - chrono::Duration::seconds(600),
            last_heartbeat_at: heartbeat_age_secs.map(|s| now - chrono::Duration::seconds(s)),
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: "0.0.0-test".into(),
            build_sha: "deadbee".into(),
        }
    }

    /// A pid that is certainly not running: spawn a trivial child, wait for it,
    /// and reuse its reaped pid. Checking a literal pid would be a guess.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let pid = child.id();
        child.wait().expect("reap child");
        pid
    }

    #[test]
    fn every_state_liveness_cell() {
        let now = Utc::now();
        let live = std::process::id();
        let dead = dead_pid();

        // Non-terminal + live process + fresh heartbeat => alive.
        for state in [State::Running, State::Blocked] {
            let s = classify(run(state, live, Some(1), now), now, stale_after());
            assert_eq!(s.state, state);
            assert_eq!(s.liveness, Liveness::Alive, "{state:?} with a fresh beat");
        }

        // Non-terminal + live process + expired heartbeat => stalled.
        for state in [State::Running, State::Blocked] {
            let s = classify(
                run(state, live, Some(STALE_AFTER_SECS + 1), now),
                now,
                stale_after(),
            );
            assert_eq!(s.liveness, Liveness::Stalled, "{state:?} with a dead beat");
        }

        // Non-terminal + no process => dead, whatever the heartbeat said.
        for state in [State::Running, State::Blocked] {
            let s = classify(run(state, dead, Some(1), now), now, stale_after());
            assert_eq!(s.liveness, Liveness::Dead, "{state:?} with no process");
        }

        // Non-terminal + live process + rebuilt (no heartbeat) => unknown.
        let s = classify(run(State::Running, live, None, now), now, stale_after());
        assert_eq!(s.liveness, Liveness::Unknown);

        // Terminal => n/a regardless of process or heartbeat.
        for state in [State::Done, State::Failed, State::Stopped] {
            for pid in [live, dead] {
                for beat in [Some(1), Some(STALE_AFTER_SECS + 1), None] {
                    let s = classify(run(state, pid, beat, now), now, stale_after());
                    assert_eq!(
                        s.liveness,
                        Liveness::NotApplicable,
                        "{state:?} must not report liveness"
                    );
                }
            }
        }
    }

    /// Negative control for the reported defect. A test that only asserts a
    /// live process reports `alive` proves nothing: freezing the heartbeat
    /// while the process stays up MUST flip the verdict.
    #[test]
    fn frozen_heartbeat_flips_running_to_stalled() {
        let now = Utc::now();
        let live = std::process::id();
        let fresh = classify(run(State::Running, live, Some(1), now), now, stale_after());
        let frozen = classify(
            run(State::Running, live, Some(STALE_AFTER_SECS + 1), now),
            now,
            stale_after(),
        );
        assert_eq!(fresh.liveness, Liveness::Alive);
        assert_eq!(frozen.liveness, Liveness::Stalled);
        assert_ne!(fresh.liveness, frozen.liveness, "heartbeat is not consulted");
    }

    /// Negative control: a killed orchestrator must never keep reporting
    /// `running`/`alive`. `state` stays `running` because nothing wrote a
    /// terminal state — that pair IS what a crash looks like.
    #[test]
    fn killed_orchestrator_reports_dead_not_running() {
        let now = Utc::now();
        let s = classify(run(State::Running, dead_pid(), Some(1), now), now, stale_after());
        assert_eq!(s.state, State::Running, "no terminal state was ever written");
        assert_eq!(s.liveness, Liveness::Dead);
        assert!(!s.state.is_terminal(), "a crashed run is not finished");
    }

    #[test]
    fn liveness_is_never_persisted() {
        let now = Utc::now();
        let json = serde_json::to_string(&run(State::Running, std::process::id(), Some(1), now)).unwrap();
        assert!(
            !json.contains("liveness"),
            "liveness must be derived, never stored: {json}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::tests
```

Expected: FAIL to compile — `classify` and `RunStatus` are not defined.

- [ ] **Step 3: Write `classify`**

Append to `mur-core/src/run_status/mod.rs`, before the test module:

```rust
/// A run's state as reported to any surface. `state` is read from disk;
/// `liveness` is computed here and nowhere else.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunStatus {
    pub state: State,
    pub liveness: Liveness,
    pub run: RunState,
}

/// Derive a run's reportable status. THE single derivation point (spec §4).
///
/// `now` and `stale_after` are parameters rather than ambient reads so the
/// table test can address every cell without sleeping.
pub fn classify(run: RunState, now: DateTime<Utc>, stale_after: chrono::Duration) -> RunStatus {
    let liveness = if run.state.is_terminal() {
        Liveness::NotApplicable
    } else if !mur_common::lock_file::pid_alive(run.pid) {
        Liveness::Dead
    } else {
        match run.last_heartbeat_at {
            // Rebuilt from the channel: the heartbeat is not recoverable and
            // must not be invented.
            None => Liveness::Unknown,
            Some(beat) if now.signed_duration_since(beat) <= stale_after => Liveness::Alive,
            Some(_) => Liveness::Stalled,
        }
    };
    RunStatus {
        state: run.state,
        liveness,
        run,
    }
}

/// The heartbeat age past which a live process counts as `stalled`.
///
/// Derived here, once, so no surface recomputes `interval × intervals` and
/// drifts from the others — the same class of bug as two renderers disagreeing
/// about one fact.
pub fn stale_after(cfg: &mur_common::config::RunsConfig) -> chrono::Duration {
    chrono::Duration::seconds(
        (cfg.heartbeat_interval_secs * u64::from(cfg.heartbeat_stale_after_intervals)) as i64,
    )
}

/// Load a run and classify it against the configured staleness threshold and
/// the current clock. `Ok(None)` when no such run was recorded.
///
/// Every surface calls THIS, not `classify` directly: it is the only place the
/// config load, the clock read, and the derivation are assembled, so no caller
/// can assemble them differently. `classify` stays pure so the table test can
/// address every cell without a clock or a config file.
pub fn status_of(mur_home: &std::path::Path, run_id: &str) -> anyhow::Result<Option<RunStatus>> {
    let Some(record) = store::load(mur_home, run_id)? else {
        return Ok(None);
    };
    let cfg = mur_common::config::Config::load_or_default(mur_home);
    Ok(Some(classify(
        record,
        Utc::now(),
        stale_after(&cfg.runs),
    )))
}
```

- [ ] **Step 4: Add the config section**

In `mur-common/src/config.rs`, add a field to `pub struct Config` following the existing pattern (each field carries `#[serde(default)]`):

```rust
    #[serde(default)]
    pub runs: RunsConfig,
```

And add the section type next to the other `*Config` structs in the same file:

```rust
/// Run-status heartbeat tuning. Both values are config, never literals at a
/// call site: the right interval depends on how long the machine's steps take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunsConfig {
    /// How often `execute_dag` stamps `last_heartbeat_at`.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    /// How many missed intervals before a live process counts as `stalled`.
    /// Three tolerates one lost tick plus scheduling jitter without calling a
    /// healthy run dead.
    #[serde(default = "default_heartbeat_stale_after_intervals")]
    pub heartbeat_stale_after_intervals: u32,
}

fn default_heartbeat_interval_secs() -> u64 {
    10
}

fn default_heartbeat_stale_after_intervals() -> u32 {
    3
}

impl Default for RunsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            heartbeat_stale_after_intervals: default_heartbeat_stale_after_intervals(),
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-common config
```

Expected: PASS. The `run_status` filter runs Task 1's 5 store tests plus the 4 new ones.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/run_status/mod.rs mur-common/src/config.rs
git commit -m "feat(run-status): classify() derives liveness from state and heartbeat"
```

---

## Task 3: Heartbeat ticker

**Files:**
- Modify: `mur-core/src/run_status/heartbeat.rs`
- Test: inline `#[cfg(test)]` in `mur-core/src/run_status/heartbeat.rs`

**Interfaces:**
- Consumes: `run_status::store::{load, save}`, `run_status::RunState` (Task 1); `mur_common::config::RunsConfig` (Task 2).
- Produces: `run_status::heartbeat::Heartbeat` with `Heartbeat::spawn(mur_home: PathBuf, run_id: String, interval: Duration) -> Heartbeat` and `Heartbeat::stop(self)`; `run_status::heartbeat::beat_once(mur_home: &Path, run_id: &str, now: DateTime<Utc>) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Replace `mur-core/src/run_status/heartbeat.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::store;
    use crate::run_status::{RunKind, RunState, State, RUN_SCHEMA};

    fn seed(mur_home: &std::path::Path, run_id: &str) {
        store::save(
            mur_home,
            &RunState {
                schema: RUN_SCHEMA,
                run_id: run_id.into(),
                channel_id: None,
                kind: RunKind::Job,
                label: "l".into(),
                pid: std::process::id(),
                started_at: chrono::Utc::now(),
                last_heartbeat_at: None,
                state: State::Running,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".into(),
                build_sha: "deadbee".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn beat_once_stamps_the_heartbeat_and_touches_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");
        let before = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(before.last_heartbeat_at.is_none());

        let now = chrono::Utc::now();
        beat_once(tmp.path(), "r", now).unwrap();

        let after = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(after.last_heartbeat_at, Some(now));
        assert_eq!(after.state, before.state, "heartbeat must not change state");
        assert_eq!(after.label, before.label);
    }

    /// A run whose record is gone must not resurrect it. Writing a fresh
    /// record here would manufacture a run that no longer exists.
    #[test]
    fn beat_once_on_a_missing_run_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        beat_once(tmp.path(), "ghost", chrono::Utc::now()).unwrap();
        assert!(store::load(tmp.path(), "ghost").unwrap().is_none());
    }

    /// A terminal run's heartbeat must stop moving — otherwise a finished run
    /// would look perpetually fresh.
    #[test]
    fn beat_once_skips_terminal_runs() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");
        let mut run = store::load(tmp.path(), "r").unwrap().unwrap();
        run.state = State::Done;
        store::save(tmp.path(), &run).unwrap();

        beat_once(tmp.path(), "r", chrono::Utc::now()).unwrap();

        let after = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(after.last_heartbeat_at.is_none(), "terminal run got a beat");
    }

    #[tokio::test]
    async fn spawned_ticker_beats_then_stops() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");

        let hb = Heartbeat::spawn(
            tmp.path().to_path_buf(),
            "r".into(),
            std::time::Duration::from_millis(20),
        );
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let while_running = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(while_running.last_heartbeat_at.is_some(), "ticker never beat");

        hb.stop();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let after_stop = store::load(tmp.path(), "r").unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let later = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(
            after_stop.last_heartbeat_at, later.last_heartbeat_at,
            "ticker kept beating after stop"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::heartbeat
```

Expected: FAIL to compile — `beat_once` and `Heartbeat` are not defined.

- [ ] **Step 3: Write the implementation**

Put this above the test module in `mur-core/src/run_status/heartbeat.rs`:

```rust
//! The heartbeat ticker.
//!
//! `last_heartbeat_at` is the one field `rebuild` cannot recover, which is
//! precisely why it is worth writing: it is the only evidence that separates
//! "this process is up" from "this run is moving".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::store;

/// Stamp one beat. Silently does nothing when the record is missing (the run
/// is gone — do not resurrect it) or terminal (a finished run must stop
/// looking fresh).
pub fn beat_once(mur_home: &Path, run_id: &str, now: DateTime<Utc>) -> Result<()> {
    let Some(mut run) = store::load(mur_home, run_id)? else {
        return Ok(());
    };
    if run.state.is_terminal() {
        return Ok(());
    }
    run.last_heartbeat_at = Some(now);
    store::save(mur_home, &run)
}

/// Handle to a background ticker. Dropping it also stops the ticker, so a
/// panicking executor cannot leave a run beating forever.
pub struct Heartbeat {
    stop: Arc<AtomicBool>,
}

impl Heartbeat {
    /// Start beating `run_id` every `interval` until `stop` (or drop).
    pub fn spawn(mur_home: PathBuf, run_id: String, interval: std::time::Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick fires immediately; that first beat is wanted, so
            // a run is never briefly indistinguishable from a rebuilt one.
            loop {
                ticker.tick().await;
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                // A failed beat is not fatal to the run it is observing.
                let _ = beat_once(&mur_home, &run_id, Utc::now());
            }
        });
        Self { stop }
    }

    /// Stop beating. Idempotent.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::heartbeat
```

Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/run_status/heartbeat.rs
git commit -m "feat(run-status): heartbeat ticker, skipping missing and terminal runs"
```

---

## Task 4: Wire the run lifecycle into `execute_dag`

**Files:**
- Modify: `mur-core/src/executor/dag.rs:848-860` (function head) and its terminal-status path
- Modify: `mur-core/src/executor/jobs.rs:141`
- Modify: `mur-core/src/cmd/fleet/run.rs:392`
- Modify: `mur-core/src/cmd/fleet/loop_run.rs:587`
- Modify: `mur-core/src/cmd/workflow.rs:211`
- Test: inline `#[cfg(test)]` in `mur-core/src/executor/dag.rs`

**Interfaces:**
- Consumes: `run_status::{RunState, RunKind, State, RUN_SCHEMA}`, `run_status::store::save`, `run_status::heartbeat::Heartbeat` (Tasks 1–3); `mur_common::config::RunsConfig` (Task 2).
- Produces: `DagExecOptions.run_kind: Option<RunKind>` and `DagExecOptions.run_label: String`; a `run.json` present for the lifetime of every run with a non-empty `run_id`.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `mur-core/src/executor/dag.rs`:

```rust
    /// A run with an id must be observable from disk while it executes, and
    /// must land on a terminal state when it finishes. Without this, a
    /// timeout is the only signal a caller ever gets — which is the defect.
    #[tokio::test]
    async fn execute_dag_records_and_finalizes_a_run() {
        use crate::run_status::{State, store};

        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let procedure = Procedure {
            steps: vec![Step {
                id: "s1".into(),
                depends_on: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let opts = DagExecOptions {
            run_id: "run-under-test".into(),
            run_kind: Some(crate::run_status::RunKind::Workflow),
            run_label: "test run".into(),
            ..Default::default()
        };

        let _ = execute_dag(mur_home, "test-skill", &procedure, &opts).await;

        let run = store::load(mur_home, "run-under-test")
            .unwrap()
            .expect("execute_dag never wrote run.json");
        assert_eq!(run.run_id, "run-under-test");
        assert_eq!(run.pid, std::process::id(), "must record the orchestrator pid");
        assert!(
            run.state.is_terminal(),
            "run left non-terminal after execute_dag returned: {:?}",
            run.state
        );
    }

    /// An empty `run_id` is the legacy default. It must not create a
    /// directory called "" under runs/.
    #[tokio::test]
    async fn execute_dag_without_a_run_id_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let procedure = Procedure {
            steps: vec![Step {
                id: "s1".into(),
                depends_on: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let opts = DagExecOptions::default();

        let _ = execute_dag(tmp.path(), "test-skill", &procedure, &opts).await;

        assert!(
            crate::run_status::store::list_ids(tmp.path()).unwrap().is_empty(),
            "recorded a run for an empty run_id"
        );
    }
```

> If `Step`/`Procedure` in this crate do not implement `Default`, build them with the same literal the neighbouring tests in this module already use — copy that construction rather than adding a `Default` impl.

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core execute_dag_records
```

Expected: FAIL to compile — `DagExecOptions` has no `run_kind` / `run_label`.

- [ ] **Step 3: Extend `DagExecOptions`**

In `mur-core/src/executor/dag.rs`, add two fields to `pub struct DagExecOptions<'a>` (after `run_id`, so the run identity stays together):

```rust
    /// What kind of run this is, for `~/.mur/runs/<run_id>/run.json`. `None`
    /// (or an empty `run_id`) means "do not record" — the legacy path.
    pub run_kind: Option<crate::run_status::RunKind>,
    /// Human-readable label for the run, shown by `mur job list`.
    pub run_label: String,
```

And to its `impl Default`:

```rust
            run_kind: None,
            run_label: String::new(),
```

- [ ] **Step 4: Record, beat, and finalize**

In `execute_dag`, insert this AFTER the empty-procedure guard that returns at
`dag.rs:864-874` — not right after `build_dag`. A procedure with no nodes
returns immediately and has nothing to observe, so it must not leave a run
record behind that no one will ever finalize:

```rust
    // Record the run so it can be queried while it executes. A run is only
    // recorded when it has both an id and a kind; the legacy callers that
    // pass neither behave exactly as before.
    let recorded = (!opts.run_id.is_empty()).then(|| opts.run_kind).flatten();
    let mut heartbeat = if let Some(kind) = recorded {
        let cfg = mur_common::config::Config::load_or_default(mur_home);
        let now = chrono::Utc::now();
        let record = crate::run_status::RunState {
            schema: crate::run_status::RUN_SCHEMA,
            run_id: opts.run_id.clone(),
            channel_id: opts.channel_id.clone(),
            kind,
            label: opts.run_label.clone(),
            pid: std::process::id(),
            started_at: now,
            last_heartbeat_at: Some(now),
            state: crate::run_status::State::Running,
            steps: vec![],
            blocked_on: None,
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            build_sha: mur_common::build::SHORT_SHA.to_string(),
        };
        crate::run_status::store::save(mur_home, &record)?;
        Some(crate::run_status::heartbeat::Heartbeat::spawn(
            mur_home.to_path_buf(),
            opts.run_id.clone(),
            std::time::Duration::from_secs(cfg.runs.heartbeat_interval_secs),
        ))
    } else {
        None
    };
```

> Both paths are verified against `main`: `Config::load_or_default(&Path) -> Self` is at `mur-common/src/config.rs:445`, and `mur_common::build::SHORT_SHA` is at `mur-common/src/build.rs:6` — the same constant `mur-agent-runtime/src/supervisor.rs:605` writes into `LockFile.build_sha`. Use them as written.

**Finalizing: there are three exits, and the codebase already names the hook.**

`execute_dag` returns a `PipelineOutput` at four places: `dag.rs:865` (the
empty-procedure guard, which is BEFORE the recording insertion above and so
never has a run to finalize), and `dag.rs:1030`, `dag.rs:1076`, `dag.rs:1111`.
Those last three each already call `emit_final(...)` immediately beforehand,
and the closure's own comment states the invariant: *"Closure for terminal
StateChange — call before each PipelineOutput return."*

Finalize the run at those same three points. Do NOT put the logic inside
`emit_final` itself: it is a `Fn` closure called three times, whereas stopping
the heartbeat consumes the `Heartbeat` and must be awaited.

Add this async helper as a free function in the same module:

```rust
/// Stop the run's heartbeat and stamp its terminal state.
///
/// The stop MUST be awaited before the terminal save. `Heartbeat::stop` is
/// async because flipping its flag is not enough: a beat already inside
/// `beat_once` has passed the flag check, and its read-modify-write would
/// clobber the terminal state back to `running` with a fresh heartbeat — a
/// finished run reported alive forever, which is the exact failure this
/// module exists to prevent. Awaiting guarantees any in-flight beat lands
/// BEFORE this save, so the terminal write wins.
///
/// Mirrors `emit_final`: call it before every `PipelineOutput` return that
/// can be reached once a run has been recorded.
async fn finalize_run(
    mur_home: &std::path::Path,
    run_id: &str,
    recorded: bool,
    heartbeat: &mut Option<crate::run_status::heartbeat::Heartbeat>,
    failed: bool,
) {
    if !recorded {
        return;
    }
    if let Some(hb) = heartbeat.take() {
        hb.stop().await;
    }
    // `update` holds an exclusive lock across load-modify-save. A bare
    // load/save pair here would race `mur job stop` in another process, which
    // does the same read-modify-write on the same file.
    let _ = crate::run_status::store::update(mur_home, run_id, |record| {
        record.state = if failed {
            crate::run_status::State::Failed
        } else {
            crate::run_status::State::Done
        };
    });
}
```

Then, at each of the three sites, immediately after the existing
`emit_final(...)` call, add the matching finalize with the SAME `failed`
argument that `emit_final` was given:

```rust
        emit_final(true);
        finalize_run(mur_home, &opts.run_id, recorded.is_some(), &mut heartbeat, true).await;
```

and at `dag.rs:1110`:

```rust
    emit_final(overall_exit_code != 0);
    finalize_run(
        mur_home,
        &opts.run_id,
        recorded.is_some(),
        &mut heartbeat,
        overall_exit_code != 0,
    )
    .await;
```

Declare the heartbeat binding as `let mut heartbeat = ...` so `take()` works,
and note that `finalize_run` is idempotent: the second call finds `None` and a
record already terminal, and writes the same value.

> Pairing `finalize_run` with `emit_final` is deliberate. Any future exit added
> to this function must already call `emit_final` per the existing comment, so
> the run-status stamp travels with an invariant the codebase enforces rather
> than depending on someone remembering a second, separate rule.

- [ ] **Step 5: Add `run_kind` and `run_label` at all four entry points — and do NOT touch `run_id`**

`execute_dag` has exactly four callers, and **all four already set a real,
deliberate `run_id`.** Verified against `main`:

| Call site | existing `run_id` — LEAVE IT ALONE | add `run_kind` | add `run_label` |
|---|---|---|---|
| `mur-core/src/executor/jobs.rs:135` | `format!("run-{}", uuid::Uuid::now_v7())` | `Job` | `format!("{} parallel job(s)", jobs.len())` |
| `mur-core/src/cmd/fleet/run.rs:377` | `run-{uuid_v7}`, bound to a local `run_id` and reused later for job stamping | `Fleet` | `format!("fleet {}", fleet.name)` |
| `mur-core/src/cmd/fleet/loop_run.rs:582` | `format!("loop-{}-{}-{}", name, uuid::Uuid::now_v7(), iteration)` | `Fleet` | `format!("fleet {name} iter {iteration}")` |
| `mur-core/src/cmd/workflow.rs:208` | `format!("run-{}", uuid::Uuid::now_v7())` | `Workflow` | `matched.manifest.name.clone()` |

**Do not "improve" any of these ids into a deterministic form.** The uuid nonce
in `loop_run.rs` is load-bearing and its own comment says why: *"uuid nonce so
concurrent `--loop` runs don't collide on the channel's idempotency-key dedup"*.
Replacing it would reintroduce exactly the collision that comment prevents. The
ids are already correct; this task only adds the two new fields.

Each edit is therefore two added lines on an existing `DagExecOptions` literal.
For `mur-core/src/executor/jobs.rs`:

```rust
        run_kind: Some(crate::run_status::RunKind::Job),
        run_label: format!("{} parallel job(s)", jobs.len()),
```

and for `mur-core/src/cmd/fleet/loop_run.rs`:

```rust
        run_kind: Some(crate::run_status::RunKind::Fleet),
        run_label: format!("fleet {name} iter {iteration}"),
```

`loop_run.rs` is the site that matters most in practice: it is the guarded loop
that runs unattended, so it is the path an operator is least able to watch and
most needs to query.

> If a label expression does not compile because a variable is not in scope
> under that exact name, use the equivalent one that is — the label is a
> human-readable string, not a contract. Do not introduce new state to build one.

- [ ] **Step 6: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core execute_dag
```

Expected: PASS, including the two new tests.

- [ ] **Step 7: Run the full suite — this task changes a shared execution path**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run --workspace
```

Expected: PASS. Baseline before this plan was `7324 passed, 0 failed, 30 skipped`; the count should only grow.

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/executor/dag.rs mur-core/src/executor/jobs.rs mur-core/src/cmd/fleet/run.rs
git commit -m "feat(run-status): execute_dag records, beats, and finalizes every identified run"
```

---

## Task 5: Rebuild a run record from the channel

**Files:**
- Modify: `mur-core/src/run_status/rebuild.rs`
- Test: inline `#[cfg(test)]` in `mur-core/src/run_status/rebuild.rs`

**Interfaces:**
- Consumes: `run_status::{RunState, RunKind, State, StepState, RUN_SCHEMA}` (Task 1); `mur_channel::ChannelService::{open, load_events}`; `mur_common::channel::{ChannelEvent, EventKind}`.
- Produces: `run_status::rebuild::from_channel(mur_home: &Path, run_id: &str, channel_id: &str) -> Result<Option<RunState>>`.

- [ ] **Step 1: Write the failing test**

Replace `mur-core/src/run_status/rebuild.rs` with only this test module for now:

```rust
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
        assert!(from_channel(tmp.path(), "run-x", "no-such-channel").unwrap().is_none());
    }

    /// Helper: write a channel whose events describe a delegation that moved
    /// to `failed`. Build it with `mur_channel::ChannelService` rather than by
    /// hand-writing JSONL, so the test breaks if the event contract changes.
    fn seed_channel_with_a_failed_delegation(mur_home: &std::path::Path) -> String {
        use mur_common::channel::{ChannelActor, EventKind};
        let svc = mur_channel::ChannelService::open(mur_home).unwrap();
        let id = svc
            .create("rebuild-test", ChannelActor::User)
            .expect("create channel");
        svc.append_event(
            &id,
            ChannelActor::System,
            EventKind::Delegation,
            serde_json::json!({ "step_id": "s1", "target_agent": "pm" }),
            None,
        )
        .unwrap();
        svc.append_event(
            &id,
            ChannelActor::System,
            EventKind::StateChange,
            serde_json::json!({ "from": "working", "to": "failed" }),
            None,
        )
        .unwrap();
        id
    }
}
```

> `ChannelService::create` / `append_event` signatures must be read from `mur-channel/src/service.rs` before writing this helper — match them exactly rather than adapting the service to the test.

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::rebuild
```

Expected: FAIL to compile — `from_channel` is not defined.

- [ ] **Step 3: Write the rebuild**

Put this above the test module in `mur-core/src/run_status/rebuild.rs`:

```rust
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
                if let Some(id) = ev.payload.get("step_id").and_then(|v| v.as_str()) {
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
                // The executor writes channel state as a `to` field; map only
                // the values it actually emits, and leave anything else alone
                // rather than inventing a state.
                match ev.payload.get("to").and_then(|v| v.as_str()) {
                    Some("failed") => state = State::Failed,
                    Some("completed") | Some("done") => state = State::Done,
                    Some("input-required") => state = State::Blocked,
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
        kind: RunKind::Workflow,
        label: format!("rebuilt from {channel_id}"),
        // No orchestrator process is known: the record was reconstructed after
        // the fact. `pid: 0` never matches a live process, so a rebuilt
        // non-terminal run reads as `dead` rather than as healthy.
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
```

> Confirm the `StateChange` payload's actual key and values by reading how `dag.rs` writes them (`grep -n "StateChange" mur-core/src/executor/dag.rs`), and match those strings exactly. The observed production event was `{"from":"working","to":"failed"}`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core run_status::rebuild
```

Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/run_status/rebuild.rs
git commit -m "feat(run-status): rebuild a run record from its channel, heartbeat unknown"
```

---

## Task 6: `mur job` CLI

**Files:**
- Create: `mur-core/src/cmd/job.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod job;`)
- Modify: `mur-core/src/cli/mod.rs` (add the `Job` command and `JobAction` enum)
- Modify: `mur-core/src/dispatch.rs` (add the dispatch arm)
- Test: inline `#[cfg(test)]` in `mur-core/src/cmd/job.rs`

**Interfaces:**
- Consumes: `run_status::{classify, RunStatus, State, Liveness}`, `run_status::store::{list_ids, load, save}` (Tasks 1–2).
- Produces: `cmd::job::{JobAction, run}`; `cmd::job::visible_in_list(status: &RunStatus) -> bool`.

- [ ] **Step 1: Write the failing test**

First register the module, so the test below is actually compiled and can fail
for the right reason. A test file the crate never includes reports "no tests to
run", which is not a red — it is silence. Add to `mur-core/src/cmd/mod.rs`, in
alphabetical position:

```rust
pub mod job;
```

Then create `mur-core/src/cmd/job.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{Liveness, RunKind, RunState, State, RUN_SCHEMA, classify};

    fn status(state: State, pid: u32, beat: Option<i64>) -> crate::run_status::RunStatus {
        let now = chrono::Utc::now();
        classify(
            RunState {
                schema: RUN_SCHEMA,
                run_id: "r".into(),
                channel_id: None,
                kind: RunKind::Job,
                label: "l".into(),
                pid,
                started_at: now,
                last_heartbeat_at: beat.map(|s| now - chrono::Duration::seconds(s)),
                state,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".into(),
                build_sha: "deadbee".into(),
            },
            now,
            chrono::Duration::seconds(30),
        )
    }

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    /// A crashed run — `running` on disk with no process — is the single most
    /// important row in the list. Filtering it out as "not running" would
    /// hide exactly what the operator came to find.
    #[test]
    fn crashed_run_stays_visible_in_the_default_list() {
        let s = status(State::Running, dead_pid(), Some(1));
        assert_eq!(s.liveness, Liveness::Dead);
        assert!(visible_in_list(&s), "a crashed run was filtered out of the list");
    }

    #[test]
    fn unfinished_runs_are_visible_and_finished_ones_are_not() {
        let live = std::process::id();
        assert!(visible_in_list(&status(State::Running, live, Some(1))));
        assert!(visible_in_list(&status(State::Blocked, live, Some(1))));
        assert!(visible_in_list(&status(State::Running, live, Some(999))));
        for terminal in [State::Done, State::Failed, State::Stopped] {
            assert!(
                !visible_in_list(&status(terminal, live, Some(1))),
                "{terminal:?} should be hidden without --all"
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core cmd::job
```

Expected: FAIL to compile — `visible_in_list` is not defined.

- [ ] **Step 3: Write the command**

Put this above the test module in `mur-core/src/cmd/job.rs`:

```rust
//! `mur job` — query and stop runs. Rendering only: every verdict comes from
//! `run_status::classify`, which is the sole derivation point (spec §4).

use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::run_status::{Liveness, RunStatus, State, classify, store};

#[derive(Subcommand, Debug)]
pub enum JobAction {
    /// List runs. Hides cleanly finished runs unless `--all`.
    List {
        /// Include runs that finished, failed, or were stopped.
        #[arg(long)]
        all: bool,
    },
    /// Show one run in detail.
    Status {
        /// Run id (from `mur job list`).
        run_id: String,
    },
    /// Mark a run stopped and signal its orchestrator.
    Stop {
        /// Run id (from `mur job list`).
        run_id: String,
    },
}

/// Whether a run appears in `mur job list` without `--all`.
///
/// A crashed run — `State::Running` with `Liveness::Dead` — is deliberately
/// visible: nothing wrote a terminal state for it, and it is precisely what an
/// operator is looking for.
pub fn visible_in_list(status: &RunStatus) -> bool {
    !status.state.is_terminal()
}

fn load_status(mur_home: &Path, run_id: &str) -> Result<Option<RunStatus>> {
    crate::run_status::status_of(mur_home, run_id)
}

fn liveness_label(l: Liveness) -> &'static str {
    match l {
        Liveness::Alive => "alive",
        Liveness::Stalled => "STALLED",
        Liveness::Dead => "DEAD",
        Liveness::Unknown => "unknown",
        Liveness::NotApplicable => "-",
    }
}

fn state_label(s: State) -> &'static str {
    match s {
        State::Running => "running",
        State::Blocked => "blocked",
        State::Done => "done",
        State::Failed => "failed",
        State::Stopped => "stopped",
    }
}

pub fn run(mur_home: &Path, action: JobAction) -> Result<()> {
    match action {
        JobAction::List { all } => {
            let mut rows = Vec::new();
            for id in store::list_ids(mur_home)? {
                if let Some(status) = load_status(mur_home, &id)?
                    && (all || visible_in_list(&status))
                {
                    rows.push(status);
                }
            }
            rows.sort_by(|a, b| b.run.started_at.cmp(&a.run.started_at));
            if rows.is_empty() {
                println!("no runs");
                return Ok(());
            }
            println!("{:<28} {:<9} {:<9} {}", "RUN", "STATE", "LIVENESS", "LABEL");
            for s in rows {
                println!(
                    "{:<28} {:<9} {:<9} {}",
                    s.run.run_id,
                    state_label(s.state),
                    liveness_label(s.liveness),
                    s.run.label
                );
            }
            Ok(())
        }
        JobAction::Status { run_id } => {
            let Some(s) = load_status(mur_home, &run_id)? else {
                anyhow::bail!("no run recorded for `{run_id}` (try `mur job list --all`)");
            };
            println!("run       {}", s.run.run_id);
            println!("kind      {:?}", s.run.kind);
            println!("label     {}", s.run.label);
            println!("state     {}", state_label(s.state));
            println!("liveness  {}", liveness_label(s.liveness));
            println!("pid       {}", s.run.pid);
            println!("started   {}", s.run.started_at.to_rfc3339());
            match s.run.last_heartbeat_at {
                Some(b) => println!("heartbeat {}", b.to_rfc3339()),
                None => println!("heartbeat unknown (record was rebuilt from the channel)"),
            }
            if let Some(c) = &s.run.channel_id {
                println!("channel   {c}");
            }
            if let Some(b) = &s.run.blocked_on {
                println!("blocked   {} — {} (since {})", b.hitl_id, b.summary, b.since.to_rfc3339());
            }
            for step in &s.run.steps {
                println!(
                    "  step {:<12} {:<9} {}",
                    step.id,
                    state_label(step.state),
                    step.member.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        JobAction::Stop { run_id } => {
            // MUST go through `update`, not load + save: the executor process
            // for this run may still be beating its heartbeat, and a bare
            // read-modify-write here would be reverted by the next beat —
            // leaving a stopped run reporting `running` forever.
            let mut was_terminal = None;
            let existed = store::update(mur_home, &run_id, |record| {
                if record.state.is_terminal() {
                    was_terminal = Some(record.state);
                    return;
                }
                record.state = State::Stopped;
            })
            .with_context(|| format!("stop run `{run_id}`"))?;
            if !existed {
                anyhow::bail!("no run recorded for `{run_id}`");
            }
            if let Some(state) = was_terminal {
                println!("run {run_id} already {}", state_label(state));
                return Ok(());
            }
            println!("run {run_id} marked stopped");
            println!(
                "note: `mur job stop` stops one run. To stop a fleet's loop, use `mur fleet stop <name>`."
            );
            Ok(())
        }
    }
}
```

- [ ] **Step 4: Register the command**

Add to the `Commands` enum in `mur-core/src/cli/mod.rs`, following the `Channel { … }` pattern:

```rust
    /// Inspect and stop job / fleet / workflow runs
    Job {
        #[command(subcommand)]
        action: crate::cmd::job::JobAction,
    },
```

Add to `mur-core/src/dispatch.rs`, alongside `Commands::Channel { action } =>`:

```rust
        Commands::Job { action } => crate::cmd::job::run(&mur_home, action),
```

> Match the surrounding arms' shape: if neighbouring arms are `async` or take a different home variable name, follow them rather than this literal.

- [ ] **Step 5: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-core cmd::job
```

Expected: PASS, 2 tests.

- [ ] **Step 6: Verify the command is actually reachable**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo run -p mur-core --bin mur -- job list
```

Expected: prints `no runs` (or a table) and exits 0. A clap wiring mistake shows up here and nowhere in the unit tests.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/job.rs mur-core/src/cmd/mod.rs mur-core/src/cli/mod.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): mur job list/status/stop over run_status"
```

---

## Task 7: `mur_job_status` MCP tool

**Files:**
- Modify: `mur-mcp-server/src/tools.rs`
- Test: inline `#[cfg(test)]` in `mur-mcp-server/src/tools.rs` (or `mur-mcp-server/tests/integration.rs`, matching where the existing tool tests live)

**Interfaces:**
- Consumes: `mur_core::run_status::{classify, store}` (Tasks 1–2).
- Produces: MCP tool `mur_job_status` taking `{ run_id: string }` and returning the classified status as text.

- [ ] **Step 1: Write the failing test**

`mur-mcp-server` does not depend on `chrono`, and the fixture below needs a
timestamp. Add it as a **dev**-dependency only — the tool arm itself does not
touch `chrono`, so the shipped binary's dependency graph is unchanged. In
`mur-mcp-server/Cargo.toml`, under `[dev-dependencies]`:

```toml
chrono = { workspace = true }
tempfile = "3"
```

(Omit either line if it is already present.)

Add to the test module that already covers tool dispatch in `mur-mcp-server`:

```rust
    /// The agent-facing half of the fix. Without this, a tool timeout leaves
    /// the model with "outcome unknown" and nothing to ask — which is what
    /// taught agents to re-dispatch work that was still in flight.
    #[tokio::test]
    async fn mur_job_status_reports_a_recorded_run() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        mur_core::run_status::store::save(
            mur_home,
            &mur_core::run_status::RunState {
                schema: mur_core::run_status::RUN_SCHEMA,
                run_id: "run-1".into(),
                channel_id: None,
                kind: mur_core::run_status::RunKind::Job,
                label: "two jobs".into(),
                pid: std::process::id(),
                started_at: chrono::Utc::now(),
                last_heartbeat_at: Some(chrono::Utc::now()),
                state: mur_core::run_status::State::Running,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".into(),
                build_sha: "deadbee".into(),
            },
        )
        .unwrap();

        let out = call_tool_in(
            mur_home,
            "mur_job_status",
            serde_json::json!({ "run_id": "run-1" }),
        )
        .await
        .expect("tool call succeeded");

        assert!(out.contains("running"), "state missing from output: {out}");
        assert!(out.contains("alive"), "liveness missing from output: {out}");
    }

    #[tokio::test]
    async fn mur_job_status_on_an_unknown_run_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let out = call_tool_in(tmp.path(), "mur_job_status", serde_json::json!({ "run_id": "ghost" }))
            .await
            .expect("tool call succeeded");
        assert!(out.contains("no run recorded"), "unhelpful miss message: {out}");
    }
```

> `call_tool_in` stands for whatever helper the existing tests in this file use to invoke a tool against a given MUR home. Read the neighbouring tests and use theirs; if they invoke the dispatch function directly, do that instead of adding a helper.

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-mcp-server mur_job_status
```

Expected: FAIL — unknown tool `mur_job_status`.

- [ ] **Step 3: Add the tool definition**

In the tool list in `mur-mcp-server/src/tools.rs`, next to the `parallel_jobs` entry:

```rust
        Tool {
            name: "mur_job_status".into(),
            description: "Report the live status of a MUR run (a parallel_jobs dispatch, a fleet run, or a workflow run) by its run_id. Returns both a semantic state (running / blocked / done / failed / stopped) and a liveness verdict (alive / STALLED / DEAD / unknown). Use this after a tool call times out: a timeout means MUR stopped waiting, NOT that the work failed — ask here instead of re-dispatching.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([(
                    "run_id".into(),
                    ToolParam {
                        param_type: "string".into(),
                        description: "The run id returned when the run was dispatched.".into(),
                        default: None,
                    },
                )])),
                required: Some(vec!["run_id".into()]),
            },
        },
```

> Copy the exact field set of `ToolInputSchema` / `ToolParam` from the neighbouring `parallel_jobs` entry — this literal reproduces the fields visible there, but the struct may carry more.

- [ ] **Step 4: Add the dispatch arm**

Next to the `"parallel_jobs" => {` arm:

```rust
        "mur_job_status" => {
            let run_id = arguments
                .get("run_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'run_id' (string)".to_string())?;

            let loaded = mur_core::run_status::status_of(&mur_home, run_id)
                .map_err(|e| format!("read run {run_id}: {e}"))?;
            let Some(status) = loaded else {
                return Ok(format!(
                    "no run recorded for `{run_id}` — it may predate run recording, or the id may be wrong"
                ));
            };

            let liveness = match status.liveness {
                mur_core::run_status::Liveness::Alive => "alive",
                mur_core::run_status::Liveness::Stalled => "STALLED",
                mur_core::run_status::Liveness::Dead => "DEAD",
                mur_core::run_status::Liveness::Unknown => "unknown",
                mur_core::run_status::Liveness::NotApplicable => "n/a",
            };
            let state = match status.state {
                mur_core::run_status::State::Running => "running",
                mur_core::run_status::State::Blocked => "blocked",
                mur_core::run_status::State::Done => "done",
                mur_core::run_status::State::Failed => "failed",
                mur_core::run_status::State::Stopped => "stopped",
            };
            Ok(format!(
                "run {} — state: {state}, liveness: {liveness}\nlabel: {}\nstarted: {}\nsteps: {}",
                status.run.run_id,
                status.run.label,
                status.run.started_at.to_rfc3339(),
                status.run.steps.len()
            ))
        }
```

> `mur_home` stands for however this file already resolves the MUR home in neighbouring arms; reuse that expression. Return type and error type must match the surrounding arms exactly.

- [ ] **Step 5: Add `mur_job_status` to the auto-compress skip list**

A status reply is short and is read for its exact wording; compressing it would put the answer behind another tool call. In `mur-mcp-server/src/tools.rs`:

```rust
const AUTO_COMPRESS_SKIP: &[&str] = &[
    "mur_compress",
    "mur_retrieve",
    "mur_compress_stats",
    "mur_job_status",
];
```

- [ ] **Step 6: Run tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run -p mur-mcp-server mur_job_status
```

Expected: PASS, 2 tests.

- [ ] **Step 7: Full verification gate**

```bash
cargo fmt --check
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo clippy --workspace --all-targets -- -D warnings
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist \
  cargo nextest run --workspace
```

Expected: all three exit 0. Read the actual `Summary` line and the real exit codes — do not infer success from a command appearing to finish.

- [ ] **Step 8: Commit**

```bash
git add mur-mcp-server/src/tools.rs
git commit -m "feat(mcp): mur_job_status so an agent can ask instead of guessing after a timeout"
```

---

## Not in this plan

Plan B (`docs/superpowers/plans/…-job-fleet-run-status-b.md`, written after this one lands) carries the behaviour changes that depend on this foundation: non-blocking `parallel_jobs`, HITL block-instead-of-deny writing `blocked_on`, ready-set DAG scheduling so a blocked step stops holding the wave barrier, open-item notification, the Hub Panel renderer, and the attended/unattended budget split.

Spec §5's seam still holds: `2026-08-17-murmur-tui-composer-and-tool-lines-design.md` must be implemented after Plan B, because Plan B changes `hitl::gate::DEFAULT_TIMEOUT`'s semantics and the TUI reads that constant to draw its countdown.
