#!/usr/bin/env python3
"""Gate 2+3: cherry-pick quality and CAS efficiency measurement.
Run this against a real mur-core parallel session after Task 12 is working.

Prerequisites:
  1. Create a test fleet: mur fleet create qual-test --members agent1,agent2,agent3 --parallel
  2. Run it: mur fleet run qual-test
  3. Run judge: mur fleet judge qual-test
  4. Run this script pointing at the state dir
"""
import sys, json, subprocess, statistics
from pathlib import Path

def run_cargo_check(path: Path) -> bool:
    result = subprocess.run(
        ["cargo", "check", "--quiet"],
        cwd=path, capture_output=True,
        env={**__import__("os").environ, "ORT_STRATEGY": "download"}
    )
    return result.returncode == 0

def main(fleet_dir: Path):
    state_dir = fleet_dir / "parallel_state"
    if not state_dir.exists():
        print(f"ERROR: {state_dir} not found. Run the fleet first.")
        sys.exit(1)

    cherry_dir = fleet_dir / "cherry-result"
    if not cherry_dir.exists():
        print("ERROR: no cherry result. Run `mur fleet cherry <name>` first.")
        sys.exit(1)

    # Gate 2: cargo check pass rate
    cargo_ok = run_cargo_check(cherry_dir)
    print(f"Gate 2a — cargo check on cherry result: {'✅ PASS' if cargo_ok else '❌ FAIL'}")

    # Gate 3: count CAS hits from state DB (requires LMDB introspection)
    # ponytail: read summary JSON written by mur fleet judge --stats flag (P1 polish)
    stats_file = fleet_dir / "judge_stats.json"
    if stats_file.exists():
        stats = json.loads(stats_file.read_text())
        hit_rate = stats.get("cas_hit_rate", 0)
        cost_ratio = stats.get("cost_ratio_vs_single", 99)
        print(f"Gate 3a — CAS hit rate: {hit_rate:.1%} (PASS if ≥ 30%): {'✅' if hit_rate >= 0.30 else '❌'}")
        print(f"Gate 3b — Cost ratio vs single agent: {cost_ratio:.1f}× (PASS if ≤ 2.5×): {'✅' if cost_ratio <= 2.5 else '❌'}")
    else:
        print("Gate 3: stats file not found — run with --stats flag (P1 polish iteration)")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <path-to-fleet-dir>")
        sys.exit(1)
    main(Path(sys.argv[1]))
