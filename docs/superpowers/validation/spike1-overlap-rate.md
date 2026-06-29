# Spike-1: Concurrent-edit overlap rate (decides whether CRDT is worth building)

**Question:** In real MUR parallel runs, how often do ≥2 agents edit overlapping lines,
and how does this distribute across N (tracks per run)? If overlap is rare or N≈2, the
zero-dep `StructuralMerger` already captures most auto-accept value and the Loro engine
(Phase 1) is NOT worth its ~40+ transitive dependencies.

## How to run

1. Execute real parallel runs (`mur fleet run <fleet>` with ≥2 member agents and `parallel:` config)
   so `~/.mur/fleets/<name>/tracks.json` and worktrees exist.
2. `MUR_PARALLEL_CONCURRENT=1 mur fleet merge-concurrent <fleet> --stats`
3. Read `~/.mur/fleets/<fleet>/concurrent_stats.json`.

## Decision gate

| overlap_rate | Conclusion |
|---|---|
| < 5% | STOP — StructuralMerger sufficient; skip Loro (Phase 1). |
| 5–20% | INVESTIGATE — profile which files conflict; may be addressable by agent prompting. |
| > 20% | PROCEED — Loro CRDT engine (Phase 1) worth its dependency cost. |

## Results (fill in after real runs)

| run | fleet | N | files | clean_groups | overlap_regions | overlap_rate |
|-----|-------|---|-------|--------------|-----------------|--------------| 
| | | | | | | |

**Conclusion:** _(STOP / INVESTIGATE / PROCEED + one-line rationale)_
