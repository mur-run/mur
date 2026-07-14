# Deep Research Run Progress — Design

Date: 2026-07-14
Status: Approved (brainstorm)
Related: `2026-07-13-deep-research-ux-simplification-design.md` (Phase 1 shipped: setup/panel/ask)

## Problem

A deep-research run is silent for minutes at a time: the loop prints only iteration
headers and router assignments, and the bare `mur deep-research` panel shows nothing
about an in-flight run. The user cannot see how many steps are done vs pending, what
each stage is doing, what it costs, or which model is running.

## Decision (from brainstorm)

Single progress source, multiple views (approach 1): the fleet loop writes a
`.run_progress.json` per event; the run command renders log-style lines + per-iteration
summaries from the same data; the bare panel shows the in-flight (or last) run.
murmur Panel rendering is Phase 2 on the same file.

## §1 Progress model

New pure module `mur-core/src/cmd/fleet/progress.rs`:

- `RunProgress { schema_version: 1, run_id, question, started_at, finished_at: Option, outcome: Option<Outcome>, iteration, model, budget_usd: Option<f64>, steps: Vec<StepProgress>, }`
- `Outcome`: `Converged | MaxIterations | Deadline | Budget | Stopped | Failed`
  (mirrors the loop's existing `LoopStop` reasons).
- `StepProgress { id, worker, phase: Phase, desc, state: StepState, cost_usd: Option<f64>, started_at: Option, ended_at: Option }`
- `Phase`: `Probe | Research | Verify | Synthesize | Other` — classified from the
  router assignment text by a pure heuristic (keyword match, case-insensitive);
  unclassifiable → `Other`, never blocks the run.
- `StepState`: `Pending | Running | Done | Failed`.
- `totals()` computed, not stored: `{done, running, pending, failed, spend_usd}`.

## §2 Write points

In `loop_run.rs` (and the DAG execution seam it drives):

- Iteration start: all planned steps written as `Pending` (this is what makes
  "N pending" real).
- Step dispatched → `Running` (+ `started_at`); reply received → `Done`
  (+ `ended_at`, per-turn token cost when available); step failure → `Failed`.
- Run end: `finished_at` + `outcome` written; file kept as the last-run record and
  overwritten by the next run.
- File: `~/.mur/fleets/<name>/.run_progress.json`, atomic temp+rename (yaml.rs
  pattern). ALL writes best-effort — a progress-file error must never fail or slow
  the run (log at debug, continue).

## §3 Run output (log style)

Existing lines stay. Added:

- one line per step completion/failure: `✓ s2 research dr_worker_2 $0.08 42s`
  (`✗` for failed);
- per-iteration summary line:
  `iteration 2 done: 3✓ 0✗ 2 pending · spend $0.31/$2.00 · model claude_haiku`.

No in-place TUI redraw; append-only so logs stay pasteable.

## §4 Bare panel integration

`mur deep-research` reads the progress file:

- In-flight (`finished_at` absent): "Run in progress" block — question, iteration,
  per-phase done/pending counts, currently running steps (worker + phase + elapsed),
  spend vs budget, total elapsed. If the file's mtime is older than 10 minutes it is
  labeled stale ("possibly crashed — check `mur fleet stop/start`").
- Finished: one line — `last run: converged · $0.61 · 4 iterations · <ended>`.
- No file: panel unchanged.

## §5 Phase 2 (murmur Panel)

Separate plan later: the murmur TUI Panel (glass box) renders a deep-research
progress block from the same file via the panel bridge. This spec only reserves
`schema_version: 1` in the file for that consumer.

## §6 Testing

- Pure unit tests: phase classification, totals, finished/stale detection,
  summary-line rendering.
- Atomic-write reuses the existing tested pattern.
- Panel rendering: string-assertion tests like `panel.rs`.
- Live loop behavior remains operator-verified (fleet loop dials live sockets).
