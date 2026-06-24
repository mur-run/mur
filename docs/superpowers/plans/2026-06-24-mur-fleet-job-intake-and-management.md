# MUR Fleet — Job Intake & Roster Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an operator dispatch work to a fleet by command (`send` / `run "<job>"` / `jobs`) instead of hand-editing `fleet.yaml`'s `goal`, and add first-class `add` / `remove` / `delete` plus a scannable `list`.

**Architecture:** A fleet stays a durable squad; `goal` becomes the standing-mission default. Work enters as a **job** — a small YAML record under `~/.mur/fleets/<name>/jobs/<uuidv7>.yaml` whose status enum mirrors the A2A Task lifecycle. The existing `run` / `run --loop` / daemon `fleet_tick` executors drain the queue (oldest first) — no new daemon, no new loop. Roster commands keep `fleet.yaml` and the shared channel `fleet-<name>` in sync via new `ChannelService` participant/delete primitives.

**Tech Stack:** Rust (edition 2024), `serde` / `serde_yaml`, `uuid` (v7, time-sortable), `chrono`, `clap` (derive), `anyhow`. Tests via `cargo nextest`.

## Global Constraints

- **No hardcoded values** — constants/config/env, never literals for tunables (Mandatory Rule 1).
- **Single source file ≤ 800 lines** — new surface goes in new sibling modules, never bolted onto the already-large `import.rs` / `loop_run.rs` (Mandatory Rule 4).
- **mur-core build/test requires `ORT_STRATEGY=download`** and **`cargo nextest`** (plain `cargo test --workspace` fails spuriously). mur-common / mur-channel don't need the env var.
- **Fleet name is validated everywhere** it forms a path — call `mur_common::fleet::valid_fleet_name` at every job-store entry point (defense-in-depth, matches `store::save_fleet`).
- **Fail-closed execution** — every DAG run passes `DagExecOptions { yes: false, .. }`. A job is a plain goal string; it grants no new authority and is never shell/path-interpolated.
- **Brand:** user-facing text says "MUR"; the CLI/command/dir slug stays lowercase `mur`.

---

## File Structure

- `mur-common/src/fleet.rs` — **modify**: add `Job` struct + `JobStatus` enum (pure data, no I/O).
- `mur-core/src/cmd/fleet/jobs.rs` — **create**: job store API + `send` / `jobs` commands.
- `mur-core/src/cmd/fleet/store.rs` — **modify**: nothing required (jobs.rs derives paths from `fleet_dir`).
- `mur-core/src/cmd/fleet/run.rs` — **modify**: goal resolution (arg > queued > standing goal) + job lifecycle stamping.
- `mur-core/src/cmd/fleet/loop_run.rs` — **modify**: per-iteration queue drain (this also covers the daemon, which calls `cmd_fleet_run_loop`).
- `mur-core/src/cmd/fleet/list.rs` — **modify**: aligned-table renderer.
- `mur-core/src/cmd/fleet/roster.rs` — **create**: `add` / `remove`.
- `mur-core/src/cmd/fleet/delete.rs` — **create**: `delete`.
- `mur-core/src/cmd/fleet/mod.rs` — **modify**: declare `jobs`, `roster`, `delete`.
- `mur-channel/src/service.rs` — **modify**: `add_participant` / `remove_participant` / `delete_channel`.
- `mur-channel/src/store.rs` — **modify**: `delete` (remove channel dir).
- `mur-channel/src/index.rs` — **modify**: `remove` (delete the read-model row).
- `mur-core/src/cli/actions.rs` — **modify**: `FleetAction::{Send, Jobs, Add, Remove, Delete}` + `Run.job`.
- `mur-core/src/dispatch.rs` — **modify**: wire the new actions.
- `mur-core/tests/cli_fleet.rs` — **modify**: end-to-end round-trip.

---

## Task 1: Job model (`Job` + `JobStatus`)

**Files:**
- Modify: `mur-common/src/fleet.rs` (append after the `FleetLoop` block)
- Test: `mur-common/src/fleet.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `mur_common::fleet::Job { id, text, source, status, created_at, started_at, finished_at, run_id, result, error }`; `mur_common::fleet::JobStatus::{Queued, Running, Done, Failed, Canceled}` with `is_terminal(&self) -> bool`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `mur-common/src/fleet.rs`:

```rust
#[test]
fn job_status_serde_is_lowercase_and_terminal_predicate() {
    assert_eq!(serde_yaml::to_string(&JobStatus::Queued).unwrap().trim(), "queued");
    assert_eq!(serde_yaml::to_string(&JobStatus::Done).unwrap().trim(), "done");
    assert!(!JobStatus::Queued.is_terminal());
    assert!(!JobStatus::Running.is_terminal());
    assert!(JobStatus::Done.is_terminal());
    assert!(JobStatus::Failed.is_terminal());
    assert!(JobStatus::Canceled.is_terminal());
}

#[test]
fn job_yaml_roundtrip_with_optional_fields_skipped() {
    let j = Job {
        id: "0190f3a2-0000-7000-8000-000000000000".into(),
        text: "ship it".into(),
        source: "cli".into(),
        status: JobStatus::Queued,
        created_at: "2026-06-24T00:00:00Z".into(),
        started_at: None,
        finished_at: None,
        run_id: None,
        result: None,
        error: None,
    };
    let yaml = serde_yaml::to_string(&j).unwrap();
    assert!(!yaml.contains("started_at"), "None optionals must be skipped: {yaml}");
    let back: Job = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(back, j);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-common job_status_serde job_yaml_roundtrip`
Expected: FAIL — `cannot find type Job` / `JobStatus`.

- [ ] **Step 3: Write minimal implementation**

Append to `mur-common/src/fleet.rs` (before the `#[cfg(test)]` block):

```rust
/// A unit of work handed to a fleet. Status mirrors the A2A Task lifecycle so
/// A2A intake (follow-on) can treat a job AS an A2A Task with no model change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,   // A2A: submitted
    Running,  // A2A: working
    Done,     // A2A: completed
    Failed,   // A2A: failed
    Canceled, // A2A: canceled
}

impl JobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobStatus::Done | JobStatus::Failed | JobStatus::Canceled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// UUIDv7 — time-sortable, so FIFO ordering is just a filename sort.
    pub id: String,
    pub text: String,
    /// "cli" | "a2a:<agent-id>" (a2a is follow-on).
    pub source: String,
    pub status: JobStatus,
    /// RFC3339 timestamps.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Channel run that executed the job (results live there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-common job_status_serde job_yaml_roundtrip`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/fleet.rs
git commit -m "feat(fleet): Job + JobStatus model (A2A-aligned lifecycle)"
```

---

## Task 2: Jobs store API

**Files:**
- Create: `mur-core/src/cmd/fleet/jobs.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add `pub mod jobs;`)
- Test: in `jobs.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `mur_common::fleet::{Job, JobStatus, valid_fleet_name}`; `super::store::fleet_dir`.
- Produces: `jobs::jobs_dir(home,&str)->PathBuf`; `jobs::enqueue_job(home,&str,text:&str,source:&str)->Result<Job>`; `jobs::save_job(home,&str,&Job)->Result<()>`; `jobs::list_jobs(home,&str)->Result<Vec<Job>>`; `jobs::next_queued(home,&str)->Result<Option<Job>>`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/jobs.rs` with ONLY the test module first (the impl comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::JobStatus;

    #[test]
    fn enqueue_list_next_fifo_and_update() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let a = enqueue_job(home, "dev", "first", "cli").unwrap();
        let b = enqueue_job(home, "dev", "second", "cli").unwrap();
        assert_ne!(a.id, b.id);

        let all = list_jobs(home, "dev").unwrap();
        assert_eq!(all.len(), 2);
        // uuid v7 => creation order
        assert_eq!(all[0].text, "first");
        assert_eq!(all[1].text, "second");

        // oldest queued is `a`
        assert_eq!(next_queued(home, "dev").unwrap().unwrap().id, a.id);

        // mark `a` running => next_queued skips it
        let mut a2 = a.clone();
        a2.status = JobStatus::Running;
        save_job(home, "dev", &a2).unwrap();
        assert_eq!(next_queued(home, "dev").unwrap().unwrap().id, b.id);
    }

    #[test]
    fn invalid_fleet_name_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(enqueue_job(tmp.path(), "../evil", "x", "cli").is_err());
    }

    #[test]
    fn list_empty_when_no_jobs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_jobs(tmp.path(), "dev").unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core enqueue_list_next_fifo`
Expected: FAIL — `cannot find function enqueue_job`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-core/src/cmd/fleet/jobs.rs` (above the test module):

```rust
//! Fleet job queue — small YAML records under `~/.mur/fleets/<name>/jobs/`.
//! A job is a unit of work handed to a fleet by command; it becomes the goal
//! for one run. FIFO ordering is the UUIDv7 filename sort (no index file).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::fleet::{Job, JobStatus, valid_fleet_name};

use super::store;

pub fn jobs_dir(mur_home: &Path, fleet: &str) -> PathBuf {
    store::fleet_dir(mur_home, fleet).join("jobs")
}

fn job_path(mur_home: &Path, fleet: &str, id: &str) -> PathBuf {
    jobs_dir(mur_home, fleet).join(format!("{id}.yaml"))
}

/// Write a new queued job and return it.
pub fn enqueue_job(mur_home: &Path, fleet: &str, text: &str, source: &str) -> Result<Job> {
    let job = Job {
        id: uuid::Uuid::now_v7().to_string(),
        text: text.to_string(),
        source: source.to_string(),
        status: JobStatus::Queued,
        created_at: chrono::Utc::now().to_rfc3339(),
        started_at: None,
        finished_at: None,
        run_id: None,
        result: None,
        error: None,
    };
    save_job(mur_home, fleet, &job)?;
    Ok(job)
}

/// Atomic write (temp + rename), matching `store::save_fleet`.
pub fn save_job(mur_home: &Path, fleet: &str, job: &Job) -> Result<()> {
    if !valid_fleet_name(fleet) {
        bail!("invalid fleet name '{fleet}': use lowercase letters, digits, '-' or '_'");
    }
    let dir = jobs_dir(mur_home, fleet);
    std::fs::create_dir_all(&dir).with_context(|| format!("create jobs dir {}", dir.display()))?;
    let path = job_path(mur_home, fleet, &job.id);
    let yaml = serde_yaml::to_string(job).context("serialize job")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// All jobs sorted oldest-first (UUIDv7 lexical sort == time order).
pub fn list_jobs(mur_home: &Path, fleet: &str) -> Result<Vec<Job>> {
    if !valid_fleet_name(fleet) {
        bail!("invalid fleet name '{fleet}'");
    }
    let dir = jobs_dir(mur_home, fleet);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        // Skip the *.yaml.tmp atomic-write staging files (extension == "tmp").
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(j) = serde_yaml::from_str::<Job>(&s)
        {
            jobs.push(j);
        }
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(jobs)
}

/// The oldest `Queued` job, if any.
pub fn next_queued(mur_home: &Path, fleet: &str) -> Result<Option<Job>> {
    Ok(list_jobs(mur_home, fleet)?
        .into_iter()
        .find(|j| j.status == JobStatus::Queued))
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod jobs;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/enqueue_list_next_fifo|invalid_fleet_name_refused|list_empty_when_no_jobs_dir/)'`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet/jobs.rs mur-core/src/cmd/fleet/mod.rs
git commit -m "feat(fleet): job queue store API (enqueue/list/next_queued/save)"
```

---

## Task 3: `mur fleet send` + `mur fleet jobs` commands + CLI wiring

**Files:**
- Modify: `mur-core/src/cmd/fleet/jobs.rs` (add the two command fns + tests)
- Modify: `mur-core/src/cli/actions.rs` (`FleetAction::Send`, `FleetAction::Jobs`)
- Modify: `mur-core/src/dispatch.rs` (wire them)

**Interfaces:**
- Consumes: `store::load_fleet`, `jobs::{enqueue_job, list_jobs}`.
- Produces: `jobs::cmd_fleet_send(home,fleet,text)->Result<()>`; `jobs::cmd_fleet_jobs(home,fleet,all:bool)->Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` in `jobs.rs`:

```rust
#[test]
fn send_requires_existing_fleet_then_enqueues() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // no fleet yet → error
    assert!(cmd_fleet_send(home, "dev", "do it").is_err());
    super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, None).unwrap();
    cmd_fleet_send(home, "dev", "do it").unwrap();
    let jobs = list_jobs(home, "dev").unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].text, "do it");
    assert_eq!(jobs[0].source, "cli");
    // jobs view runs without panicking on an existing fleet
    cmd_fleet_jobs(home, "dev", true).unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core send_requires_existing_fleet`
Expected: FAIL — `cannot find function cmd_fleet_send`.

- [ ] **Step 3: Write minimal implementation**

Add to `mur-core/src/cmd/fleet/jobs.rs` (above the test module):

```rust
/// `mur fleet send <name> "<job>"` — enqueue a job (asynchronous).
pub fn cmd_fleet_send(mur_home: &Path, fleet: &str, text: &str) -> Result<()> {
    let _ = store::load_fleet(mur_home, fleet)?; // validates name + existence
    let job = enqueue_job(mur_home, fleet, text, "cli")?;
    println!(
        "Queued job {} for fleet '{fleet}'. Drain it with `mur fleet run {fleet}` (or the daemon).",
        job.id
    );
    Ok(())
}

/// `mur fleet jobs <name> [--all]` — list jobs and their status.
pub fn cmd_fleet_jobs(mur_home: &Path, fleet: &str, all: bool) -> Result<()> {
    let _ = store::load_fleet(mur_home, fleet)?;
    let jobs = list_jobs(mur_home, fleet)?;
    let shown: Vec<&Job> = jobs
        .iter()
        .filter(|j| all || !j.status.is_terminal())
        .collect();
    if shown.is_empty() {
        println!("No jobs. Queue one: mur fleet send {fleet} \"<job>\"");
        return Ok(());
    }
    for j in shown {
        let short = &j.id[..j.id.len().min(8)];
        let status = format!("{:?}", j.status).to_lowercase();
        let note = j.error.as_deref().or(j.result.as_deref()).unwrap_or("");
        let text = if j.text.chars().count() > 50 {
            format!("{}…", j.text.chars().take(49).collect::<String>())
        } else {
            j.text.clone()
        };
        println!("{short}  {status:<9}  {text}  {note}");
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core send_requires_existing_fleet`
Expected: PASS.

- [ ] **Step 5: Wire the CLI**

In `mur-core/src/cli/actions.rs`, add to `enum FleetAction` (after `Run`):

```rust
    /// Queue a job for a fleet (async; drained by `run` or the daemon)
    Send {
        /// Fleet name
        name: String,
        /// The job text (becomes the goal for one run)
        job: String,
    },
    /// List a fleet's jobs and their status
    Jobs {
        /// Fleet name
        name: String,
        /// Include terminal (done/failed/canceled) jobs
        #[arg(long)]
        all: bool,
    },
```

In `mur-core/src/dispatch.rs`, add to the `match action` arms:

```rust
                FleetAction::Send { name, job } => {
                    cmd::fleet::jobs::cmd_fleet_send(&mur_home, &name, &job)?
                }
                FleetAction::Jobs { name, all } => {
                    cmd::fleet::jobs::cmd_fleet_jobs(&mur_home, &name, all)?
                }
```

- [ ] **Step 6: Verify it builds + lints**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/jobs.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): mur fleet send + jobs commands"
```

---

## Task 4: `mur fleet run [<job>]` goal resolution + job lifecycle

**Files:**
- Modify: `mur-core/src/cmd/fleet/run.rs`
- Modify: `mur-core/src/cli/actions.rs` (`Run.job`)
- Modify: `mur-core/src/dispatch.rs` (pass `job`; loop path enqueues then loops)

**Interfaces:**
- Consumes: `jobs::{enqueue_job, next_queued, save_job}`, `mur_common::fleet::{Fleet, Job, JobStatus}`.
- Produces: `run::cmd_fleet_run(home, name, job_arg: Option<String>) -> Result<()>` (signature change — add the `job_arg` param).

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` in `run.rs`:

```rust
#[test]
fn resolve_goal_prefers_arg_then_queued_then_standing() {
    // pure resolution helper — no execution
    use mur_common::fleet::JobStatus;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("standing".into())).unwrap();

    // no arg, empty queue => standing goal
    let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
    assert_eq!(goal, "standing");
    assert!(job.is_none());

    // queued job => that job's text, marked running
    super::super::jobs::enqueue_job(home, "dev", "queued-work", "cli").unwrap();
    let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
    assert_eq!(goal, "queued-work");
    assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);

    // explicit arg jumps ahead of the queue
    let (goal, job) = resolve_run_goal(home, "dev", Some("urgent".into()), "standing").unwrap();
    assert_eq!(goal, "urgent");
    assert_eq!(job.unwrap().text, "urgent");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core resolve_goal_prefers_arg`
Expected: FAIL — `cannot find function resolve_run_goal`.

- [ ] **Step 3: Write minimal implementation**

In `mur-core/src/cmd/fleet/run.rs`, add this helper (above `cmd_fleet_run`):

```rust
use mur_common::fleet::{Job, JobStatus};

/// Resolve this run's goal: explicit arg > oldest queued job > standing goal.
/// When a job backs the run it is enqueued (arg) and marked `Running` here so
/// the queue cursor advances atomically before execution.
pub fn resolve_run_goal(
    mur_home: &Path,
    name: &str,
    job_arg: Option<String>,
    standing_goal: &str,
) -> Result<(String, Option<Job>)> {
    let mut active: Option<Job> = match job_arg {
        Some(text) => Some(super::jobs::enqueue_job(mur_home, name, &text, "cli")?),
        None => super::jobs::next_queued(mur_home, name)?,
    };
    let goal = active
        .as_ref()
        .map(|j| j.text.clone())
        .unwrap_or_else(|| standing_goal.to_string());
    if let Some(job) = active.as_mut() {
        job.status = JobStatus::Running;
        job.started_at = Some(chrono::Utc::now().to_rfc3339());
        super::jobs::save_job(mur_home, name, job)?;
    }
    Ok((goal, active))
}
```

Now change `cmd_fleet_run`'s signature and body. Replace the current `pub async fn cmd_fleet_run(mur_home: &Path, name: &str) -> Result<()> {` through the `let proc = ...; let opts = ...;` section with:

```rust
pub async fn cmd_fleet_run(mur_home: &Path, name: &str, job_arg: Option<String>) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        bail!("fleet '{name}' has no members");
    }
    if super::control::is_stopped(mur_home, name) {
        bail!("fleet '{name}' is stopped (kill-switch). Run `mur fleet start {name}` to re-enable.");
    }
    let (goal, mut active_job) = resolve_run_goal(mur_home, name, job_arg, &fleet.goal)?;
    if goal.is_empty() {
        bail!("fleet '{name}' has no goal and no queued job; pass one: mur fleet run {name} \"<job>\"");
    }
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let events = svc.load_events(&fleet.channel_id)?;
    let since = events.last().map(|e| e.seq).unwrap_or(0);
    // Plan with the resolved goal (override the standing goal for this run).
    let planning_fleet = mur_common::fleet::Fleet { goal: goal.clone(), ..fleet.clone() };
    let proc = super::plan::plan_via_router(mur_home, &planning_fleet, &events)
        .unwrap_or_else(|| build_fleet_procedure(&goal, &fleet.members));
    let run_id = format!("run-{}", uuid::Uuid::now_v7());
    let opts = crate::executor::dag::DagExecOptions {
        yes: false,
        channel_id: Some(fleet.channel_id.clone()),
        run_id: run_id.clone(),
        ..Default::default()
    };
    let result =
        crate::executor::dag::execute_dag(mur_home, &format!("fleet:{}", fleet.name), &proc, &opts)
            .await;

    // Stamp the job terminal (if a job backed this run).
    if let Some(job) = active_job.as_mut() {
        job.run_id = Some(run_id);
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());
        match &result {
            Ok(out) => {
                job.status = JobStatus::Done;
                job.result = out.output_text.clone().filter(|t| !t.is_empty());
            }
            Err(e) => {
                job.status = JobStatus::Failed;
                job.error = Some(format!("{e:#}"));
            }
        }
        super::jobs::save_job(mur_home, name, job)?;
    }
    let out = result?;
    if let Some(t) = out.output_text.filter(|t| !t.is_empty()) {
        println!("{t}");
    }
```

The existing reply-tail loop (filtering `e.seq > since`) and the closing `Ok(())` stay unchanged below this point.

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core resolve_goal_prefers_arg`
Expected: PASS.

- [ ] **Step 5: Wire the CLI arg**

In `mur-core/src/cli/actions.rs`, add the optional positional to `FleetAction::Run`:

```rust
        /// Optional job text — runs this as a one-shot (jumps ahead of the queue)
        job: Option<String>,
```

(Place it after `name` and before `loop_flag` so the positional binds correctly.)

In `mur-core/src/dispatch.rs`, update the `FleetAction::Run` arm — destructure `job` and route it:

```rust
                FleetAction::Run {
                    name,
                    job,
                    loop_flag,
                    max_iterations,
                    deadline,
                    budget_usd,
                } => {
                    if loop_flag {
                        // A job arg with --loop is enqueued, then the loop drains it.
                        if let Some(text) = job {
                            cmd::fleet::jobs::enqueue_job(&mur_home, &name, &text, "cli")?;
                        }
                        cmd::fleet::loop_run::cmd_fleet_run_loop(
                            &mur_home, &name, max_iterations, deadline, budget_usd,
                        )
                        .await?
                    } else {
                        cmd::fleet::run::cmd_fleet_run(&mur_home, &name, job).await?
                    }
                }
```

- [ ] **Step 6: Verify build + lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean (also confirms the new `cmd_fleet_run` arity matches every caller).

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/run.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): run [<job>] resolves arg>queued>goal + stamps job lifecycle"
```

---

## Task 5: Per-iteration queue drain in `run --loop` (covers the daemon)

**Files:**
- Modify: `mur-core/src/cmd/fleet/loop_run.rs`

**Interfaces:**
- Consumes: `jobs::{next_queued, save_job}`, `mur_common::fleet::{Fleet, Job, JobStatus}`.
- Produces: no new public symbol; behavior change only. (The daemon `fleet_tick` calls `cmd_fleet_run_loop`, so this drain is automatically active for daemon auto-run too.)

- [ ] **Step 1: Write the failing test**

Add a focused unit test to the `mod tests` in `loop_run.rs` that exercises the per-iteration goal resolution in isolation (no executor). First extract the resolution into a tiny helper so it is testable:

```rust
#[test]
fn iteration_goal_drains_queue_then_falls_back_to_standing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("standing".into())).unwrap();

    // empty queue => standing goal, no job
    let (g, j) = iteration_goal(home, "dev", "standing").unwrap();
    assert_eq!(g, "standing");
    assert!(j.is_none());

    // queued job => its text, marked running
    super::super::jobs::enqueue_job(home, "dev", "job-1", "cli").unwrap();
    let (g, j) = iteration_goal(home, "dev", "standing").unwrap();
    assert_eq!(g, "job-1");
    assert_eq!(j.unwrap().status, mur_common::fleet::JobStatus::Running);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core iteration_goal_drains_queue`
Expected: FAIL — `cannot find function iteration_goal`.

- [ ] **Step 3: Write minimal implementation**

Add this helper to `loop_run.rs` (module level, above `cmd_fleet_run_loop`):

```rust
use mur_common::fleet::{Job, JobStatus};

/// Resolve one loop iteration's goal: oldest queued job > standing goal.
/// Marks the chosen job `Running` so the queue cursor advances before the run.
fn iteration_goal(mur_home: &Path, name: &str, standing_goal: &str) -> Result<(String, Option<Job>)> {
    let mut job = super::jobs::next_queued(mur_home, name)?;
    let goal = job
        .as_ref()
        .map(|j| j.text.clone())
        .unwrap_or_else(|| standing_goal.to_string());
    if let Some(j) = job.as_mut() {
        j.status = JobStatus::Running;
        j.started_at = Some(chrono::Utc::now().to_rfc3339());
        super::jobs::save_job(mur_home, name, j)?;
    }
    Ok((goal, job))
}
```

Now wire it into the loop body. Replace the planning lines (currently):

```rust
        let pre_events = svc.load_events(&fleet.channel_id).unwrap_or_default();
        let proc = super::plan::plan_via_router(mur_home, &fleet, &pre_events)
            .unwrap_or_else(|| build_fleet_procedure(&fleet.goal, &fleet.members));
```

with:

```rust
        let pre_events = svc.load_events(&fleet.channel_id).unwrap_or_default();
        // Drain the job queue: oldest queued job is this iteration's goal; else the standing goal.
        let (iter_goal, mut active_job) = iteration_goal(mur_home, name, &fleet.goal)?;
        let planning_fleet = mur_common::fleet::Fleet { goal: iter_goal.clone(), ..fleet.clone() };
        let proc = super::plan::plan_via_router(mur_home, &planning_fleet, &pre_events)
            .unwrap_or_else(|| build_fleet_procedure(&iter_goal, &fleet.members));
```

After the `execute_dag(...).await?` line and the `iteration += 1;` line, add the terminal stamp:

```rust
        if let Some(job) = active_job.as_mut() {
            job.run_id = Some(opts.run_id.clone());
            job.finished_at = Some(chrono::Utc::now().to_rfc3339());
            job.status = JobStatus::Done;
            job.result = out.output_text.clone().filter(|t| !t.is_empty());
            let _ = super::jobs::save_job(mur_home, name, job);
        }
```

(`out` is the `execute_dag` result already bound in the loop; `opts.run_id` is still in scope. Note: the loop propagates executor errors via `?` today, so a failed iteration aborts the loop before this stamp — the job stays `Running`, which is correct: it was not completed. No silent retry.)

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core iteration_goal_drains_queue`
Expected: PASS.

- [ ] **Step 5: Verify build + lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/fleet/loop_run.rs
git commit -m "feat(fleet): drain job queue per loop iteration (also covers daemon auto-run)"
```

---

## Task 6: `mur fleet list` aligned table

**Files:**
- Modify: `mur-core/src/cmd/fleet/list.rs`

**Interfaces:**
- Consumes: `store::{list_fleets, load_fleet}`, `control::is_stopped`, `jobs::list_jobs`, `mur_common::fleet::JobStatus`.
- Produces: `list::status_symbol(stopped: bool, running: bool) -> &'static str`; `list::truncate_goal(goal: &str, width: usize) -> String` (testable pure helpers).

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)] mod tests` to `list.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_symbol_precedence() {
        assert_eq!(status_symbol(true, true), "⏸");   // stopped wins over running
        assert_eq!(status_symbol(true, false), "⏸");
        assert_eq!(status_symbol(false, true), "▶");
        assert_eq!(status_symbol(false, false), "●");
    }

    #[test]
    fn truncate_goal_collapses_newlines_and_caps_width() {
        assert_eq!(truncate_goal("a\nb\nc", 80), "a b c");
        let long = "x".repeat(100);
        let out = truncate_goal(&long, 10);
        assert!(out.chars().count() <= 10, "got {} chars", out.chars().count());
        assert!(out.ends_with('…'));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core status_symbol_precedence truncate_goal_collapses`
Expected: FAIL — `cannot find function status_symbol`.

- [ ] **Step 3: Write minimal implementation**

Replace the body of `mur-core/src/cmd/fleet/list.rs` with:

```rust
//! `mur fleet list` — aligned table: NAME / ST / MEM / JOBS / ROUTER / GOAL.

use std::path::Path;

use anyhow::Result;
use mur_common::fleet::JobStatus;

use super::{control, jobs, store};

/// Status glyph: stopped (kill-switch) wins over running, else idle.
pub fn status_symbol(stopped: bool, running: bool) -> &'static str {
    if stopped {
        "⏸"
    } else if running {
        "▶"
    } else {
        "●"
    }
}

/// Collapse newlines to spaces and truncate to `width` chars with an ellipsis.
pub fn truncate_goal(goal: &str, width: usize) -> String {
    let one_line = goal.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= width {
        return one_line;
    }
    let keep = width.saturating_sub(1);
    format!("{}…", one_line.chars().take(keep).collect::<String>())
}

/// Terminal width from $COLUMNS, default 80.
fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w >= 40)
        .unwrap_or(80)
}

pub fn cmd_fleet_list(mur_home: &Path) -> Result<()> {
    let names = store::list_fleets(mur_home)?;
    if names.is_empty() {
        println!("No fleets. Create one: mur fleet create <name> --members a,b,c --goal \"...\"");
        return Ok(());
    }

    // Gather rows first so column widths fit the data.
    struct Row {
        name: String,
        st: &'static str,
        mem: usize,
        queued: usize,
        router: String,
        goal: String,
    }
    let mut rows = Vec::new();
    for n in &names {
        let f = store::load_fleet(mur_home, n)?;
        let job_list = jobs::list_jobs(mur_home, n).unwrap_or_default();
        let running = job_list.iter().any(|j| j.status == JobStatus::Running);
        let queued = job_list.iter().filter(|j| j.status == JobStatus::Queued).count();
        rows.push(Row {
            name: f.name.clone(),
            st: status_symbol(control::is_stopped(mur_home, n), running),
            mem: f.members.len(),
            queued,
            router: f.router_or_concierge().to_string(),
            goal: f.goal.clone(),
        });
    }

    let name_w = rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(4).max(4);
    let router_w = rows.iter().map(|r| r.router.chars().count()).max().unwrap_or(6).max(6);
    // GOAL gets the terminal remainder after the fixed columns and their
    // 2-space gaps: ST(2)+MEM(4)+JOBS(4)=10 fixed-width cols, plus NAME +
    // ROUTER, plus five 2-space gaps (=10).
    let fixed = name_w + router_w + 10 + 10;
    let goal_w = term_width().saturating_sub(fixed).max(20);

    println!(
        "{:<name_w$}  {:<2}  {:<4}  {:<4}  {:<router_w$}  {}",
        "NAME", "ST", "MEM", "JOBS", "ROUTER", "GOAL"
    );
    for r in &rows {
        println!(
            "{:<name_w$}  {:<2}  {:<4}  {:<4}  {:<router_w$}  {}",
            r.name,
            r.st,
            r.mem,
            r.queued,
            r.router,
            truncate_goal(&r.goal, goal_w),
        );
    }
    println!("\n● idle   ⏸ stopped   ▶ running");
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core status_symbol_precedence truncate_goal_collapses`
Expected: PASS (2 tests).

- [ ] **Step 5: Verify build + lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/fleet/list.rs
git commit -m "feat(fleet): aligned list table with status + job count, truncated goal"
```

---

## Task 7: Channel primitives — add/remove participant + delete

**Files:**
- Modify: `mur-channel/src/store.rs` (`delete`)
- Modify: `mur-channel/src/index.rs` (`remove`)
- Modify: `mur-channel/src/service.rs` (`add_participant`, `remove_participant`, `delete_channel`)

**Interfaces:**
- Consumes: `ChannelStore::{load_manifest, save_manifest}`, `ChannelIndex::upsert`, `mur_common::channel::{ChannelActor, Participant, ParticipantRole}`.
- Produces:
  - `ChannelStore::delete(&self, id:&str) -> Result<()>`
  - `ChannelIndex::remove(&self, id:&str) -> Result<()>`
  - `ChannelService::add_participant(&self, channel_id:&str, agent_id:&str, role:ParticipantRole) -> Result<()>`
  - `ChannelService::remove_participant(&self, channel_id:&str, agent_id:&str) -> Result<()>`
  - `ChannelService::delete_channel(&self, channel_id:&str) -> Result<()>`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `mur-channel/src/service.rs`:

```rust
#[test]
fn add_remove_participant_and_delete_channel() {
    use mur_common::channel::ParticipantRole;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_fleet("dev", "mur", &["pm".to_string()]).unwrap();

    // add a Delegate member (idempotent)
    svc.add_participant(&ch.id, "qa", ParticipantRole::Delegate).unwrap();
    svc.add_participant(&ch.id, "qa", ParticipantRole::Delegate).unwrap();
    let m = svc.store().load_manifest(&ch.id).unwrap();
    let qa_count = m
        .participants
        .iter()
        .filter(|p| matches!(&p.actor, mur_common::channel::ChannelActor::Agent { id } if id == "qa"))
        .count();
    assert_eq!(qa_count, 1, "add must be idempotent");

    // remove it
    svc.remove_participant(&ch.id, "qa").unwrap();
    let m = svc.store().load_manifest(&ch.id).unwrap();
    assert!(!m.participants.iter().any(
        |p| matches!(&p.actor, mur_common::channel::ChannelActor::Agent { id } if id == "qa")
    ));

    // delete the whole channel
    svc.delete_channel(&ch.id).unwrap();
    assert!(svc.store().load_manifest(&ch.id).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-channel add_remove_participant_and_delete_channel`
Expected: FAIL — `no method named add_participant`.

- [ ] **Step 3: Write minimal implementation**

In `mur-channel/src/store.rs` add:

```rust
    /// Remove a channel's directory entirely (manifest + events). Idempotent.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.channel_dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        }
        Ok(())
    }
```

In `mur-channel/src/index.rs` add:

```rust
    /// Drop a channel row from the read-model. Idempotent.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM channels WHERE id = ?1", [id])?;
        Ok(())
    }
```

In `mur-channel/src/service.rs` add (alongside the other `pub fn`s, e.g. after `create_for_fleet`):

```rust
    /// Add an agent participant to a channel (idempotent on agent id). Re-indexes.
    pub fn add_participant(
        &self,
        channel_id: &str,
        agent_id: &str,
        role: ParticipantRole,
    ) -> Result<()> {
        let mut ch = self.store.load_manifest(channel_id)?;
        let exists = ch.participants.iter().any(
            |p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent_id),
        );
        if !exists {
            ch.participants.push(Participant {
                actor: ChannelActor::Agent { id: agent_id.to_string() },
                role,
                joined_at: Utc::now(),
            });
            ch.updated_at = Utc::now();
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(())
    }

    /// Remove an agent participant from a channel (no-op if absent). Re-indexes.
    pub fn remove_participant(&self, channel_id: &str, agent_id: &str) -> Result<()> {
        let mut ch = self.store.load_manifest(channel_id)?;
        let before = ch.participants.len();
        ch.participants.retain(
            |p| !matches!(&p.actor, ChannelActor::Agent { id } if id == agent_id),
        );
        if ch.participants.len() != before {
            ch.updated_at = Utc::now();
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(())
    }

    /// Delete a channel entirely (store dir + read-model row). Idempotent.
    pub fn delete_channel(&self, channel_id: &str) -> Result<()> {
        self.store.delete(channel_id)?;
        self.index.remove(channel_id)?;
        Ok(())
    }
```

Ensure the imports at the top of `service.rs` include `Participant` and `ParticipantRole` from `mur_common::channel` (add to the existing `use` if missing).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-channel add_remove_participant_and_delete_channel`
Expected: PASS.

- [ ] **Step 5: Verify build + lint**

Run: `cargo clippy -p mur-channel -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-channel/src/store.rs mur-channel/src/index.rs mur-channel/src/service.rs
git commit -m "feat(channel): add/remove participant + delete_channel primitives"
```

---

## Task 8: `mur fleet add` / `mur fleet remove`

**Files:**
- Create: `mur-core/src/cmd/fleet/roster.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (`pub mod roster;`)
- Modify: `mur-core/src/cli/actions.rs` (`Add`, `Remove`)
- Modify: `mur-core/src/dispatch.rs`

**Interfaces:**
- Consumes: `store::{load_fleet, save_fleet}`, `crate::a2a_dial::canonicalize_agent_name`, `mur_channel::ChannelService::{add_participant, remove_participant}`, `mur_common::channel::ParticipantRole`.
- Produces: `roster::cmd_fleet_add(home, name, agents: Vec<String>) -> Result<()>`; `roster::cmd_fleet_remove(home, name, agents: Vec<String>) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/roster.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{create, store};

    #[test]
    fn add_then_remove_member_syncs_fleet_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("g".into())).unwrap();

        cmd_fleet_add(home, "dev", vec!["qa".into()]).unwrap();
        cmd_fleet_add(home, "dev", vec!["qa".into()]).unwrap(); // idempotent
        let f = store::load_fleet(home, "dev").unwrap();
        assert_eq!(f.members.iter().filter(|m| *m == "qa").count(), 1);

        cmd_fleet_remove(home, "dev", vec!["qa".into()]).unwrap();
        let f = store::load_fleet(home, "dev").unwrap();
        assert!(!f.members.contains(&"qa".to_string()));
    }

    #[test]
    fn remove_router_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // router defaults to the concierge "mur"; make it a member too
        create::cmd_fleet_create(home, "dev", vec!["mur".into(), "pm".into()], None, Some("g".into())).unwrap();
        assert!(cmd_fleet_remove(home, "dev", vec!["mur".into()]).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core add_then_remove_member_syncs remove_router_is_refused`
Expected: FAIL — `cannot find function cmd_fleet_add`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-core/src/cmd/fleet/roster.rs`:

```rust
//! `mur fleet add` / `mur fleet remove` — mutate membership in BOTH the fleet
//! manifest and the shared channel `fleet-<name>` so they never drift.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::channel::ParticipantRole;

use super::store;

/// Add one or more agents as Delegate members. Idempotent per agent.
pub fn cmd_fleet_add(mur_home: &Path, name: &str, agents: Vec<String>) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;
    let svc = mur_channel::ChannelService::open(mur_home)?;
    for raw in agents {
        let agent = crate::a2a_dial::canonicalize_agent_name(mur_home, &raw);
        if fleet.members.contains(&agent) {
            println!("'{agent}' is already a member of '{name}'.");
            continue;
        }
        svc.add_participant(&fleet.channel_id, &agent, ParticipantRole::Delegate)?;
        fleet.members.push(agent.clone());
        println!("Added '{agent}' to fleet '{name}'.");
    }
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}

/// Remove one or more agents. Refuses the current router; no-ops on non-members.
pub fn cmd_fleet_remove(mur_home: &Path, name: &str, agents: Vec<String>) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;
    let router = fleet.router_or_concierge().to_string();
    let svc = mur_channel::ChannelService::open(mur_home)?;
    for raw in agents {
        let agent = crate::a2a_dial::canonicalize_agent_name(mur_home, &raw);
        if agent == router {
            bail!("router '{agent}' cannot be removed from '{name}'; set a new router first");
        }
        if !fleet.members.contains(&agent) {
            println!("'{agent}' is not a member of '{name}'.");
            continue;
        }
        svc.remove_participant(&fleet.channel_id, &agent)?;
        fleet.members.retain(|m| m != &agent);
        println!("Removed '{agent}' from fleet '{name}'.");
    }
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod roster;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core add_then_remove_member_syncs remove_router_is_refused`
Expected: PASS (2 tests).

- [ ] **Step 5: Wire the CLI**

In `mur-core/src/cli/actions.rs`, add to `enum FleetAction`:

```rust
    /// Add agent(s) to a fleet (member + channel role)
    Add {
        /// Fleet name
        name: String,
        /// Agent name(s) to add
        #[arg(required = true)]
        agents: Vec<String>,
    },
    /// Remove agent(s) from a fleet (member + channel)
    Remove {
        /// Fleet name
        name: String,
        /// Agent name(s) to remove
        #[arg(required = true)]
        agents: Vec<String>,
    },
```

In `mur-core/src/dispatch.rs`:

```rust
                FleetAction::Add { name, agents } => {
                    cmd::fleet::roster::cmd_fleet_add(&mur_home, &name, agents)?
                }
                FleetAction::Remove { name, agents } => {
                    cmd::fleet::roster::cmd_fleet_remove(&mur_home, &name, agents)?
                }
```

- [ ] **Step 6: Verify build + lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/roster.rs mur-core/src/cmd/fleet/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): add/remove members (fleet manifest + channel in sync)"
```

---

## Task 9: `mur fleet delete`

**Files:**
- Create: `mur-core/src/cmd/fleet/delete.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (`pub mod delete;`)
- Modify: `mur-core/src/cli/actions.rs` (`Delete`)
- Modify: `mur-core/src/dispatch.rs`

**Interfaces:**
- Consumes: `store::{load_fleet, fleet_dir}`, `mur_channel::ChannelService::delete_channel`.
- Produces: `delete::cmd_fleet_delete(home, name, yes: bool) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/delete.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{create, store};

    #[test]
    fn delete_removes_fleet_dir_and_channel_keeps_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("g".into())).unwrap();
        assert!(store::fleet_path(home, "dev").exists());
        let svc = mur_channel::ChannelService::open(home).unwrap();
        assert!(svc.store().load_manifest("fleet-dev").is_ok());

        cmd_fleet_delete(home, "dev", true).unwrap();

        assert!(!store::fleet_dir(home, "dev").exists(), "fleet dir must be gone");
        let svc = mur_channel::ChannelService::open(home).unwrap();
        assert!(svc.store().load_manifest("fleet-dev").is_err(), "channel must be gone");
    }

    #[test]
    fn delete_unknown_fleet_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(cmd_fleet_delete(tmp.path(), "nope", true).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core delete_removes_fleet_dir delete_unknown_fleet`
Expected: FAIL — `cannot find function cmd_fleet_delete`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-core/src/cmd/fleet/delete.rs`:

```rust
//! `mur fleet delete <name> [--yes]` — remove the fleet dir (manifest, jobs,
//! sentinels) AND the shared channel `fleet-<name>`. Member agents are NEVER
//! touched — they are independent, shared resources.

use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};

use super::store;

pub fn cmd_fleet_delete(mur_home: &Path, name: &str, yes: bool) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?; // validates name + existence
    if !yes && !confirm(name)? {
        println!("Aborted.");
        return Ok(());
    }
    // Channel first (its own audit history goes with it), then the fleet dir.
    let svc = mur_channel::ChannelService::open(mur_home)?;
    svc.delete_channel(&fleet.channel_id)?;
    let dir = store::fleet_dir(mur_home, name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    println!("Deleted fleet '{name}' and its channel '{}'. Member agents were left intact.", fleet.channel_id);
    Ok(())
}

/// Interactive y/N confirmation (skipped with --yes). Destructive: also removes
/// the channel's audit history.
fn confirm(name: &str) -> Result<bool> {
    print!("Delete fleet '{name}' and its channel history? Members are NOT deleted. [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod delete;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core delete_removes_fleet_dir delete_unknown_fleet`
Expected: PASS (2 tests). (Tests pass `yes=true`, so `confirm` is not exercised by stdin.)

- [ ] **Step 5: Wire the CLI**

In `mur-core/src/cli/actions.rs`, add to `enum FleetAction`:

```rust
    /// Delete a fleet + its shared channel (members are NOT deleted)
    Delete {
        /// Fleet name
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
```

In `mur-core/src/dispatch.rs`:

```rust
                FleetAction::Delete { name, yes } => {
                    cmd::fleet::delete::cmd_fleet_delete(&mur_home, &name, yes)?
                }
```

- [ ] **Step 6: Verify build + lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/delete.rs mur-core/src/cmd/fleet/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): mur fleet delete (fleet dir + channel; members untouched)"
```

---

## Task 10: End-to-end round-trip test (function-level, hermetic)

**Files:**
- Modify: `mur-core/tests/cli_fleet.rs` (append one `#[test]`)

**Interfaces:**
- Consumes the public command functions (`mur_core::cmd::fleet::{create, jobs, roster, delete, store}`) directly with a temp home. This matches the codebase's existing pattern — every `cmd_fleet_*` unit test calls the function with a temp home; there is **no** binary-spawning harness (`assert_cmd`/`predicates` are not dependencies). The test asserts on persisted state, not captured stdout, and does **not** call `run`/`run --loop` (those need live member agents — covered by the manual smoke below).

- [ ] **Step 1: Write the failing test**

Append to `mur-core/tests/cli_fleet.rs`:

```rust
// ── Fleet command round-trip (job intake + roster management) ─────────

#[test]
fn fleet_job_and_roster_round_trip() {
    use mur_common::fleet::JobStatus;
    use mur_core::cmd::fleet::{create, delete, jobs, roster, store};

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // create (members need not exist on disk; names canonicalize to themselves)
    create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("standing".into()))
        .unwrap();

    // add a member → fleet manifest + channel stay in sync
    roster::cmd_fleet_add(home, "dev", vec!["qa".into()]).unwrap();
    assert!(store::load_fleet(home, "dev").unwrap().members.contains(&"qa".to_string()));

    // send a job → it lands queued (no execution)
    jobs::cmd_fleet_send(home, "dev", "first job").unwrap();
    let q = jobs::list_jobs(home, "dev").unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].status, JobStatus::Queued);
    assert_eq!(q[0].text, "first job");

    // remove the member
    roster::cmd_fleet_remove(home, "dev", vec!["qa".into()]).unwrap();
    assert!(!store::load_fleet(home, "dev").unwrap().members.contains(&"qa".to_string()));

    // delete the fleet → fleet dir + channel gone
    delete::cmd_fleet_delete(home, "dev", true).unwrap();
    assert!(store::list_fleets(home).unwrap().is_empty());
    let svc = mur_channel::ChannelService::open(home).unwrap();
    assert!(svc.store().load_manifest("fleet-dev").is_err());
}
```

(Confirm `mur-channel` is a dev-dependency of `mur-core`; the existing tests already use `mur_common`/`mur_core` — if `mur_channel` isn't in `[dev-dependencies]`, add it. It is already a normal dependency of `mur-core`, so it resolves.)

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core fleet_job_and_roster_round_trip`
Expected: FAIL — won't compile until Tasks 2/3/8/9 land the `jobs`/`roster`/`delete` functions (or, if run after them, passes).

- [ ] **Step 3: Make it pass**

With Tasks 1–9 implemented, re-run until green.

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core fleet_job_and_roster_round_trip`
Expected: PASS.

- [ ] **Step 4: Full fleet test sweep + lint**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(fleet)'`
Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings && cargo fmt --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add mur-core/tests/cli_fleet.rs
git commit -m "test(fleet): end-to-end send/jobs/add/remove/delete round-trip"
```

---

## Final verification

- [ ] **Workspace build + lint:** `ORT_STRATEGY=download cargo clippy --workspace -- -D warnings && cargo fmt --check`
- [ ] **Fleet + channel + common test sweep:** `ORT_STRATEGY=download cargo nextest run -p mur-common -p mur-channel && ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(fleet)'`
- [ ] **Manual smoke (operator):**
  - `mur fleet create demo --members pm,qa --goal "standing mission"`
  - `mur fleet send demo "actually do X"` → `mur fleet jobs demo` (shows queued)
  - `mur fleet run demo` → drains the job; `mur fleet jobs demo --all` shows it `done` with a `run_id`
  - `mur fleet list` → aligned table, JOBS column reflects the queue
  - `mur fleet add demo repomanager` / `mur fleet remove demo qa` → `mur fleet show demo` reflects roster
  - `mur fleet delete demo --yes` → gone; agents `pm`/`qa` still exist (`mur agent list`)

## Docs to update after merge

- `CLAUDE.md` fleet section — add `send` / `jobs` / `add` / `remove` / `delete` to the command list; note "jobs replace hand-editing `goal`; `goal` is the standing default".
- The fleet design spec is already committed (`docs/superpowers/specs/2026-06-24-mur-fleet-job-intake-and-management-design.md`).
