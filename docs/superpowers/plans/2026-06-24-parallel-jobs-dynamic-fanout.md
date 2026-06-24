# Parallel Jobs — Dynamic Fan-out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the concierge fan N distinct ad-hoc jobs out to running agents over an ephemeral in-memory DAG — no per-job YAML/skill — via one `parallel_jobs` MCP tool.

**Architecture:** Generalize the existing fleet-broadcast machinery. A new `build_jobs_procedure` builds an in-memory `Procedure` of rank-0 `delegate_to` steps (one per job, per-job prompt + free assignee) and runs it through the unchanged `execute_dag`. A `max_concurrency` option (semaphore) bounds total in-flight steps. A `parallel_jobs` MCP tool resolves assignees and calls a thin `run_parallel_jobs` entry that mints a throwaway channel and invokes `execute_dag`.

**Tech Stack:** Rust (edition 2024), tokio (`spawn` + `Semaphore`), `mur-core` (`executor::dag`, `a2a_dial`), `mur-channel` (`ChannelService`), `mur-mcp-server` (stdio JSON-RPC tools), `serde_json`.

## Global Constraints

- **Rust edition 2024** — `let` chains stable.
- **No hardcoded values** — fan-out width / concurrency ceilings are **named `const`s** (input guardrails, not config wiring; see spec §3).
- **Single source file ≤ 800 lines.** `dag.rs` is already large; do **not** add the new builder to it — new `executor/jobs.rs`.
- **Brand is uppercase "MUR"** in any user-facing string (the tool `description`). Tool `name` stays lowercase `parallel_jobs`.
- **Fail-closed:** `yes` defaults to `false`; risk-tiered steps still hit the existing HITL gate.
- **mur-core tests need `ORT_STRATEGY=download`** and run under **`cargo nextest`**, not `cargo test` (plain `cargo test --workspace` fails ~7 mur-core tests spuriously). All test commands below use nextest.
- **Backward compatibility:** `max_concurrency: None` must preserve today's unbounded executor behaviour exactly — every existing `DagExecOptions` caller is unchanged.

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `mur-core/src/executor/dag.rs` | DAG executor | Add `max_concurrency: Option<usize>` to `DagExecOptions` (3 sites) + a `Semaphore` in the rank loop. |
| `mur-core/src/executor/jobs.rs` | **New.** Ephemeral job fan-out primitive + entry point. | `Job`, `build_jobs_procedure`, `RawJob`, `resolve_jobs`, `run_parallel_jobs` + unit tests. |
| `mur-core/src/executor/mod.rs` | executor module index | Add `pub mod jobs;`. |
| `mur-mcp-server/src/tools.rs` | MCP tool list + dispatch | Add `parallel_jobs` to `all_tools()` and a dispatch arm. |
| `mur-mcp-server/tests/integration.rs` | MCP integration tests | Bump tool count; add validation-path test. |

**Why `executor/jobs.rs` and not `cmd/fleet/run.rs`:** the MCP crate calls `mur_core::executor::jobs::…` (cross-crate, needs `pub`), and `dag.rs` is already >800 lines. `executor` is `pub` (lib.rs:33). `build_fleet_procedure` is left untouched — its mapping (`intent = goal`, `description = "{member}: {goal}"`) differs from per-job (`intent == description`), so sharing would silently change the fleet delegate prompt.

---

### Task 1: Bounded concurrency (`max_concurrency` on the DAG executor)

**Files:**
- Modify: `mur-core/src/executor/dag.rs` (struct `:27-47`, `Default` `:49-62`, spawn loop `:752-797`)
- Test: `mur-core/src/executor/dag.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `DagExecOptions.max_concurrency: Option<usize>` — `None` = unbounded (default); `Some(n)` bounds total concurrent steps to `n.max(1)`. Consumed by Task 4.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `mur-core/src/executor/dag.rs`:

```rust
    #[tokio::test]
    async fn max_concurrency_bounds_parallel_steps() {
        // 6 independent rank-0 steps, each sleeping 0.2s.
        let tmp = tempfile::TempDir::new().unwrap();
        let proc = Procedure {
            variables: vec![],
            steps: (0..6)
                .map(|i| ProcedureStep {
                    description: format!("s{i}"),
                    command: Some("sleep 0.2".to_string()),
                    id: Some(format!("s{i}")),
                    ..Default::default()
                })
                .collect(),
        };

        // Bounded to 2 -> at least 3 sequential waves -> >= ~0.6s.
        let opts = DagExecOptions {
            max_concurrency: Some(2),
            ..Default::default()
        };
        let t = std::time::Instant::now();
        execute_dag(tmp.path(), "cc-bounded", &proc, &opts)
            .await
            .unwrap();
        let bounded = t.elapsed();
        assert!(
            bounded.as_millis() >= 400,
            "bounded run finished too fast ({bounded:?}); cap not applied"
        );

        // Unbounded -> single wave -> ~0.2s.
        let opts2 = DagExecOptions {
            max_concurrency: None,
            ..Default::default()
        };
        let t2 = std::time::Instant::now();
        execute_dag(tmp.path(), "cc-unbounded", &proc, &opts2)
            .await
            .unwrap();
        let unbounded = t2.elapsed();
        assert!(
            unbounded.as_millis() < 400,
            "unbounded run too slow ({unbounded:?}); regression in default path"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core max_concurrency_bounds_parallel_steps`
Expected: **compile error** — `DagExecOptions` has no field `max_concurrency`.

- [ ] **Step 3: Add the struct field (site a)**

In `mur-core/src/executor/dag.rs`, inside `pub struct DagExecOptions<'a>` (after the `run_id` field, ~`:46`):

```rust
    /// Cap on the number of steps running concurrently across the whole DAG.
    /// `None` = unbounded (every same-rank step spawned at once — prior
    /// behaviour). `Some(n)` bounds total in-flight steps to `n.max(1)` via a
    /// shared semaphore. The 2026 dynamic-fan-out hard precondition: cap
    /// concurrency, not just cost, or parallel delegations cascade past API
    /// rate limits.
    pub max_concurrency: Option<usize>,
```

- [ ] **Step 4: Add the `Default` field (site b)**

In `impl<'a> Default for DagExecOptions<'a>` (after `run_id: String::new(),` ~`:59`):

```rust
            max_concurrency: None,
```

- [ ] **Step 5: Add a semaphore and fix the spawn-loop literal (site c)**

In `execute_dag`, immediately after `let mut overall_tokens: u64 = 0;` (~`:752`), add:

```rust
    // Optional global concurrency cap. One semaphore for the whole run bounds
    // total in-flight steps (across all ranks). `None` => no permit, unbounded.
    let sem = opts
        .max_concurrency
        .map(|n| std::sync::Arc::new(tokio::sync::Semaphore::new(n.max(1))));
```

Then inside the `for &i in &indices {` loop, just before `let mh = mur_home.to_path_buf();` (~`:783`), add:

```rust
            let sem = sem.clone();
```

And replace the spawned closure (the `tokio::task::spawn(async move { … })` at `:784-796`) with:

```rust
            handles.push(tokio::task::spawn(async move {
                // Hold a permit for the whole step when a cap is set.
                let _permit = match sem {
                    Some(s) => Some(s.acquire_owned().await.expect("semaphore open")),
                    None => None,
                };
                let opts_clone = DagExecOptions {
                    yes: opt_yes,
                    input: inp,
                    env_class_override: env_override.as_deref(),
                    variables: vars,
                    device_id: dev_id,
                    trigger: &tr,
                    channel_id: chan_id,
                    run_id,
                    max_concurrency: None,
                };
                execute_step(&step, &opts_clone, i, 0, &mh).await
            }));
```

> If compilation fails on `tokio::sync::Semaphore`, add `"sync"` to the `tokio` features in `mur-core/Cargo.toml`.

- [ ] **Step 6: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core max_concurrency_bounds_parallel_steps`
Expected: **PASS** (bounded run ≥ 400ms, unbounded < 400ms).

- [ ] **Step 7: Verify no existing caller broke**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core executor::`
Expected: **PASS** — all existing dag/pipeline tests green (the spawn-loop literal and both fleet callers compile; `..Default::default()` supplies `max_concurrency: None`).

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/executor/dag.rs mur-core/Cargo.toml
git commit -m "feat(executor): optional max_concurrency cap on DagExecOptions

Semaphore-bounded total in-flight steps; None preserves unbounded behaviour.
Hard precondition for safe parallel fan-out (2026 best practice)."
```

---

### Task 2: `Job` + `build_jobs_procedure`

**Files:**
- Create: `mur-core/src/executor/jobs.rs`
- Modify: `mur-core/src/executor/mod.rs`
- Test: `mur-core/src/executor/jobs.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub struct Job { pub description: String, pub assignee: String }`
  - `pub fn build_jobs_procedure(jobs: &[Job]) -> Procedure` — one rank-0 `ProcedureStep` per job: `intent = Some(description)` (the delegate prompt, `dag.rs:474`), `description = description` (channel/ledger/failure labels), `id = "job-{i}"` (stable+unique), `depends_on = []`.
- Consumed by Tasks 3 & 4.

- [ ] **Step 1: Register the module**

In `mur-core/src/executor/mod.rs`, add after `pub mod pipeline;`:

```rust
pub mod jobs;
```

- [ ] **Step 2: Create the file with the failing test**

Create `mur-core/src/executor/jobs.rs`:

```rust
//! Ephemeral parallel-jobs fan-out: build an in-memory DAG of rank-0
//! `delegate_to` steps (one per job) and run it through `execute_dag` — no
//! authored workflow/skill file. Generalizes fleet broadcast (`cmd/fleet/run.rs`)
//! to per-job prompts with a free assignee. See
//! `docs/superpowers/specs/2026-06-24-parallel-jobs-dynamic-fanout-design.md`.

use mur_common::skill::manifest::{Procedure, ProcedureStep};

/// A single job: a prompt and the (canonicalized) agent to delegate it to.
pub struct Job {
    pub description: String,
    pub assignee: String,
}

/// One rank-0 `ProcedureStep` per job (all parallel, no deps). Sets BOTH
/// `intent` (the delegate prompt) and `description` (channel/ledger labels)
/// to the job text, and a stable unique `id` for idempotency / crash-resume.
pub fn build_jobs_procedure(jobs: &[Job]) -> Procedure {
    Procedure {
        variables: vec![],
        steps: jobs
            .iter()
            .enumerate()
            .map(|(i, j)| ProcedureStep {
                description: j.description.clone(),
                intent: Some(j.description.clone()),
                delegate_to: Some(j.assignee.clone()),
                id: Some(format!("job-{i}")),
                ..Default::default()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_jobs_procedure_one_rank0_step_per_job() {
        let jobs = vec![
            Job { description: "add caching to fetch".into(), assignee: "rustsmith".into() },
            Job { description: "write the README".into(), assignee: "frontend".into() },
        ];
        let p = build_jobs_procedure(&jobs);
        assert_eq!(p.steps.len(), 2);
        // delegate target per job
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("rustsmith"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("frontend"));
        // BOTH intent (prompt) and description (labels) carry the job text
        assert_eq!(p.steps[0].intent.as_deref(), Some("add caching to fetch"));
        assert_eq!(p.steps[0].description, "add caching to fetch");
        // stable, unique ids
        assert_eq!(p.steps[0].id.as_deref(), Some("job-0"));
        assert_eq!(p.steps[1].id.as_deref(), Some("job-1"));
        // all rank-0 (no dependencies => all parallel)
        assert!(p.steps.iter().all(|s| s.depends_on.is_empty()));
    }
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core build_jobs_procedure_one_rank0_step_per_job`
Expected: **PASS** (the implementation is in the same file — this step proves the module compiles and the shape is correct).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/executor/jobs.rs mur-core/src/executor/mod.rs
git commit -m "feat(executor): build_jobs_procedure ephemeral fan-out primitive"
```

---

### Task 3: `RawJob` + `resolve_jobs` (assignee resolution)

**Files:**
- Modify: `mur-core/src/executor/jobs.rs`
- Test: `mur-core/src/executor/jobs.rs`

**Interfaces:**
- Consumes: `Job` (Task 2), `crate::a2a_dial::canonicalize_agent_name(home, typed) -> String`.
- Produces:
  - `pub struct RawJob { pub description: String, pub agent: Option<String> }`
  - `pub fn resolve_jobs(mur_home: &Path, raw: &[RawJob], default_agent: Option<&str>) -> anyhow::Result<Vec<Job>>` — per job: `agent` → else `default_agent` → else error; rejects empty descriptions; canonicalizes assignee names.
- Consumed by Task 5 (the MCP handler).

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests` in `mur-core/src/executor/jobs.rs`:

```rust
    #[test]
    fn resolve_jobs_precedence_and_validation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();

        // per-job agent wins over the default
        let raw = vec![RawJob { description: "A".into(), agent: Some("rustsmith".into()) }];
        let jobs = resolve_jobs(home, &raw, Some("frontend")).unwrap();
        assert_eq!(jobs[0].assignee, "rustsmith");

        // falls back to the default agent when a job omits its own
        let raw = vec![RawJob { description: "B".into(), agent: None }];
        let jobs = resolve_jobs(home, &raw, Some("frontend")).unwrap();
        assert_eq!(jobs[0].assignee, "frontend");

        // error when neither a per-job nor a default agent is set
        let raw = vec![RawJob { description: "C".into(), agent: None }];
        assert!(resolve_jobs(home, &raw, None).is_err());

        // error on an empty description
        let raw = vec![RawJob { description: "  ".into(), agent: Some("rustsmith".into()) }];
        assert!(resolve_jobs(home, &raw, None).is_err());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core resolve_jobs_precedence_and_validation`
Expected: **compile error** — `RawJob` / `resolve_jobs` not found.

- [ ] **Step 3: Implement**

In `mur-core/src/executor/jobs.rs`, extend the top-of-file `use` lines to:

```rust
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use crate::a2a_dial::canonicalize_agent_name;
```

Then add, after `build_jobs_procedure`:

```rust
/// Untyped job as it arrives from the MCP tool: a description and an optional
/// explicit assignee. Resolved into a `Job` by `resolve_jobs`.
pub struct RawJob {
    pub description: String,
    pub agent: Option<String>,
}

/// Resolve each `RawJob` to a `Job` with a concrete, canonicalized assignee.
/// Precedence per job: explicit `agent` -> `default_agent` -> error.
/// Rejects empty descriptions. Names are canonicalized so the runtime
/// spoof check passes (case-insensitive on-disk match, else used verbatim).
pub fn resolve_jobs(
    mur_home: &Path,
    raw: &[RawJob],
    default_agent: Option<&str>,
) -> Result<Vec<Job>> {
    if raw.is_empty() {
        bail!("no jobs provided");
    }
    raw.iter()
        .enumerate()
        .map(|(i, j)| {
            if j.description.trim().is_empty() {
                bail!("job {i} has an empty description");
            }
            let assignee = j
                .agent
                .as_deref()
                .or(default_agent)
                .ok_or_else(|| {
                    anyhow!("job {i} has no assignee: pass per-job `agent` or a top-level default `agent`")
                })?;
            Ok(Job {
                description: j.description.clone(),
                assignee: canonicalize_agent_name(mur_home, assignee),
            })
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core resolve_jobs_precedence_and_validation`
Expected: **PASS** (canonicalize returns the typed name verbatim in an empty temp home — no `agents/` dir).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/executor/jobs.rs
git commit -m "feat(executor): resolve_jobs assignee resolution + validation"
```

---

### Task 4: `run_parallel_jobs` entry point

**Files:**
- Modify: `mur-core/src/executor/jobs.rs`
- Test: `mur-core/src/executor/jobs.rs`

**Interfaces:**
- Consumes: `build_jobs_procedure` (Task 2); `mur_channel::ChannelService::{open, create_for_workflow}`; `crate::executor::dag::{execute_dag, DagExecOptions}` (incl. `max_concurrency` from Task 1).
- Produces: `pub async fn run_parallel_jobs(mur_home: &Path, jobs: &[Job], max_concurrency: Option<usize>, yes: bool) -> anyhow::Result<(String, PipelineOutput)>` — mints a throwaway channel, runs the ephemeral DAG, returns `(channel_id, output)`.
- Consumed by Task 5.

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests` in `mur-core/src/executor/jobs.rs`:

```rust
    #[tokio::test]
    async fn run_parallel_jobs_mints_channel_even_when_delegate_unreachable() {
        // No runtime is running, so the delegate dial fails fast (RequireRunning).
        // run_parallel_jobs must still mint the channel and return Ok (the
        // executor turns a failed delegate into a failed step, not an Err).
        let tmp = tempfile::TempDir::new().unwrap();
        let jobs = vec![Job {
            description: "do A".into(),
            assignee: "nonexistent-agent-xyz".into(),
        }];
        let (channel_id, _out) = run_parallel_jobs(tmp.path(), &jobs, Some(2), false)
            .await
            .expect("must not error when the delegate is unreachable");
        assert!(!channel_id.is_empty(), "a channel should have been minted");
        // The minted channel is persisted and loadable.
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        assert!(svc.load_events(&channel_id).is_ok());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core run_parallel_jobs_mints_channel`
Expected: **compile error** — `run_parallel_jobs` not found.

- [ ] **Step 3: Implement**

In `mur-core/src/executor/jobs.rs`, extend the `use` block to add:

```rust
use mur_channel::ChannelService;
use mur_common::pipeline::PipelineOutput;

use crate::executor::dag::{DagExecOptions, execute_dag};
```

Then add, after `resolve_jobs`:

```rust
/// Run N jobs as one ephemeral, channel-recorded DAG. Mints a throwaway
/// workflow channel, fans the jobs out (bounded by `max_concurrency`), and
/// returns `(channel_id, output)`. Per-job replies are persisted on the
/// channel; the caller reads them back via `channel_id`. `yes` is passed
/// straight through — `false` keeps risk-tiered steps fail-closed at the HITL gate.
pub async fn run_parallel_jobs(
    mur_home: &Path,
    jobs: &[Job],
    max_concurrency: Option<usize>,
    yes: bool,
) -> Result<(String, PipelineOutput)> {
    let proc = build_jobs_procedure(jobs);
    let svc = ChannelService::open(mur_home)?;
    let channel_id = svc.create_for_workflow("parallel-jobs")?.id;
    let opts = DagExecOptions {
        yes,
        trigger: "agent",
        channel_id: Some(channel_id.clone()),
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        max_concurrency,
        ..Default::default()
    };
    let out = execute_dag(mur_home, "parallel-jobs", &proc, &opts).await?;
    Ok((channel_id, out))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core run_parallel_jobs_mints_channel`
Expected: **PASS** (channel minted; delegate-unreachable becomes a failed step inside `Ok`, not an `Err`).

- [ ] **Step 5: Run the whole jobs module + clippy**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core executor::jobs`
Expected: **PASS** (all 3 jobs tests).

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/executor/jobs.rs
git commit -m "feat(executor): run_parallel_jobs entry — ephemeral channel + DAG fan-out"
```

---

### Task 5: `parallel_jobs` MCP tool

**Files:**
- Modify: `mur-mcp-server/src/tools.rs` (`all_tools()` ~`:48-340`, `dispatch_tool()` ~`:384-683`)
- Modify: `mur-mcp-server/tests/integration.rs` (tool-count assert ~`:80`)
- Test: `mur-mcp-server/tests/integration.rs`

**Interfaces:**
- Consumes: `mur_core::executor::jobs::{RawJob, resolve_jobs, run_parallel_jobs}` (Tasks 3-4); `resolve_mur_home()` (existing util, `tools.rs:685`).
- Produces: MCP tool `parallel_jobs` returning `{ "channel_id": String, "output": String }`.

- [ ] **Step 1: Bump the integration tool-count assertion (failing test first)**

In `mur-mcp-server/tests/integration.rs`, change the existing assertion (currently `assert_eq!(tools.len(), 18, "Expected 18 tools");`) to:

```rust
    assert_eq!(tools.len(), 19, "Expected 19 tools");
```

- [ ] **Step 2: Add the validation-path integration test**

Append to `mur-mcp-server/tests/integration.rs` (mirror the `calls_mur_compress_tool` setup):

```rust
#[test]
fn parallel_jobs_rejects_empty_jobs() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
    );
    let _ = read_response(&mut stdout);
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    // Empty jobs array -> tool returns an error envelope (isError), never panics.
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"parallel_jobs","arguments":{"jobs":[],"agent":"rustsmith"}}}"#,
    );
    let resp = read_response(&mut stdout);
    let resp_str = serde_json::to_string(&resp).unwrap();
    assert!(
        resp_str.contains("isError") || resp_str.to_lowercase().contains("error"),
        "empty jobs should yield an error envelope: {resp_str}"
    );

    child.kill().ok();
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p mur-mcp-server parallel_jobs_rejects_empty_jobs test_initialize_and_list_tools`
Expected: **FAIL** — count assert fails (still 18) / `parallel_jobs` unknown tool.

- [ ] **Step 4: Register the tool schema**

In `mur-mcp-server/src/tools.rs`, add this `Tool { … }` to the vec returned by `all_tools()` (place it alongside the other tools, before the closing `]`):

```rust
        Tool {
            name: "parallel_jobs".into(),
            description: "Fan out N distinct jobs to running MUR agents in parallel over an ephemeral channel — no workflow file. Each job is delegated as its own concurrent turn. Before coding fan-out, apply the parallel-code gate: disjoint files (no shared registry/lockfile), contracts frozen first, one writer per file. Targets the agents you name; runtimes must already be running.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("jobs".into(), ToolParam {
                        param_type: "array".into(),
                        description: "Jobs to run in parallel. Each: { description: string, agent?: string }.".into(),
                        default: None,
                    }),
                    ("agent".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Default assignee agent name for jobs that omit their own `agent`.".into(),
                        default: None,
                    }),
                    ("max_concurrency".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Max jobs in flight at once, 1-32 (default 8).".into(),
                        default: Some(json!(8)),
                    }),
                    ("yes".into(), ToolParam {
                        param_type: "boolean".into(),
                        description: "Auto-approve risk-tiered steps. Default false (fail-closed).".into(),
                        default: Some(json!(false)),
                    }),
                ])),
                required: Some(vec!["jobs".into()]),
            },
        },
```

- [ ] **Step 5: Add the dispatch arm**

In `mur-mcp-server/src/tools.rs`, add a match arm inside `dispatch_tool` (before the `_ => Err(...)` fallback):

```rust
        "parallel_jobs" => {
            // Input guardrails (not behaviour config — see spec §3).
            const MAX_JOBS: usize = 32;
            const DEFAULT_MAX_CONCURRENCY: u64 = 8;

            let raw = arguments
                .get("jobs")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "Missing required parameter: 'jobs' (array)".to_string())?;
            if raw.is_empty() || raw.len() > MAX_JOBS {
                return Err(format!(
                    "'jobs' must have 1..={MAX_JOBS} entries (got {})",
                    raw.len()
                ));
            }
            let jobs_in: Vec<mur_core::executor::jobs::RawJob> = raw
                .iter()
                .map(|j| mur_core::executor::jobs::RawJob {
                    description: j
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    agent: j
                        .get("agent")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                })
                .collect();
            let default_agent = arguments.get("agent").and_then(|v| v.as_str());
            let max_concurrency = arguments
                .get("max_concurrency")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_MAX_CONCURRENCY)
                .clamp(1, MAX_JOBS as u64) as usize;
            let yes = arguments
                .get("yes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let home = resolve_mur_home().map_err(|e| format!("parallel_jobs failed: {e}"))?;
            let jobs = mur_core::executor::jobs::resolve_jobs(&home, &jobs_in, default_agent)
                .map_err(|e| format!("parallel_jobs: {e}"))?;
            let (channel_id, out) =
                mur_core::executor::jobs::run_parallel_jobs(&home, &jobs, Some(max_concurrency), yes)
                    .await
                    .map_err(|e| format!("parallel_jobs failed: {e}"))?;
            Ok(json!({
                "channel_id": channel_id,
                "output": out.output_text.unwrap_or_default(),
            }))
        }
```

> `BTreeMap` and `json!` are already imported in `tools.rs`. If `json!` is not in scope in the `all_tools` region, it is `serde_json::json!` (already `use`d for the existing `default: Some(json!(5))`).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p mur-mcp-server parallel_jobs_rejects_empty_jobs test_initialize_and_list_tools`
Expected: **PASS** — 19 tools listed; empty-jobs call returns an error envelope.

- [ ] **Step 7: Clippy the MCP crate**

Run: `cargo clippy -p mur-mcp-server -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add mur-mcp-server/src/tools.rs mur-mcp-server/tests/integration.rs
git commit -m "feat(mcp): parallel_jobs tool — concierge-triggered parallel fan-out

First mutating tool in mur-mcp-server. Resolves assignees, runs an ephemeral
DAG via mur_core::executor::jobs::run_parallel_jobs, returns {channel_id, output}."
```

---

### Task 6: Workspace verification + docs

**Files:**
- Modify: `mur-mcp-server` skill/docs if the tool list is enumerated anywhere user-facing (check only).

- [ ] **Step 1: Full lint + format**

Run: `cargo fmt --check && ORT_STRATEGY=download cargo clippy -p mur-core -p mur-mcp-server -- -D warnings`
Expected: clean. (If `fmt --check` fails, run `cargo fmt` and amend.)

- [ ] **Step 2: Targeted test sweep**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core executor:: && cargo nextest run -p mur-mcp-server`
Expected: **PASS**.

- [ ] **Step 3: Check for a user-facing tool enumeration to update**

Run: `rg -n "mur_project_search|mur_notes_search" --glob '!target' docs README.md mur-mcp-server 2>/dev/null | rg -iv "src/tools.rs|tests/"`
Expected: if any docs file lists the MCP tools, add a one-line `parallel_jobs` entry (uppercase **MUR** in prose). If none, skip.

- [ ] **Step 4: Commit (only if Step 1 or 3 changed files)**

```bash
git add -A && git commit -m "chore: fmt + docs for parallel_jobs tool"
```

---

## Live / Operator verification (post-merge, needs running runtimes + cc-proxy)

Not an automated task — the integration gates from spec §"Integration risks". Run manually:

1. Ensure a target agent runtime is running and fresh (registers `channel/delegate`; the stale-binary `-32601` gotcha) and its `entitlements.filesystem.write` includes `~/.mur/channels` (peer-self-reply lands in the audit trail).
2. From the concierge (or an MCP client), call `parallel_jobs` with 3 distinct jobs to one running agent; confirm 3 concurrent delegate turns and `{channel_id, output}` back, and that per-job replies are on the channel (`mur channel show <id>` or events).
3. Call with two explicitly-named agents (the "jobs across a fleet" v1 shape) — confirm each job lands on its named agent.

---

## Self-Review

**Spec coverage:**
- §1 core primitive → Task 2 (`build_jobs_procedure`) + Task 4 (`run_parallel_jobs`). ✓
- §2 concurrency cap (3 sites + semaphore) → Task 1. ✓
- §3 MCP tool (`{jobs, agent, max_concurrency, yes}` → `{channel_id, output}`, `MAX_JOBS` const) → Task 5. ✓
- §4 assignee resolution (per-job → default → error, canonicalized) → Task 3. ✓
- §5 safety (fail-closed `yes:false`, HITL via executor, disjoint-ownership note in description, input validation) → Tasks 3+5. ✓
- §6 result handling (`{channel_id, output}`, per-step failure isolation via executor) → Tasks 4+5. ✓
- Non-goals (fleet auto-distribute, typed per-job envelope, router, fan-in verify, budget) → not implemented, by design. ✓
- Integration risks (first mutating MCP tool; runtimes-running; channels entitlement) → Live verification section. ✓

**Placeholder scan:** none — every code step shows full code; every run step shows the command + expected output.

**Type consistency:** `Job{description,assignee}`, `RawJob{description,agent}`, `build_jobs_procedure(&[Job])`, `resolve_jobs(&Path,&[RawJob],Option<&str>)->Result<Vec<Job>>`, `run_parallel_jobs(&Path,&[Job],Option<usize>,bool)->Result<(String,PipelineOutput)>`, `DagExecOptions.max_concurrency: Option<usize>` — names/signatures identical across Tasks 1-5. ✓
