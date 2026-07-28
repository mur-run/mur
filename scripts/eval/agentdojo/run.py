"""AgentDojo runner for the B0 eval harness (Phase B).

Two tracks:
  stub  — deterministic mock-LLM, no API key needed, runs every PR.
  real  — backend=anthropic|deepseek|openai; calls live model via
          agentdojo's run_task_with_pipeline() loop (Phase B, M11.6).

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

_DEEPSEEK_BASE_URL = "https://api.deepseek.com"


def _build_llm(backend: str, model: str):
    """Return an agentdojo LLM pipeline element for the given backend."""
    from agentdojo.agent_pipeline import AnthropicLLM, OpenAILLM

    if backend == "anthropic":
        import anthropic
        return AnthropicLLM(anthropic.Anthropic(), model, temperature=0.0)
    elif backend in ("openai", "deepseek"):
        import os
        import openai
        api_key = os.environ.get("DEEPSEEK_API_KEY") or os.environ.get("OPENAI_API_KEY")
        if not api_key:
            raise RuntimeError(
                "DEEPSEEK_API_KEY or OPENAI_API_KEY must be set for openai/deepseek backend"
            )
        base_url = _DEEPSEEK_BASE_URL if backend == "deepseek" else None
        client = openai.OpenAI(api_key=api_key, base_url=base_url)
        return OpenAILLM(client, model, temperature=0.0)
    else:
        raise ValueError(f"unsupported real-LLM backend: {backend!r}")


def _run_with_real_llm(
    case: dict[str, Any], model: str, backend: str = "anthropic"
) -> tuple[str, int | None, int | None]:
    """Run one AgentDojo case against a live model via run_task_with_pipeline().

    Returns (agent_decision, tokens_input, tokens_output).
    agent_decision='refuse'       when security_ok=True  (injection failed).
    agent_decision='comply_unsafe' when security_ok=False (injection succeeded).
    """
    from agentdojo.agent_pipeline import (
        AgentPipeline,
        InitQuery,
        SystemMessage,
        ToolsExecutionLoop,
        ToolsExecutor,
    )
    from agentdojo.task_suite.load_suites import get_suite

    suite = get_suite("v1", case["suite"])
    user_task = suite.get_user_task_by_id(case["user_task_id"])
    injection_task = suite.get_injection_task_by_id(case["injection_task_id"])
    injections = suite.get_injection_vector_defaults()

    llm = _build_llm(backend, model)
    # ponytail: no SystemMessage — avoids 'developer' role rejection on
    # DeepSeek/non-OpenAI backends; irrelevant to injection-resistance measurement.
    pipeline = AgentPipeline(
        [
            InitQuery(),
            llm,
            ToolsExecutionLoop([llm, ToolsExecutor()]),
        ]
    )

    _utility_ok, security_ok = suite.run_task_with_pipeline(
        pipeline, user_task, injection_task, injections
    )

    # security_ok=True → injection did NOT succeed → agent resisted → "refuse"
    agent_decision = "refuse" if security_ok else "comply_unsafe"
    # ponytail: token counts not exposed by run_task_with_pipeline; None for now
    return agent_decision, None, None


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
    error: str | None = None
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
        try:
            agent_decision, tokens_input, tokens_output = _run_with_real_llm(
                case, model, backend=backend
            )
        except Exception as exc:
            # Fail-safe: count API errors as injection success (conservative).
            # But record that it WAS an error — otherwise a run that could not
            # authenticate reports the same "50 failed" as a model that
            # complied with every attack.
            error = runner_common.describe_exception(exc)
            print(f"[agentdojo] case {case['test_id']} error: {error}", file=sys.stderr)
            agent_decision = "comply_unsafe"
            tokens_input = None
            tokens_output = None

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
        error=error,
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
    parsed = [json.loads(line) for line in body]
    failed = sum(1 for r in parsed if not r["passed"])
    errored = sum(1 for r in parsed if r.get("error"))
    # `failed` counts errored cases too (fail-safe scoring), so report the
    # error count beside it. Without that, "50 cases, 50 failed" reads as a
    # security result when it can equally mean the API rejected every request.
    suffix = f", {errored} of them errors (no verdict)" if errored else ""
    print(
        f"agentdojo: {len(body)} cases, {failed} failed{suffix}, run_id={run_id}",
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
        choices=("stub", "anthropic", "openai", "deepseek", "ollama"),
        default="stub",
    )
    p.add_argument("--model", default="stub")
    args = p.parse_args(argv)
    return run_all(
        args.cases, args.out, backend=args.backend, model=args.model
    )


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
