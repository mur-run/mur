# Deep Research Run Progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-stage run progress for deep-research: a `.run_progress.json` written by the fleet loop, log-style per-step/per-iteration output, and an in-flight-run block on the bare `mur deep-research` panel.

**Architecture:** A pure progress model (`cmd/fleet/progress.rs`) + an optional step-event observer on the DAG executor + write/print wiring in `loop_run.rs` + a read-only render in `cmd/deep_research/panel.rs`. Single data source, two views (murmur Panel is Phase 2, out of scope).

**Tech Stack:** Rust (edition 2024), serde_json, existing atomic temp+rename write pattern.

**Spec:** `docs/superpowers/specs/2026-07-14-deep-research-run-progress-design.md`

## Global Constraints

- ALL progress-file writes are best-effort: an error must never fail, slow, or change the run (log at `tracing::debug!`, continue).
- Atomic write = temp file + rename (the `store/yaml.rs` / `ModelRegistry::save_to` pattern).
- File: `~/.mur/fleets/<name>/.run_progress.json`, `schema_version: 1`, kept after the run (last-run record), overwritten by the next run.
- Phase classification is a pure heuristic; unclassifiable → `Other`; it never blocks anything.
- No hardcoded values: stale threshold, file name etc. are named consts.
- Existing loop output lines stay byte-identical; new lines are additions only.
- Build env: PATH `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin`, `ORT_STRATEGY=download`, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`; test via `cargo nextest run -p mur-core <filter>`; `cargo fmt` + `cargo clippy -p mur-core -- -D warnings` before every commit.

---

### Task 1: Progress model (`progress.rs`)

**Files:**
- Create: `mur-core/src/cmd/fleet/progress.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add `pub mod progress;` — check the existing mod list with `grep -n "pub mod" mur-core/src/cmd/fleet/mod.rs`)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces (used by Tasks 2–4):
  - `pub const PROGRESS_FILE: &str = ".run_progress.json";`
  - `pub const STALE_AFTER_SECS: u64 = 600;`
  - `#[derive(Serialize, Deserialize)] pub struct RunProgress { pub schema_version: u32, pub run_id: String, pub question: String, pub started_at: String, pub finished_at: Option<String>, pub outcome: Option<String>, pub iteration: u32, pub model: Option<String>, pub budget_usd: Option<f64>, pub spend_usd: f64, pub steps: Vec<StepProgress> }`
  - `#[derive(Serialize, Deserialize)] pub struct StepProgress { pub id: String, pub worker: Option<String>, pub phase: Phase, pub desc: String, pub state: StepState, pub cost_usd: Option<f64>, pub started_at: Option<String>, pub ended_at: Option<String> }`
  - `#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy)] pub enum Phase { Probe, Research, Verify, Synthesize, Other }` (serde rename_all = "snake_case"; same for `StepState { Pending, Running, Done, Failed }`)
  - `pub fn classify_phase(assignment: &str) -> Phase`
  - `pub struct Totals { pub done: usize, pub running: usize, pub pending: usize, pub failed: usize }` + `impl RunProgress { pub fn totals(&self) -> Totals }`
  - `pub fn iteration_summary_line(p: &RunProgress) -> String`
  - `impl RunProgress { pub fn save(&self, mur_home: &Path, fleet: &str) }` (best-effort, atomic; logs debug on error, returns `()`)
  - `pub fn load(mur_home: &Path, fleet: &str) -> Option<(RunProgress, std::time::SystemTime)>` (None if missing/corrupt; SystemTime = file mtime for staleness)
  - `pub fn progress_path(mur_home: &Path, fleet: &str) -> PathBuf` (= `mur_home.join("fleets").join(fleet).join(PROGRESS_FILE)`)

- [ ] **Step 1: Write the failing tests** (bottom of `progress.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_phase_heuristics() {
        assert_eq!(classify_phase("Run a single minimal gateway health probe"), Phase::Probe);
        assert_eq!(classify_phase("Research failure-handling best practices"), Phase::Research);
        assert_eq!(classify_phase("verify s2's claims under correctness lenses"), Phase::Verify);
        assert_eq!(classify_phase("Synthesize s1-s3 findings into a cited report"), Phase::Synthesize);
        assert_eq!(classify_phase("hello world"), Phase::Other);
    }

    fn sample() -> RunProgress {
        RunProgress {
            schema_version: 1,
            run_id: "r1".into(),
            question: "q".into(),
            started_at: "2026-07-14T00:00:00Z".into(),
            finished_at: None,
            outcome: None,
            iteration: 2,
            model: Some("claude_haiku".into()),
            budget_usd: Some(2.0),
            spend_usd: 0.31,
            steps: vec![
                StepProgress { id: "s1".into(), worker: Some("dr_worker_1".into()), phase: Phase::Probe, desc: "probe".into(), state: StepState::Done, cost_usd: Some(0.01), started_at: None, ended_at: None },
                StepProgress { id: "s2".into(), worker: Some("dr_worker_2".into()), phase: Phase::Research, desc: "research".into(), state: StepState::Running, cost_usd: None, started_at: None, ended_at: None },
                StepProgress { id: "s3".into(), worker: None, phase: Phase::Verify, desc: "verify".into(), state: StepState::Pending, cost_usd: None, started_at: None, ended_at: None },
            ],
        }
    }

    #[test]
    fn totals_counts_states() {
        let t = sample().totals();
        assert_eq!((t.done, t.running, t.pending, t.failed), (1, 1, 1, 0));
    }

    #[test]
    fn summary_line_shows_counts_spend_model() {
        let line = iteration_summary_line(&sample());
        assert!(line.contains("iteration 2"));
        assert!(line.contains("1✓"));
        assert!(line.contains("1 pending"));
        assert!(line.contains("$0.31/$2.00"));
        assert!(line.contains("claude_haiku"));
    }

    #[test]
    fn save_load_roundtrip_and_missing_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(tmp.path(), "deep-research").is_none());
        let p = sample();
        p.save(tmp.path(), "deep-research");
        let (loaded, _mtime) = load(tmp.path(), "deep-research").unwrap();
        assert_eq!(loaded.iteration, 2);
        assert_eq!(loaded.steps.len(), 3);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core fleet::progress`
Expected: compile FAIL (module missing) after adding the mod decl.

- [ ] **Step 3: Implement**

```rust
//! Single-source run-progress model for fleet loops (deep-research UX).
//! Pure data + best-effort atomic persistence; consumers render it
//! (loop stdout, `mur deep-research` panel, murmur Panel in Phase 2).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name under `~/.mur/fleets/<name>/`. Kept after the run as the
/// last-run record; overwritten by the next run.
pub const PROGRESS_FILE: &str = ".run_progress.json";
/// An in-flight file whose mtime is older than this is labeled stale
/// (loop probably crashed).
pub const STALE_AFTER_SECS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Probe,
    Research,
    Verify,
    Synthesize,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepProgress {
    pub id: String,
    pub worker: Option<String>,
    pub phase: Phase,
    pub desc: String,
    pub state: StepState,
    pub cost_usd: Option<f64>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProgress {
    pub schema_version: u32,
    pub run_id: String,
    pub question: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// converged | max-iterations | deadline | budget | stopped | stuck | failed
    pub outcome: Option<String>,
    pub iteration: u32,
    pub model: Option<String>,
    pub budget_usd: Option<f64>,
    pub spend_usd: f64,
    pub steps: Vec<StepProgress>,
}

pub struct Totals {
    pub done: usize,
    pub running: usize,
    pub pending: usize,
    pub failed: usize,
}

/// Keyword heuristic over the router's assignment text. Unclassifiable
/// text is `Other` — classification is display-only and never gates the run.
pub fn classify_phase(assignment: &str) -> Phase {
    let a = assignment.to_lowercase();
    if a.contains("probe") || a.contains("health") {
        Phase::Probe
    } else if a.contains("synthesi") || a.contains("report") {
        Phase::Synthesize
    } else if a.contains("verify") || a.contains("refute") || a.contains("confirm") {
        Phase::Verify
    } else if a.contains("research") || a.contains("search") || a.contains("fetch") {
        Phase::Research
    } else {
        Phase::Other
    }
}

impl RunProgress {
    pub fn totals(&self) -> Totals {
        let mut t = Totals { done: 0, running: 0, pending: 0, failed: 0 };
        for s in &self.steps {
            match s.state {
                StepState::Done => t.done += 1,
                StepState::Running => t.running += 1,
                StepState::Pending => t.pending += 1,
                StepState::Failed => t.failed += 1,
            }
        }
        t
    }

    /// Best-effort atomic save; errors are logged at debug and swallowed —
    /// the progress file must never affect the run.
    pub fn save(&self, mur_home: &Path, fleet: &str) {
        let res = (|| -> anyhow::Result<()> {
            let path = progress_path(mur_home, fleet);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(e) = res {
            tracing::debug!("run progress save failed (ignored): {e}");
        }
    }
}

pub fn progress_path(mur_home: &Path, fleet: &str) -> PathBuf {
    mur_home.join("fleets").join(fleet).join(PROGRESS_FILE)
}

/// None on missing/corrupt file (a corrupt progress file is not an error
/// condition anywhere). The mtime feeds the panel's staleness label.
pub fn load(mur_home: &Path, fleet: &str) -> Option<(RunProgress, std::time::SystemTime)> {
    let path = progress_path(mur_home, fleet);
    let body = std::fs::read(&path).ok()?;
    let p: RunProgress = serde_json::from_slice(&body).ok()?;
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some((p, mtime))
}

pub fn iteration_summary_line(p: &RunProgress) -> String {
    let t = p.totals();
    format!(
        "iteration {} done: {}✓ {}✗ {} pending · spend ${:.2}{} · model {}",
        p.iteration,
        t.done,
        t.failed,
        t.pending,
        p.spend_usd,
        p.budget_usd
            .map(|b| format!("/${b:.2}"))
            .unwrap_or_default(),
        p.model.as_deref().unwrap_or("?"),
    )
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core fleet::progress`
Expected: 4 passed.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/fleet/
git commit -m "feat(fleet): run-progress model with atomic best-effort persistence (T1)"
```

---

### Task 2: DAG step-event observer

**Files:**
- Modify: `mur-core/src/executor/dag.rs` (`DagExecOptions` ~line 31; the step spawn/completion paths in `execute_step` / rank loop)

**Interfaces:**
- Consumes: nothing from Task 1 (deliberately decoupled — the observer is generic).
- Produces (Task 3 relies on):
  - `#[derive(Debug, Clone)] pub struct StepEvent { pub id: String, pub agent: Option<String>, pub kind: StepEventKind, pub tokens_used: u64 }`
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub enum StepEventKind { Started, Done, Failed }`
  - `DagExecOptions.on_step: Option<std::sync::Arc<dyn Fn(StepEvent) + Send + Sync>>` (Default: `None`)

Implementation notes (confirm mechanically):
- Fire `Started` right before a step begins executing, `Done`/`Failed` where the executor records the step's `StepResult` (grep `struct StepResult` at dag.rs:253 and the sites constructing it in `execute_step_inner`). `tokens_used` = the per-step value already tracked at dag.rs:262 (0 when absent); `agent` = the delegate target for delegated steps — find the field with `grep -n "agent\|delegate" mur-core/src/executor/dag.rs | head -30` (fleet steps are built in `cmd/fleet/plan.rs` / `build_fleet_procedure` — check which ProcedureStep field carries the member name; if it is encoded in `tool`/params, extract from there; `None` when not a delegation).
- The observer is display-only: call sites must not `?` on it, and a panicking observer must not be possible (it's a plain Fn — document "must not panic" on the field).
- Because ranks run concurrently (`tokio::spawn`), the closure crosses threads — hence `Arc<dyn Fn + Send + Sync>`.
- `DagExecOptions` has a lifetime param `<'a>`; the new field is `'static`-owned (Arc), no lifetime change.

- [ ] **Step 1: Write the failing test** (in dag.rs `#[cfg(test)]`, following existing test style there — find it with `grep -n "mod tests" mur-core/src/executor/dag.rs`)

```rust
#[tokio::test]
async fn on_step_observer_sees_start_and_done() {
    use std::sync::{Arc, Mutex};
    // Two trivial command-mode steps (existing tests show the ProcedureStep
    // construction pattern — reuse it; `echo ok` steps).
    let steps = vec![
        mur_common::skill::manifest::ProcedureStep {
            description: "one".into(),
            tool: Some("bash".into()),
            command: Some("echo one".into()),
            id: Some("s1".into()),
            ..Default::default()
        },
        mur_common::skill::manifest::ProcedureStep {
            description: "two".into(),
            tool: Some("bash".into()),
            command: Some("echo two".into()),
            id: Some("s2".into()),
            ..Default::default()
        },
    ];
    let seen: Arc<Mutex<Vec<(String, StepEventKind)>>> = Arc::new(Mutex::new(vec![]));
    let sink = seen.clone();
    let opts = DagExecOptions {
        on_step: Some(Arc::new(move |e: StepEvent| {
            sink.lock().unwrap().push((e.id, e.kind));
        })),
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let _ = execute_dag(tmp.path(), "test", &steps, &opts).await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen.contains(&("s1".into(), StepEventKind::Started)));
    assert!(seen.contains(&("s1".into(), StepEventKind::Done)));
    assert!(seen.contains(&("s2".into(), StepEventKind::Done)));
}
```

(Adapt the `ProcedureStep` literal to its real fields — check whether `command` exists or commands ride in `tool`/params: `grep -n "command" mur-common/src/skill/manifest.rs | head -5`. If `ProcedureStep` lacks `Default`, construct it the way the existing dag.rs tests do.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core executor::dag::tests::on_step_observer`
Expected: compile FAIL (field/types missing).

- [ ] **Step 3: Implement** — add the types + field + `Default` arm (`on_step: None`) + three firing sites:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepEventKind {
    Started,
    Done,
    Failed,
}

/// Display-only step lifecycle event for progress observers. The callback
/// runs on executor worker tasks: it MUST be cheap and MUST NOT panic.
#[derive(Debug, Clone)]
pub struct StepEvent {
    pub id: String,
    pub agent: Option<String>,
    pub kind: StepEventKind,
    /// Per-step delegate token usage (0 for non-delegate or unknown).
    pub tokens_used: u64,
}
```

Firing pattern at each site (adapt names):

```rust
if let Some(cb) = &opts.on_step {
    cb(StepEvent {
        id: step_id.clone(),
        agent: delegate_agent.clone(),
        kind: StepEventKind::Started,
        tokens_used: 0,
    });
}
```

- [ ] **Step 4: Run tests** — the new test + the whole executor suite:

Run: `cargo nextest run -p mur-core executor::`
Expected: all pass (no behavior change for `on_step: None`).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/executor/dag.rs
git commit -m "feat(executor): optional on_step observer for DAG step lifecycle (T2)"
```

---

### Task 3: Loop wiring — write progress + print step/summary lines

**Files:**
- Modify: `mur-core/src/cmd/fleet/loop_run.rs` (`run_guarded`, iteration body ~lines 398-475)

**Interfaces:**
- Consumes: Task 1 (`RunProgress`, `StepProgress`, `classify_phase`, `iteration_summary_line`, `save`), Task 2 (`on_step`, `StepEvent`, `StepEventKind`), existing `iteration_cost_usd`/`price_per_1k`.
- Produces: the written `.run_progress.json` contract Task 4 reads; no new public API.

Behavior to implement inside `run_guarded`:
1. Before the loop: build `RunProgress` (`run_id` = a uuid for the whole run, `question` = `fleet.goal`, `started_at` = now RFC3339, `model` = first member's `AgentProfile::load(mur_home, name).ok().and_then(|p| p.model_ref)`, `budget_usd` = the effective budget already computed, `spend_usd` = 0, `iteration` = 0, `steps` = empty) wrapped in `std::sync::Arc<std::sync::Mutex<RunProgress>>`; `save` immediately.
2. Iteration start (right after `proc` is obtained): lock, set `iteration = iteration + 1`, APPEND this iteration's steps as `Pending` (`id` = step id or index, `worker` = delegate target if extractable — same source as Task 2's `agent`, `phase` = `classify_phase(&step.description)`, `desc` = first 120 chars of description), save.
3. Build `opts.on_step` closure (clone of the Arc<Mutex>, plus `price_per_1k` copied in): on `Started` → mark step Running + `started_at`, save, no print; on `Done`/`Failed` → mark state, `ended_at`, `cost_usd = Some(iteration_cost_usd(e.tokens_used, price_per_1k))` when `tokens_used > 0`, save, and print one line:
   `println!("  {} {} {} {} {}", mark, e.id, phase_str, worker_str, cost_str)` → rendered like `✓ s2 research dr_worker_2 $0.08` (`✗` for Failed; omit cost when None). Elapsed seconds = from the step's `started_at` if present, appended as `42s`.
4. After `execute_dag` returns and `spent` is updated: lock, set `spend_usd = spent`, save, `println!("{}", iteration_summary_line(&locked))`.
5. On every loop exit path (the `break LoopStop::…` values and the guard-stop return): set `finished_at` + `outcome` (map `LoopStop::Converged→"converged"`, `MaxIterations→"max-iterations"`, `Deadline→"deadline"`, `Stuck→"stuck"`, `BudgetExceeded→"budget"` — check the real variant names at loop_run.rs:35 — commander/stopped variants likewise), save. Easiest single point: after the `let (stop, iteration, spent) = …` binding where the loop result is known (grep the exact return shape; `run_guarded` returns `Ok((stop, iteration, spent))` at ~line 478).
6. Everything is best-effort: `save()` already swallows errors; the closure must not panic (use `unwrap_or_else(|e| e.into_inner())` on the mutex like `task_runner.rs` does).

- [ ] **Step 1: Write the failing test.** Full-loop tests can't run live agents, but `run_loop_for_test` (loop_run.rs:787) exercises guard short-circuits. Add:

```rust
#[tokio::test]
async fn progress_file_written_with_outcome_on_guard_stop() {
    let tmp = tempfile::tempdir().unwrap();
    // run_loop_for_test drives run_guarded to a guard stop (see existing
    // commander tests for the setup pattern — reuse their fleet fixture).
    let _stop = run_loop_for_test(tmp.path()).await;
    let (p, _) = crate::cmd::fleet::progress::load(tmp.path(), "dev")
        .expect("progress file written");
    assert_eq!(p.schema_version, 1);
    assert!(p.finished_at.is_some());
    assert!(p.outcome.is_some());
}
```

(Adapt the fleet name to what `run_loop_for_test` creates — read that helper first; if it stops before entering `run_guarded`'s body, assert only on the fields the reached path writes, and say so in the report.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core loop_run::tests::progress_file_written`
Expected: FAIL (no file written yet).

- [ ] **Step 3: Implement** per the behavior list above.

- [ ] **Step 4: Run the fleet suite**

Run: `cargo nextest run -p mur-core fleet:: && cargo nextest run -p mur-core loop_run`
Expected: all pass, including existing guard/budget tests unchanged.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/fleet/
git commit -m "feat(fleet): loop writes run progress + per-step/iteration output (T3)"
```

---

### Task 4: Panel integration

**Files:**
- Modify: `mur-core/src/cmd/deep_research/panel.rs` (`render_panel` + `cmd_panel`)

**Interfaces:**
- Consumes: Task 1 `progress::{load, RunProgress, StepState, STALE_AFTER_SECS}`; existing `DeepResearchStatus`, `DEFAULT_FLEET_NAME`.
- Produces: `render_progress(p: &RunProgress, mtime_age_secs: u64) -> String` (pure, unit-tested); `render_panel` gains an `Option<(RunProgress, u64)>` parameter — update its two existing callers/tests accordingly.

Rendering rules (from spec §4):
- In-flight (`finished_at.is_none()`): a "Run in progress" block — question (truncated 80 chars), `iteration N`, per-phase `done/total` counts, each `Running` step as `⏳ s2 research dr_worker_2 (42s)`, `spend $x/$y`, elapsed since `started_at`. If `mtime_age_secs > STALE_AFTER_SECS`, append `⚠ no update for <m> min — run may have crashed (mur fleet stop/start deep-research)`.
- Finished: single line `last run: <outcome> · $<spend> · <iteration> iterations`.
- No file: no block (panel unchanged).

- [ ] **Step 1: Write the failing tests** (extend `panel.rs` tests)

```rust
#[test]
fn panel_shows_in_flight_run_block() {
    let p = progress_fixture(None); // helper: finished_at None, 1 done 1 running 1 pending
    let out = render_progress(&p, 30);
    assert!(out.contains("Run in progress"));
    assert!(out.contains("iteration 2"));
    assert!(out.contains("$0.31"));
    assert!(!out.contains("crashed"));
}

#[test]
fn panel_marks_stale_run() {
    let p = progress_fixture(None);
    let out = render_progress(&p, STALE_AFTER_SECS + 1);
    assert!(out.contains("run may have crashed"));
}

#[test]
fn panel_shows_last_run_line_when_finished() {
    let p = progress_fixture(Some(("converged", "2026-07-14T01:00:00Z")));
    let out = render_progress(&p, 10_000);
    assert!(out.contains("last run: converged"));
    assert!(!out.contains("crashed"));
}
```

(`progress_fixture` builds a `RunProgress` literal — same shape as Task 1's `sample()`; the `Some((outcome, finished_at))` variant fills both fields.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core deep_research::panel`
Expected: FAIL (render_progress missing).

- [ ] **Step 3: Implement** `render_progress` + wire `cmd_panel`:

```rust
pub fn cmd_panel(mur_home: &Path) -> anyhow::Result<()> {
    let progress = crate::cmd::fleet::progress::load(mur_home, DEFAULT_FLEET_NAME).map(|(p, mtime)| {
        let age = mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        (p, age)
    });
    print!("{}", render_panel(&collect_status(mur_home, DEFAULT_FLEET_NAME), progress));
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core deep_research::`
Expected: all pass (old panel tests updated for the new parameter, still asserting the same strings).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/deep_research/panel.rs
git commit -m "feat(deep-research): panel shows in-flight/last run progress (T4)"
```

---

### Task 5: Docs + operator verification checklist

**Files:**
- Modify: `README.md` (deep-research section: one short paragraph on run progress + panel)
- Modify: `docs/architecture/runtime-overview.md` (same content, deep-research subsection)

- [ ] **Step 1: Add the copy**

```markdown
Runs now report progress: each step prints `✓ s2 research dr_worker_2 $0.08 42s`
as it completes, every iteration ends with a summary
(`iteration 2 done: 3✓ 0✗ 2 pending · spend $0.31/$2.00 · model claude_haiku`),
and `mur deep-research` (bare) shows the in-flight run — per-phase counts,
running steps, spend vs budget — or the last run's outcome. Progress lives in
`~/.mur/fleets/deep-research/.run_progress.json` (best-effort; never affects the run).
```

- [ ] **Step 2: Operator verification (manual; record in PR body)**
1. Start a real run; confirm per-step lines + iteration summaries appear.
2. In another terminal mid-run: `mur deep-research` shows the in-progress block.
3. After the run: bare panel shows `last run: …`.
4. Kill the loop mid-run; after >10 min the panel labels the run stale.

- [ ] **Step 3: Commit**

```bash
git add README.md docs/architecture/runtime-overview.md
git commit -m "docs(deep-research): run progress output + panel (T5)"
```

---

## Self-review notes

- Spec coverage: §1→T1, §2→T2+T3, §3→T3, §4→T4, §5 explicitly out of scope (schema_version reserved in T1), §6→tests in each task + T5 operator checklist.
- Interface-confirmation points (not placeholders) are flagged with exact greps: ProcedureStep command/delegate fields (T2), LoopStop variant names and `run_loop_for_test` reach (T3).
- Type consistency: `RunProgress`/`StepProgress`/`Phase`/`StepState` defined once (T1); `StepEvent`/`StepEventKind` once (T2); `render_progress(p, age_secs)` signature consistent between T4 steps.
