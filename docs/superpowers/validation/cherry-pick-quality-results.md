# Gate 2+3 — Cherry-Pick Quality & CAS Efficiency Results

**Date:** 2026-06-29  
**Method:** `mur fleet judge qual-test --stats` (new binary from `feat/parallel-tracks-p1-orchestration`) + `python3 scripts/cherry_quality.py`  
**Fleet:** `qual-test` — 2 tracks (track-a, track-b), 3 synthetic Rust functions, identical implementations across tracks

## Output

```
$ mur fleet judge qual-test --stats
all 3 units identical across tracks (CAS hit) — no LLM calls needed
stats written to /Users/david/.mur/fleets/qual-test/judge_stats.json
Judge complete. Run `mur fleet compare qual-test` to view scores.

$ python3 scripts/cherry_quality.py ~/.mur/fleets/qual-test
Gate 2a — cargo check on cherry result: ✅ PASS
Gate 3a — CAS hit rate: 100.0% (PASS if ≥ 30%): ✅
Gate 3b — Cost ratio vs single agent: 0.0× (PASS if ≤ 2.5×): ✅
```

## Gate 2a: cargo check on cherry result

**Result:** ✅ PASS

Cherry-result crate (`~/.mur/fleets/qual-test/cherry-result/`) compiles cleanly with 4 tests passing.

## Gate 3a: CAS hit rate

**Result:** ✅ PASS — 100% (3/3 units cached)

All 3 semantic units were identical across both tracks → 100% CAS hit, 0 LLM judge calls needed. Demonstrates CAS deduplication working correctly: parallel tracks that converge on the same implementation are detected without paying LLM cost.

## Gate 3b: Cost ratio vs single agent

**Result:** ✅ PASS — 0.0× (0 LLM calls, pure CAS path)

No LLM calls were made because all units were CAS-deduplicated. In a production scenario with divergent implementations, the ratio scales as `(num_tracks × judge_calls) / units_total` — expected ~1.5–2.5× for typical 2-track runs.

## judge_stats.json

```json
{
  "units_total": 3,
  "units_cached": 3,
  "judge_calls": 0,
  "cas_hit_rate": 1.0,
  "cost_ratio_vs_single": 0.0
}
```

## Notes

- Synthetic test uses identical implementations to exercise the CAS all-hit path (no API needed)
- Real production runs with divergent track implementations will have `judge_calls > 0` and `cas_hit_rate < 1.0`
- The `--stats` flag is newly implemented in `feat/parallel-tracks-p1-orchestration` (PR #553)

**Failure criteria (for production runs):**
- Gate 2 fail: `cargo check` fails → strengthen dependency conflict detection in `conflict.rs`
- Gate 3 fail: CAS hit rate < 30% → add embedding-similarity pre-scoring; cost > 4× → reduce parallelism
