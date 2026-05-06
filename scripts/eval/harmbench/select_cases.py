"""Deterministic case selection for the HarmBench-50 acceptance subset.

Reads HarmBench's published `harmbench_behaviors_text_all.csv`,
applies the SHA-256-derived seed from
`docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md` §4.2,
and writes `case_selection.json` so the choice is reproducible across
mur tags.

Stratified sampling: 12.5% (50 of 400) from each `SemanticCategory`
preserves upstream's published distribution. Categories with <8
behaviours (harassment_bullying, harmful) get the rounded-up count
to keep ≥3 representatives so each category has a meaningful
denominator in the markdown report.

Per `LICENSES.md`: HarmBench is CC-BY-4.0; we use the dataset
verbatim (no relabelling). The output JSON only references
`BehaviorID` + upstream-published columns; no derived classifications.

Usage:
  python3 scripts/eval/harmbench/select_cases.py \
      --csv harmbench_behaviors_text_all.csv \
      --out scripts/eval/harmbench/case_selection.json
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import pathlib
import random
import sys
from collections import defaultdict


# Per scripts/eval/README.md "Selection seed" — must stay byte-identical
# to `runner_common.SELECTION_SEED` so all M11 case-selection scripts
# share one canonical seed. Bumping this is a breaking change for all
# committed `case_selection.json` files.
_SEED_PHRASE = "mur-b0-acceptance-2026"
SELECTION_SEED = int(hashlib.sha256(_SEED_PHRASE.encode()).hexdigest()[:8], 16)

# 50 total, stratified across the 7 published SemanticCategories.
# Sums to 50; ratios approximate 12.5% of upstream's per-category count
# (rounded up for small categories so each has ≥3 representatives).
QUOTAS: dict[str, int] = {
    "copyright": 13,
    "cybercrime_intrusion": 8,
    "illegal": 8,
    "misinformation_disinformation": 8,
    "chemical_biological": 7,
    "harassment_bullying": 3,
    "harmful": 3,
}


def select(csv_path: pathlib.Path) -> list[dict]:
    """Return the 50 selected behaviour dicts in stable order:
    grouped by category (alphabetical), then by BehaviorID.

    `random.Random(SELECTION_SEED)` provides determinism across Python
    versions for `sample()` (CPython contract is stable since 3.11).
    """
    rng = random.Random(SELECTION_SEED)

    by_category: dict[str, list[dict]] = defaultdict(list)
    with csv_path.open(encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for row in reader:
            by_category[row["SemanticCategory"]].append(row)

    # Sanity: every quota category must exist with enough rows.
    for cat, n in QUOTAS.items():
        if cat not in by_category:
            raise SystemExit(f"category {cat!r} missing from {csv_path}")
        if len(by_category[cat]) < n:
            raise SystemExit(
                f"category {cat!r} has only {len(by_category[cat])} rows; "
                f"quota requires {n}"
            )

    selected: list[dict] = []
    for cat in sorted(QUOTAS):
        # Sort by BehaviorID first so the input to sample() is stable
        # regardless of upstream CSV row order.
        rows = sorted(by_category[cat], key=lambda r: r["BehaviorID"])
        chosen = rng.sample(rows, QUOTAS[cat])
        chosen.sort(key=lambda r: r["BehaviorID"])
        selected.extend(chosen)

    return selected


def to_runner_format(rows: list[dict]) -> list[dict]:
    """Translate upstream's CSV row shape into the runner's
    `case_selection.json` shape — the schema the M11.3 runner reads.

    `attack_category` echoes upstream's `SemanticCategory` verbatim so
    the markdown report's bucket labels match HarmBench's published
    taxonomy. Per LICENSES.md no relabelling.
    """
    out = []
    for r in rows:
        out.append({
            "test_id": f"harmbench:{r['BehaviorID']}",
            "behavior_id": r["BehaviorID"],
            "behavior": r["Behavior"],
            "functional_category": r["FunctionalCategory"],
            "attack_category": r["SemanticCategory"],
            "context_string": r.get("ContextString") or None,
            "tags": r.get("Tags") or None,
            # Default expected-outcome: the agent should refuse to
            # follow the harmful request. HarmBench's grading model
            # is the authority at run-time; this field is what the
            # mock-LLM track checks against in M11.3.
            "expected_outcome": "refuse",
        })
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--csv", type=pathlib.Path, required=True)
    p.add_argument("--out", type=pathlib.Path, required=True)
    args = p.parse_args(argv)

    rows = select(args.csv)
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
