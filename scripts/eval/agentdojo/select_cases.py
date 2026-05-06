"""Deterministic case selection for the AgentDojo-50 acceptance subset.

Imports `agentdojo==0.1.32`, walks `Suite.user_tasks × Suite.injection_tasks`
across the v1 default suites (banking / slack / travel / workspace),
applies the seed from `runner_common.SELECTION_SEED`, and writes
`case_selection.json` so the choice is reproducible across mur tags.

Stratified sampling: ~7.95% (50 of 629) per suite, rounded so
workspace gets 20 (40 user × 6 injection = 240 cases) while smaller
suites scale down. Total = 50.

Per `LICENSES.md`: AgentDojo is Apache-2.0; cite Debenedetti et al.
NeurIPS 2024. We use upstream task IDs verbatim — `user_task_id` +
`injection_task_id` echo the upstream `BaseUserTask.ID` /
`BaseInjectionTask.ID` strings so reproducibility chains back to
the upstream commit hash.

Usage:
  pip install -r scripts/eval/requirements.txt
  python3 scripts/eval/agentdojo/select_cases.py \
      --out scripts/eval/agentdojo/case_selection.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import random
import sys
from typing import Any


# Per scripts/eval/README.md "Selection seed" — must stay byte-identical
# to `runner_common.SELECTION_SEED`. Bumping this is a breaking change
# for all committed `case_selection.json` files.
_SEED_PHRASE = "mur-b0-acceptance-2026"
SELECTION_SEED = int(hashlib.sha256(_SEED_PHRASE.encode()).hexdigest()[:8], 16)

# AgentDojo v1 has 4 suites with 629 (user × injection) cases total:
# banking 144, slack 105, travel 140, workspace 240. Stratified ~7.95%
# per suite (rounded), totalling 50.
QUOTAS: dict[str, int] = {
    "banking": 11,
    "slack": 8,
    "travel": 11,
    "workspace": 20,
}


def _enumerate_cases() -> list[dict[str, Any]]:
    """Iterate every (suite, user_task, injection_task) tuple in v1.
    Returns the full unstratified corpus — caller stratifies + samples."""
    try:
        from agentdojo.task_suite.load_suites import get_suite
    except ImportError as e:
        raise SystemExit(
            f"agentdojo not installed ({e}); "
            "`pip install -r scripts/eval/requirements.txt`"
        ) from e

    rows: list[dict[str, Any]] = []
    for suite_name in QUOTAS:
        suite = get_suite("v1", suite_name)
        # Sort user_task and injection_task IDs so the input to
        # random.sample() is stable regardless of dict iteration
        # order. Both are dicts keyed by string IDs upstream.
        for u_id in sorted(suite.user_tasks.keys()):
            user_task = suite.user_tasks[u_id]
            for i_id in sorted(suite.injection_tasks.keys()):
                injection_task = suite.injection_tasks[i_id]
                rows.append({
                    "suite": suite_name,
                    "user_task_id": u_id,
                    "injection_task_id": i_id,
                    # Upstream task classes carry a PROMPT and GOAL
                    # constant; expose minimal metadata so the runner
                    # can render diagnostics without re-importing
                    # agentdojo at run time.
                    "user_task_prompt": getattr(user_task, "PROMPT", ""),
                    "injection_task_goal": getattr(injection_task, "GOAL", ""),
                })
    return rows


def select() -> list[dict[str, Any]]:
    """Return the 50 selected cases in stable order: grouped by suite
    (alphabetical), then by (user_task_id, injection_task_id)."""
    rng = random.Random(SELECTION_SEED)
    full = _enumerate_cases()

    by_suite: dict[str, list[dict[str, Any]]] = {s: [] for s in QUOTAS}
    for row in full:
        by_suite[row["suite"]].append(row)

    for suite, rows in by_suite.items():
        if len(rows) < QUOTAS[suite]:
            raise SystemExit(
                f"suite {suite!r} has only {len(rows)} cases; "
                f"quota requires {QUOTAS[suite]}"
            )

    selected: list[dict[str, Any]] = []
    for suite in sorted(QUOTAS):
        rows = sorted(
            by_suite[suite],
            key=lambda r: (r["user_task_id"], r["injection_task_id"]),
        )
        chosen = rng.sample(rows, QUOTAS[suite])
        chosen.sort(key=lambda r: (r["user_task_id"], r["injection_task_id"]))
        selected.extend(chosen)

    return selected


def to_runner_format(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Translate to the runner's `case_selection.json` shape (matches
    HarmBench's). `attack_category` echoes the upstream suite name so
    the markdown report buckets by environment without inventing
    classifications.
    """
    out = []
    for r in rows:
        out.append({
            "test_id": (
                f"agentdojo:{r['suite']}:{r['user_task_id']}:"
                f"{r['injection_task_id']}"
            ),
            "suite": r["suite"],
            "user_task_id": r["user_task_id"],
            "injection_task_id": r["injection_task_id"],
            "user_task_prompt": r["user_task_prompt"],
            "injection_task_goal": r["injection_task_goal"],
            # Buckets the markdown report by suite environment
            # (banking / slack / travel / workspace).
            "attack_category": r["suite"],
            # Default expected-outcome: the agent should refuse the
            # injected directive AND continue with the user's
            # original task. AgentDojo's runtime utility-and-security
            # grader is the run-time authority; this default is what
            # the mock-LLM track in M11.2 checks against.
            "expected_outcome": "refuse",
        })
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--out", type=pathlib.Path, required=True)
    args = p.parse_args(argv)

    rows = select()
    output = to_runner_format(rows)
    if len(output) != 50:
        raise SystemExit(f"expected 50 cases, got {len(output)}")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(output, indent=2) + "\n")
    print(
        f"wrote {len(output)} cases (seed={SELECTION_SEED}) to {args.out}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
