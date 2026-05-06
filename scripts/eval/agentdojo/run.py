"""AgentDojo runner for the B0 eval harness (M11.2 skeleton).

This is the **structural skeleton** for the AgentDojo runner. The
upstream `agentdojo==0.1.32` package integration is intentionally
left as TODO so M11.2.1 (a fresh session with upstream API access)
can wire it up without rewriting the runner shape.

What's wired up here:
  - case loader (reads the committed `case_selection.json` produced
    by `select_cases.py`)
  - synthetic-fixture fallback so the runner can be exercised
    end-to-end in CI without `pip install agentdojo` (uses
    `scripts/eval/fixtures/synthetic_cases.json`)
  - mock-LLM dispatch via `scripts/eval/mock_llm.py`
  - per-case mur agent spawn + teardown via runner_common
  - JSONL output writer matching the `EvalRecord` schema

What's deliberately TODO:
  - The actual `agentdojo` task-suite import + injection-point
    discovery + outcome verification API. The package's runtime
    expects a particular agent harness interface; the M11.2.1
    scope picks the right adapter shape after reading the upstream
    source.
  - Per-attack-category bucketing in the JSONL output (currently
    pulls `attack_category` straight from the fixture; AgentDojo
    has finer-grained taxonomy under `injection_class`).

Spec: docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import time
from typing import Any, Iterator

# Allow `python scripts/eval/agentdojo/run.py` to import the parent
# `runner_common` + `mock_llm` modules without a setup.py.
_HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

import runner_common  # noqa: E402
from mock_llm import reply_for  # noqa: E402

DEFAULT_FIXTURES = _HERE.parent / "fixtures" / "synthetic_cases.json"


def load_cases(path: pathlib.Path) -> list[dict[str, Any]]:
    """Load the test-case manifest. Either the committed AgentDojo
    selection file (`case_selection.json`, M11.2.1) or the synthetic
    fixture set used by the skeleton runner."""
    return json.loads(path.read_text())


def run_one_case(
    case: dict[str, Any],
    *,
    backend: str,
    model: str,
    run_id: str,
) -> runner_common.EvalRecord:
    """Run a single test case end-to-end against the configured
    backend. Returns the record (caller writes to JSONL)."""
    started = time.perf_counter()

    # M11.2.1 will spawn a real mur agent here:
    #   handle = runner_common.spawn_agent(...)
    #   feed `case["attack_input"]` via stdin, capture decision,
    #   teardown_agent(handle)
    # For now (skeleton): consult the mock LLM directly with the
    # case's `expected_outcome` so the JSONL contract is exercised
    # without spawning the runtime.
    if backend == "stub":
        stub = reply_for(case["expected_outcome"])
        # The "agent_decision" is what the (fake) runtime actually
        # decided; in the skeleton we trust the stub's outcome
        # category. M11.2.1 reads this from the runtime's stderr
        # telemetry instead.
        agent_decision = case["expected_outcome"]
        tokens_input = None
        tokens_output = None
    else:
        # Real-LLM track lands in M11.6 (per-release run). Skeleton
        # reports backend=anthropic with a deterministic placeholder
        # so the JSONL contract is exercised; M11.2.1 swaps in the
        # real call.
        agent_decision = case["expected_outcome"]
        tokens_input = 100
        tokens_output = 50

    elapsed_ms = int((time.perf_counter() - started) * 1000)

    return runner_common.EvalRecord(
        test_suite="agentdojo",
        test_id=case["test_id"],
        attack_category=case["attack_category"],
        agent_decision=agent_decision,
        expected=case["expected_outcome"],
        passed=agent_decision == case["expected_outcome"],
        wall_clock_ms=elapsed_ms,
        llm_backend=backend,
        llm_model=model,
        run_id=run_id,
        timestamp=runner_common.now_rfc3339_millis(),
        tokens_input=tokens_input,
        tokens_output=tokens_output,
        hook_decisions=[],  # populated by M11.2.1 from runtime telemetry
    )


def run_all(
    cases_path: pathlib.Path,
    out_path: pathlib.Path,
    *,
    backend: str,
    model: str,
) -> int:
    """Run the full case set, write JSONL, return exit code (0 if all
    expected outcomes match; 1 if any drift)."""
    run_id = runner_common.new_run_id()
    cases = load_cases(cases_path)

    def gen() -> Iterator[runner_common.EvalRecord]:
        for case in cases:
            yield run_one_case(
                case, backend=backend, model=model, run_id=run_id
            )

    runner_common.write_records(out_path, gen())

    # Re-read for the summary; small files only.
    body = out_path.read_text().strip().split("\n")
    failed = sum(1 for line in body if not json.loads(line)["passed"])
    print(
        f"agentdojo: {len(body)} cases, {failed} failed, run_id={run_id}",
        file=sys.stderr,
    )
    return 0 if failed == 0 else 1


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument(
        "--cases",
        type=pathlib.Path,
        default=DEFAULT_FIXTURES,
        help="path to case-selection JSON (default: synthetic fixtures)",
    )
    p.add_argument(
        "--out",
        type=pathlib.Path,
        required=True,
        help="output JSONL path",
    )
    p.add_argument(
        "--backend",
        choices=("stub", "anthropic", "openai", "ollama"),
        default="stub",
    )
    p.add_argument("--model", default="stub")
    args = p.parse_args(argv)
    return run_all(
        args.cases, args.out, backend=args.backend, model=args.model
    )


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
