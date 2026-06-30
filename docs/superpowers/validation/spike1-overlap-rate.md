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

**Method A — observational (cheapest, decisive; what settled the gate).** Mine real git
history: every genuine 2-parent merge is a natural concurrent-edit experiment (two branches
diverged from a common base and both edited files). Replay each side vs the merge-base
through the **production classifier** (`count_groups` → `group_edits`, the exact code
`merge-concurrent --stats` uses) and aggregate. Zero agents, zero worktree infra, large
real sample:

```
cargo run --release --example spike1_history -- <repo_path>
```

*Inference asymmetry:* 2-parent merges are N=2. The CRDT's unique niche is N>2. If even
2-way overlap is rare in history, 3-way overlap is necessarily rarer → STOP-on-Loro holds
*a fortiori*. (Only a *high* 2-way rate would be inconclusive — it still wouldn't prove the
N>2 case, so it could at most justify INVESTIGATE, never PROCEED on this evidence alone.)

**Method B — live emergent (richer, expensive; not needed for the gate).** Real parallel
runs with N ≥ 3 worktrees + `tracks.json`, then `MUR_PARALLEL_CONCURRENT=1 mur fleet
merge-concurrent <fleet> --stats`. Blocked on production-mode worktree wiring (`mur fleet
run` has no live `create_tracks` caller) + live agents. Use only if Method A had been
inconclusive — it wasn't.

## Decision gate

| overlap_rate | Conclusion |
|---|---|
| < 5% | STOP — StructuralMerger sufficient; skip Loro (Phase 1). |
| 5–20% | INVESTIGATE — profile which files conflict; may be addressable by agent prompting. |
| > 20% | PROCEED — Loro CRDT engine (Phase 1) worth its dependency cost. |

## Results

| run | source | N | merges | files | clean_groups | overlap_regions | overlap_rate | notes |
|-----|--------|---|--------|-------|--------------|-----------------|--------------|-------|
| **4** | **live agents — PARTITION mode (DECISIVE, disjoint regime)** | 2 | — | 1 | **5** | **0** | **0.0%** | Real pm+qa via `MUR_PARALLEL_EXEC=1 mur fleet run`. LPT split `hunk.rs`'s 6 units between the two; each edited only its assigned units in its own worktree → 5 disjoint changed regions, **zero overlap**. The live equivalent of run 2 (0.1%) — disjoint parallel agent work auto-merges cleanly. |
| **3** | **live agents — SPECULATIVE mode (same-task regime)** | 2 | — | 1 | 0 | 1 | 100.0% | Real pm+qa, **same** task → both rewrote the *same* doc-comment lines differently → 1 overlap, correctly **escalated** (not silently interleaved). 100% is by construction; this regime is **select-the-best, not line-merge** — a CRDT would fuse the two sentences into garbage. Validates detection+escalation on live output; NOT a gate number. |
| **2** | **observational — MUR git history (DECISIVE)** | 2 | 27 | 626 | 1633 | **2** | **0.1%** | Method A over the repo's entire merge history (27 genuine divergent 2-parent merges). Only 2 overlaps, both in registration hotspots (`main.rs`, `mur-agent-gui/.../wiring.rs` — two branches each adding an entry). The real rate at which independently-developed branches edit the same `.rs` lines. |
| 1 | constructed (instrument validation) | 3 | — | 4 | 4 | 1 | 20.0% | 1 single-actor file ×2, 1 disjoint-region file (auto-merged →2 clean), 1 **3-way** same-line collision (escalated, all 3 actors captured). Validates the instrument (disjoint auto-merge + N-way escalation); the 20% is a *constructed* mix, not a measurement. |
| 0 | degenerate (rejected) | 2 | — | 1 | 0 | 3 | 100.0% | Two agents given the **same** task on one function → manufactured 100%. Violates both methodology requirements; kept only as a sanity check that overlaps escalate rather than silently interleave. |

**Conclusion: STOP — skip Phase 1 (Loro). The zero-dep `StructuralMerger` (P3 Phase 0) is
sufficient.** Run 2 measures the real overlap rate over MUR's entire history at **0.1%**
(2 overlaps in 1635 edit groups across 626 files / 27 divergent merges) — far below the 5%
STOP threshold. By the inference asymmetry, this is N=2 (the *easiest* case to hit
overlap); the CRDT's only unique advantage is N>2, which is necessarily rarer, so STOP
holds *a fortiori*. The two real overlaps are central-registration hotspots (both branches
appending a match arm / module decl) — exactly the conflicts a line-CRDT converges but
cannot *correctly* resolve (it would interleave two arms; still needs judge/human). So
Loro's ~40 transitive deps would buy auto-merge on ~0.1% of cases, and even those it can't
get right. `StructuralMerger`'s policy (auto-merge disjoint hunks, escalate every overlap)
captures essentially all the value with zero new dependencies. Spike-2 (footprint) and
Spike-3 (diff→ops fidelity) are therefore moot. Re-run `cargo run --example spike1_history`
on any repo to re-confirm.

*Caveats (do not change the verdict):* the sample is MUR's own history (human + some agent
work) and N=2; both make STOP **stronger**, not weaker — pure disjoint-task agent fan-out
would overlap even less, and N>2 is rarer than N=2.

## Live confirmation (real agents, both regimes)

The observational verdict was then confirmed end-to-end with **live agents** (pm+qa) via the
Tier 1 worktree-execution path (`MUR_PARALLEL_EXEC=1 mur fleet run`), in both parallel modes:

- **Partition (disjoint)** — run 4: the LPT planner split one file's units between the two
  agents; each edited only its assigned units in its own worktree → **5 clean groups, 0
  overlap, 0.0%**. This is the live equivalent of the 0.1% git-history number: *disjoint
  parallel agent work does not overlap*, and `StructuralMerger` auto-merges all of it.
- **Speculative (same task)** — run 3: both agents got the *same* task and rewrote the *same*
  lines differently → **100%** overlap, correctly **escalated** (never silently interleaved).
  100% here is by construction; this regime is **select-the-best-whole-result, not
  line-merge**.

## Why STOP is robust beyond the numbers — the three-regime framework

A line-CRDT is unnecessary, wrong, or net-negative in *every* parallel regime:

1. **Partition (disjoint by construction)** — overlap ≈ 0 (run 2: 0.1%, run 4: 0.0%) →
   `StructuralMerger` trivially handles it → a CRDT adds nothing.
2. **Speculative best-of-N (same task)** — overlap ≈ 100% (run 3) → but you **select** the
   best whole attempt (judge/cherry); CRDT-merging competing rewrites of the same function
   yields interleaved garbage → a CRDT is the **wrong tool**, not an insufficient one.
3. **Uncoordinated different-tasks-that-collide** — the only theoretical CRDT niche; but the
   design's own evidence (CodeCRDT ~80% semantic conflicts on complex tasks; DeepMerge-class
   LLM conflict-fixing 15–36% on non-trivial) shows auto-merging overlaps is net-negative →
   **escalate**, don't CRDT-merge. And you'd *design around* this regime (partition or select)
   rather than create it.

**Loro exists for real-time collaborative *document* editing** (Figma/Notion/local-first),
where convergence is the goal and prose interleaving is tolerable. Code is different:
convergence ≠ correctness, and interleaving two logic edits = broken code. A mature CRDT
ecosystem proves the collaborative-doc problem is real — **not** that MUR's post-hoc
agent-diff-merge use case is real or solvable (no known agent system uses final-state-diff →
CRDT; the design flags this novelty risk itself).

**Net:** Loro has no regime where it wins. The decision rests on the convergence of
observational (0.1%), inference-asymmetry (N=2 ⇒ N>2 rarer), live dual-mode (0% / 100%), and
the regime analysis — four independent lines, one direction. `StructuralMerger` is the final
engine; Loro/Spike-2/Spike-3 are shelved. Revisit only under a triple-conditional that is
unlikely to hold: (a) MUR ships an *uncoordinated* parallel fan-out, **and** (b) live
measurement shows the collide-but-both-correct regime is common, **and** (c) regime analysis
says a CRDT — not better partitioning or select — is the right fix.
