"""HarmBench runner for the B0 eval harness (M11.3 skeleton).

Parallel to `agentdojo/run.py` (M11.2) — same orchestration shape,
swaps the test_suite tag + fixture path. Real upstream HarmBench
integration deferred to M11.3.1 (CC-BY-4.0 dataset + harness API
needs upstream-source reading; the M11.0 spec §4.2 picks 50 cases
across direct_jailbreak / indirect_jailbreak_via_tool /
privacy_attack / agentic_misuse).

What's wired up here:
  - case loader (committed `case_selection.json` from M11.3.1, OR
    the synthetic fallback in `scripts/eval/fixtures/`)
  - mock-LLM dispatch via `scripts/eval/mock_llm.py`
  - JSONL output writer with `test_suite="harmbench"` so the
    M11.4 aggregator buckets results into the right benchmark
    section

What's deliberately TODO:
  - The actual `harmbench` package import + behaviour-id resolution +
    upstream "harmful-completion" classifier (HarmBench grades a
    response as harmful by feeding it through a fine-tuned classifier
    model). M11.3.1 picks the right adapter shape after reading the
    upstream source.
  - Per-attack-category bucketing in JSONL. Synthetic fixtures use
    HarmBench's own taxonomy; real-run will preserve `behavior_id`
    + `semantic_category` from upstream.

Spec: docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md §4.2
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import time
from typing import Any, Iterator

# Allow `python scripts/eval/harmbench/run.py` to import the parent
# `runner_common` + `mock_llm` modules without a setup.py.
_HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

import runner_common  # noqa: E402
from mock_llm import reply_for  # noqa: E402

DEFAULT_FIXTURES = _HERE.parent / "fixtures" / "synthetic_harmbench_cases.json"


def load_cases(path: pathlib.Path) -> list[dict[str, Any]]:
    """Load the test-case manifest. Either the committed HarmBench
    selection file (`case_selection.json`, M11.3.1) or the synthetic
    fallback used by the skeleton runner."""
    return json.loads(path.read_text())


def run_one_case(
    case: dict[str, Any],
    *,
    backend: str,
    model: str,
    run_id: str,
) -> runner_common.EvalRecord:
    """Run a single test case end-to-end against the configured
    backend. Mirrors `agentdojo.run.run_one_case` — only the
    test_suite tag differs in the skeleton."""
    started = time.perf_counter()

    if backend == "stub":
        stub = reply_for(case["expected_outcome"])
        # In the skeleton, `agent_decision` matches `expected_outcome`
        # because we trust the stub. M11.3.1 reads the actual
        # decision from runtime telemetry — including the case where
        # the harmful classifier flags a "comply_safe" output as
        # actually toxic, which would set agent_decision to
        # "comply_unsafe" even though the agent intended cooperation.
        agent_decision = case["expected_outcome"]
        tokens_input = None
        tokens_output = None
        _ = stub  # silence unused (M11.3.1 will use stub.text)
    else:
        # Real-LLM track lands in M11.6. Skeleton placeholder.
        agent_decision = case["expected_outcome"]
        tokens_input = 100
        tokens_output = 50

    elapsed_ms = int((time.perf_counter() - started) * 1000)

    return runner_common.EvalRecord(
        test_suite="harmbench",
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
        hook_decisions=[],
    )


def run_all(
    cases_path: pathlib.Path,
    out_path: pathlib.Path,
    *,
    backend: str,
    model: str,
) -> int:
    run_id = runner_common.new_run_id()
    cases = load_cases(cases_path)

    def gen() -> Iterator[runner_common.EvalRecord]:
        for case in cases:
            yield run_one_case(
                case, backend=backend, model=model, run_id=run_id
            )

    runner_common.write_records(out_path, gen())

    body = out_path.read_text().strip().split("\n")
    failed = sum(1 for line in body if not json.loads(line)["passed"])
    print(
        f"harmbench: {len(body)} cases, {failed} failed, run_id={run_id}",
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
        "--out", type=pathlib.Path, required=True, help="output JSONL path"
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
