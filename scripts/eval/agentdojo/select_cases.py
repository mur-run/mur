"""[STUB] Deterministic case selection for AgentDojo-50.

Phase A of M11.2.1 is **deferred** for AgentDojo. Unlike HarmBench's
flat CSV, AgentDojo's task catalog is enumerated at runtime via
Python class introspection (`Suite.user_tasks` / `Suite.injection_tasks`
across `agentdojo.default_suites.v1_x_y` modules). To produce a
canonical `case_selection.json` we need `pip install agentdojo==0.1.32`
running so we can iterate the real catalog.

The selection seed is the same shared one (
`SHA256("mur-b0-acceptance-2026").hexdigest()[:8]` = 0x47b305de =
1202914782) — see `scripts/eval/harmbench/select_cases.py` for the
full rationale.

When this script is implemented (a future M11.2.1 fresh session):
  1. `pip install agentdojo==0.1.32` (matches `requirements.txt` pin)
  2. Iterate `Suite.user_tasks` across `default_suites.v1_2_2.{banking,
     slack,travel,workspace}` — gives ~30 user tasks total
  3. Iterate `Suite.injection_tasks` for each suite — gives ~30
     injection tasks total
  4. Cartesian-product the (user_task, injection_task) pairs scoped to
     the same suite, which is AgentDojo's notion of one test case
  5. Stratified-sample 50 across the 5 environments (banking / slack
     / travel / workspace) so each gets ~12 cases
  6. Emit `scripts/eval/agentdojo/case_selection.json` matching the
     same shape HarmBench's selection uses (test_id, attack_category,
     expected_outcome) so the M11.2 runner consumes both identically

This stub fails loudly so a CI / harness flow that picks it up gets
a clear error rather than a silent skip.
"""

from __future__ import annotations

import sys


def main() -> int:
    print(
        "[M11.2.1 stub] AgentDojo case selection requires pip install "
        "agentdojo==0.1.32 to enumerate the runtime task catalog; not "
        "yet implemented. Track via the M11.2.1 follow-up issue.",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
