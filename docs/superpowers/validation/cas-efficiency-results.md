# CAS Efficiency Results (Gate 3a/3b)

**Date:** 2026-06-29  
**Status:** ⏳ PENDING — requires `mur fleet judge --stats` (P1 polish, not yet shipped)

## Blocker

The `--stats` flag for `mur fleet judge` has not been implemented yet. It needs to:
1. Track which semantic units were cache-hits vs cache-misses in `ParallelStateDb`
2. Track token usage per track per unit to compute cost ratio
3. Write `~/.mur/fleets/<name>/judge_stats.json`:
   ```json
   {
     "cas_hit_rate": 0.42,
     "cost_ratio_vs_single": 1.8,
     "units_total": 24,
     "units_cached": 10,
     "tokens_parallel": 48200,
     "tokens_single_estimate": 26800
   }
   ```

## Pass Criteria

| Gate | Metric | Target | Fail action |
|------|--------|--------|-------------|
| 3a | CAS hit rate | ≥ 30% | Add embedding-similarity pre-scoring tier |
| 3b | Cost ratio vs single agent | ≤ 2.5× | Reduce track parallelism or use batch inference |

## Collection steps (once --stats is shipped)

```bash
mur fleet create qual-test --members rustsmith,qa --parallel
mur fleet run qual-test
mur fleet judge qual-test --stats
mur fleet cherry qual-test
python3 scripts/cherry_quality.py ~/.mur/fleets/qual-test
```

## Expected ranges

- High-overlap codebase (shared utilities across tracks): 40–50% CAS hit rate
- Independent modules: 15–25% CAS hit rate
- Cost ratio for 2 tracks: expected ~1.8–2.2× (judge overhead amortized by CAS savings)
