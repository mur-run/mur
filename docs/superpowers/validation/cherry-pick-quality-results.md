# Gate 2+3 — Cherry-Pick Quality & CAS Efficiency Results

**Status:** Pending — requires full P1 cherry loop (fleet cherry-result dir)

Run: `python3 scripts/cherry_quality.py ~/.mur/fleets/<fleet-name>`

## Gate 2a: cargo check on cherry result
- **Target:** PASS
- **Expected:** cherry-result codebase compiles without errors after parallel merge conflict resolution

## Gate 3a: CAS hit rate
- **Target:** ≥ 30%
- **Expected:** At least 30% of function signatures in merged codebase match existing definitions (content-addressed store efficiency)

## Gate 3b: Cost ratio vs single agent
- **Target:** ≤ 2.5×
- **Expected:** Parallel execution cost (all tracks + merge + judge) ≤ 2.5× the cost of single-agent sequential execution

---

**Prerequisites:**
1. Create a test fleet: `mur fleet create qual-test --members agent1,agent2,agent3 --parallel`
2. Run it: `mur fleet run qual-test`
3. Run judge: `mur fleet judge qual-test`
4. Execute the script pointing at the state dir

**Failure criteria:**
- Gate 2 fail: `cargo check` fails → strengthen dependency conflict detection in `conflict.rs`
- Gate 3 fail: CAS hit rate < 30% or cost > 4× → add embedding-similarity pre-scoring tier
