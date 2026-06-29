# Parallel Tracks — Tier 1 Worktree Execution (dogfood + live overlap measurement)

**Status:** plan. **Branch:** `feat/parallel-tier1-worktree-exec` (off `origin/main` after #562 merges).
**Flag-gated, partition-first, default OFF.**

## Why (decision context)

Spike-1 (observational, `examples/spike1_history.rs`) measured **0.1%** line-overlap across MUR's
serial-Claude-Code history → STOP on Loro. But that history is the *serial* regime; the
*parallel* regime (N agents from one base, blind to each other) is **unmeasured**. MUR is itself
built by Claude Code, so the cleanest way to close that counterfactual is to **run parallel Claude
Code on real MUR tasks and measure overlap live** — which also ships a usable feature. Tier 1 is the
cheap path to that: the merge/reconcile half is already done (P1/P2.5/P3 Phase 0); only the
*execution* half is missing.

This plan does **NOT** build Loro. It builds execution + measurement. Loro remains gated on the
live number this produces.

## Scope

**In (Tier 1):** `mur fleet run` creates N worktrees, routes each delegated agent to work+commit in
its own worktree via the **existing bash `cwd` param** (prompt-routed, no runtime change), caps
concurrency, reconciles, cleans up, and records the live overlap rate.

**Out (Tier 2, separate spec):** runtime-*enforced* cwd (add `cwd` to `TaskSpec`
`task_runner.rs:21-38` + thread `channel/delegate` + proto bump to 2). Only build if Tier 1 data
shows agents stray from prompt-routing.

## Research anchors (verified this session)

- `create_tracks(cfg, project)` is complete + grant-safe, writes `.worktrees/<t>/.parallel-base`,
  CoW-copies build cache — but has **zero live callers** (`parallel/track/worktree.rs:11`).
- `mur fleet run` never creates worktrees; `build_fleet_procedure` only injects per-track *approach*
  text into delegate steps (`cmd/fleet/run.rs:23-80,153-238`).
- Bash tool **already** accepts an optional per-call `cwd` (`mur-agent-runtime/tools/bash.rs:34-37,
  50-53`) → Tier 1 routing needs no runtime change.
- Grant: repo-root grant covers `<repo>/.worktrees/` (`sandbox/policy.rs:44-47`); confirm fleet
  members are granted repo root, else add it.
- Guards inherited free + fail-closed: budget, kill-switch, `MUR_FLEET_AUTORUN`, commander, HITL.
- **GAPS to fix:** `max_concurrency` defaults `None` (`executor/dag.rs:47-53`) → unbounded fan-out;
  no crash cleanup / orphan sweep / collision guard; HITL has no dedup; codebase index is
  shared+lock-serialized.
- All 3 reconcilers complete + read `tracks.json`+`worktree_path`: `cmd/fleet/{partition_cmd,
  cherry_cmd,judge_cmd,concurrent_cmd}.rs`.

## Tasks

### 1. Gate + trigger
- [ ] Add env gate `MUR_PARALLEL_EXEC=1` (separate from `MUR_PARALLEL_CONCURRENT`, which gates the
      concurrent *merger*). Default off. Surface a one-line "experimental" note like concurrent_cmd.
- [ ] Trigger: `mur fleet run` builds worktrees **only when** `fleet.yaml` has a `parallel:` block
      AND the gate is set. No `parallel:` → today's behavior unchanged (no regression).

### 2. Wire `create_tracks` into the run flow
- [ ] In `cmd_fleet_run` (`run.rs:153-238`), before `execute_dag`: if gated+parallel, resolve
      `project_root`, call `create_tracks(cfg, &project_root)`, `TrackSet::save(&fleet_dir)`.
- [ ] Reuse `project_root_from_worktree` / existing root resolution; do not hardcode paths.

### 3. Route delegates into worktrees (prompt-routed)
- [ ] In `build_fleet_procedure` / `build_partition_procedure`, for each track's delegate step,
      append to the intent a constraint block: work exclusively in `<worktree_path>`, pass
      `cwd=<worktree_path>` on every bash call, use absolute paths under it for edits, and
      **`git add -A && git commit`** when done (the reconciler reads `git diff <base> HEAD`).
- [ ] Map track → member deterministically (partition: region→member; speculative: same goal,
      different approach per track). Keep the mapping in `tracks.json` for the reconciler.

### 4. Cap concurrency (close the gap)
- [ ] Set `DagExecOptions.max_concurrency` in `cmd_fleet_run` (`run.rs:185-194`) and `loop_run`
      (`loop_run.rs:399-409`) to `min(n_tracks, available_parallelism()-2).max(1)`, overridable via
      config. Reuses the existing semaphore (`dag.rs:764-766`).

### 5. Reconcile after fan-out
- [ ] After `execute_dag`, dispatch by mode: **Partition → `cmd_fleet_merge`** (deterministic,
      auto-run); **Speculative → `judge` then `cherry`** (LLM; auto-run or leave as explicit
      follow-on — pick auto for partition only, keep speculative as a printed next-step to avoid
      surprise LLM spend). Concurrent merger stays the zero-dep escalating `StructuralMerger`.

### 6. Safety: cleanup + orphan sweep + collision guard
- [ ] On run completion (success OR error path), `destroy_tracks` unless `--keep-tracks`.
- [ ] Orphan sweep: on `mur fleet run`, prune stale `.worktrees/` from crashed prior runs (mirror
      the per-worktree index orphan-sweep pattern from #560; unmount-safe).
- [ ] Collision guard (Tier 1 is best-effort): after fan-out, detect unexpected dirty state in the
      **main** checkout (an agent that ignored its `cwd`) and warn loudly; never auto-promote over it.

### 7. Measurement payoff (the dogfood point)
- [ ] After reconcile, run the existing overlap instrumentation (`count_groups` /
      `merge-concurrent --stats`) over the tracks and append a row to
      `docs/superpowers/validation/spike1-overlap-rate.md` (source = "live, parallel Claude, N=<n>").
      This is the number that decides Loro — replacing the serial-history proxy.

### 8. Tests + gate
- [ ] Unit: parallel config → `create_tracks` called + `tracks.json` written; intent contains the
      cwd-routing block; `max_concurrency` set; cleanup invoked on both success and error; no
      `parallel:` block → unchanged procedure (regression guard).
- [ ] `scripts/gate8_tier1_exec.sh`: the above unit tests green (no live agents).

### 9. Docs
- [ ] CLAUDE.md `mur fleet` surface: one line for gated parallel execution + the Tier 1/Tier 2
      boundary. Update `docs/architecture/runtime-overview.md` if the run flow changes materially.

## Acceptance

1. `gate8` green; existing fleet behavior unchanged when no `parallel:` block.
2. With gate + a `parallel:` fleet: N worktrees created under `.worktrees/`, each agent commits in
   its own, reconciler produces one result, worktrees cleaned up, **live overlap row appended** to
   the spike doc.
3. Operator-verified once on a real MUR task (needs cc-proxy + ≥2 running members) — this is the
   dogfood run that yields the parallel-regime overlap number.

## Follow-ons (not this plan)

- Tier 2 enforced cwd (proto bump) — only if Tier 1 shows prompt-routing strays.
- HITL dedup across concurrent delegates; per-worktree codebase index isolation — only if the
  shared-index serialization measurably hurts.
- Loro (Phase 1) — only if the **live** overlap number (task 7) lands above the gate AND regime
  analysis says CRDT (not better partitioning / select) is the right fix.
