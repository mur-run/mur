# Spike-1: Concurrent-edit overlap rate (decides whether CRDT is worth building)

**Question:** In real MUR parallel runs, how often do ≥2 agents edit overlapping lines,
and how does this distribute across N (tracks per run)? If overlap is rare or N≈2, the
zero-dep `StructuralMerger` already captures most auto-accept value and the Loro engine
(Phase 1) is NOT worth its ~40+ transitive dependencies.

## Methodology requirements (learned the hard way)

Two constraints make a run **decision-grade**. A run that violates either is invalid for the gate:

1. **N ≥ 3.** The gate in the design spec (§9, §11) hinges on `N>2`: at N=2 a plain
   3-way `git merge` already reconciles disjoint edits, so the CRDT's *only* unique
   advantage — commutative **N-way** merge — never triggers. An N=2 measurement cannot
   inform the build/skip decision, whatever its rate.
2. **Realistic file mix, not repeated same-task.** The rate is
   `overlap_groups / (clean_groups + overlap_groups)` aggregated over the **union of
   changed `.rs` files**. A representative run must contain a mix: files touched by one
   agent only (clean), files where agents edit **disjoint** regions (clean auto-merge —
   the signal *against* needing Loro), and files where agents edit the **same** region
   (overlap). Handing N agents the *same* narrow task manufactures ~100% overlap and
   measures nothing.

Also decision-relevant and observable in the run log: the **arity of each overlap**.
`merge-concurrent` prints `actors [...]` per region — a 2-way overlap is git-mergeable,
only **≥3-way** overlaps are the CRDT's exclusive niche. Weight the verdict toward
≥3-way overlap frequency, not raw 2-way collisions.

## How to run

1. Need N ≥ 3 worktrees off a common base, each with a `.parallel-base` sentinel and a
   `tracks.json` under `~/.mur/fleets/<name>/`. **Note:** `mur fleet run` does *not* yet
   create these worktrees — the `parallel:` config only injects per-track *approach* text
   into delegate steps (`create_tracks` has no live caller). Emergent runs therefore need
   the **production-mode worktree wiring** (separate spec, out of P3 Phase 0 scope) plus
   live member agents. Until then, runs are constructed (see below).
2. `MUR_PARALLEL_CONCURRENT=1 mur fleet merge-concurrent <fleet> --stats`
3. Read `~/.mur/fleets/<fleet>/concurrent_stats.json`.

## Decision gate

| overlap_rate | Conclusion |
|---|---|
| < 5% | STOP — StructuralMerger sufficient; skip Loro (Phase 1). |
| 5–20% | INVESTIGATE — profile which files conflict; may be addressable by agent prompting. |
| > 20% | PROCEED — Loro CRDT engine (Phase 1) worth its dependency cost. |

## Results

| run | source | N | files | clean_groups | overlap_regions | overlap_rate | notes |
|-----|--------|---|-------|--------------|-----------------|--------------|-------|
| 1 | constructed (instrument validation) | 3 | 4 | 4 | 1 | 20.0% | 1 single-actor file ×2, 1 disjoint-region file (auto-merged →2 clean), 1 **3-way** same-line collision (escalated, all 3 actors captured). Confirms disjoint auto-merge + N-way escalation work. |
| 0 | degenerate (rejected) | 2 | 1 | 0 | 3 | 100.0% | Two agents given the **same** task on one function → manufactured 100%. Violates *both* methodology requirements; kept only as a worst-case sanity check that overlaps escalate rather than silently interleave. **Not used for the gate.** |

**Conclusion:** _Instrument validated, gate decision DEFERRED._ Run 1 proves
`merge-concurrent --stats` correctly (a) auto-merges disjoint hunks, (b) escalates
same-region collisions without interleaving, (c) captures N-way (3-actor) overlaps, and
(d) aggregates a sane rate across a multi-file run at the decision-relevant N=3. But its
20% is a *constructed* mix, not an emergent measurement — it cannot itself decide
STOP/INVESTIGATE/PROCEED. The real gate number requires emergent data from live parallel
runs at N≥3, which is **blocked on the production-mode worktree wiring** (separate spec) +
live member agents. Phase 1 (Loro) stays unbuilt until that emergent number lands above
the gate.
