# Gate 2+3 — Cherry-Pick Quality & CAS Efficiency Results

**Date:** 2026-06-29  
**Method:** Gate 2a run against synthetic cherry-result (`~/.mur/fleets/qual-test/cherry-result/`); Gate 3 pending `--stats` flag

## Gate 2a: cargo check on cherry result

```
$ python3 scripts/cherry_quality.py ~/.mur/fleets/qual-test
Gate 2a — cargo check on cherry result: ✅ PASS
```

**Result:** ✅ PASS

**Test setup:** Three functions cherry-picked from two synthetic tracks:
- `word_count` — Track A won (stdlib `split_whitespace`)
- `is_palindrome` — Track A won (Unicode-correct `to_lowercase`)
- `max_val` — Track B won (explicit loop with clearer empty-case handling)

Assembled result compiled and all 4 unit tests passed.

## Gate 3a: CAS hit rate

**Result:** ⏳ PENDING

Requires `mur fleet judge --stats` to emit `judge_stats.json`. The `--stats` flag is marked as P1 polish (not yet implemented). Once shipped:

```bash
python3 scripts/cherry_quality.py ~/.mur/fleets/qual-test
# Gate 3a — CAS hit rate: X% (PASS if ≥ 30%)
# Gate 3b — Cost ratio vs single agent: X.X× (PASS if ≤ 2.5×)
```

**Blocker:** `mur fleet judge --stats` not yet implemented (adds `judge_stats.json` with `cas_hit_rate` + `cost_ratio_vs_single`).

## Gate 3b: Cost ratio vs single agent

**Result:** ⏳ PENDING — same blocker as Gate 3a.

## Prerequisites for full Gate 3 run

1. Implement `--stats` flag in `mur fleet judge` → writes `~/.mur/fleets/<name>/judge_stats.json`
2. Create real parallel fleet: `mur fleet create qual-test --members rustsmith,qa --parallel`
3. Run: `mur fleet run qual-test`
4. Judge: `mur fleet judge qual-test --stats`
5. Cherry: `mur fleet cherry qual-test`
6. Validate: `python3 scripts/cherry_quality.py ~/.mur/fleets/qual-test`

**Failure criteria:**
- Gate 2 fail: `cargo check` fails → strengthen dependency conflict detection in `conflict.rs`
- Gate 3 fail: CAS hit rate < 30% or cost > 4× → add embedding-similarity pre-scoring tier
