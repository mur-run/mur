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
sys.path.insert(0, str(_HERE))  # for `from grader import grade`

import runner_common  # noqa: E402
from mock_llm import reply_for  # noqa: E402

DEFAULT_FIXTURES = _HERE.parent / "fixtures" / "synthetic_harmbench_cases.json"


def load_cases(path: pathlib.Path) -> list[dict[str, Any]]:
    """Load the test-case manifest. Either the committed HarmBench
    selection file (`case_selection.json`, M11.3.1) or the synthetic
    fallback used by the skeleton runner."""
    return json.loads(path.read_text())


def _call_anthropic(
    behavior: str, model: str
) -> tuple[str, int | None, int | None]:
    """Send the behavior as a user message to Anthropic; return
    (response_text, input_tokens, output_tokens).

    Special case: Anthropic's server-side RAI content filter may block
    the request with HTTP 400 ("Output blocked by content filtering
    policy"). That's itself a refusal-at-the-platform-layer — for
    HarmBench purposes the agent did NOT produce harmful content, so
    we synthesize a refusal sentinel so the heuristic grader scores
    it as `refuse` and the eval doesn't crash mid-run.
    """
    import anthropic  # local import — only when backend=anthropic

    client = anthropic.Anthropic()
    try:
        msg = client.messages.create(
            model=model,
            max_tokens=1024,
            temperature=0.0,
            messages=[{"role": "user", "content": behavior}],
        )
    except anthropic.BadRequestError as e:
        body = getattr(e, "body", None) or {}
        err_msg = (
            (body.get("error") or {}).get("message", "") if isinstance(body, dict) else ""
        )
        if "content filtering" in err_msg.lower() or "blocked" in err_msg.lower():
            # Synthetic refusal containing a known refusal marker so
            # the heuristic grader returns "refuse".
            return (
                f"[platform-rai-block] I cannot help with that request. "
                f"(Anthropic server-side filter blocked the response: {err_msg!r})",
                None,
                None,
            )
        raise

    text = "".join(
        block.text for block in msg.content if hasattr(block, "text")
    )
    # Anthropic's safety stack can also emit a `stop_reason="refusal"`
    # which produces ~1 token of output and no text content. Synthesize
    # a refusal sentinel containing a known refusal pattern so the
    # heuristic grader returns "refuse" instead of "comply_unsafe" for
    # the empty string.
    stop_reason = getattr(msg, "stop_reason", None)
    if stop_reason == "refusal" and not text.strip():
        text = "[stop_reason:refusal] I cannot comply with that request."
    return (
        text,
        getattr(msg.usage, "input_tokens", None),
        getattr(msg.usage, "output_tokens", None),
    )


def _call_openai(
    behavior: str, model: str, base_url: str | None = None
) -> tuple[str, int | None, int | None]:
    """Send the behavior as a user message to an OpenAI-compatible endpoint.

    base_url=None → OpenAI; base_url="https://api.deepseek.com" → DeepSeek.
    Single-turn, no tool calls — no tool message ordering issues.
    """
    import os
    import openai

    api_key = os.environ.get("DEEPSEEK_API_KEY") or os.environ.get("OPENAI_API_KEY")
    client = openai.OpenAI(api_key=api_key, base_url=base_url)
    resp = client.chat.completions.create(
        model=model,
        max_tokens=1024,
        temperature=0.0,
        messages=[{"role": "user", "content": behavior}],
    )
    text = resp.choices[0].message.content or ""
    usage = resp.usage
    return (
        text,
        getattr(usage, "prompt_tokens", None),
        getattr(usage, "completion_tokens", None),
    )


def run_one_case(
    case: dict[str, Any],
    *,
    backend: str,
    model: str,
    run_id: str,
) -> tuple[runner_common.EvalRecord, dict[str, Any]]:
    """Run a single test case end-to-end against the configured
    backend. Returns the EvalRecord PLUS an "extras" dict (response_text,
    grader_kind, behavior) merged into the JSONL line so future graders
    can re-classify saved responses without re-spending API.
    """
    started = time.perf_counter()
    extras: dict[str, Any] = {}

    if backend == "stub":
        stub = reply_for(case["expected_outcome"])
        agent_decision = case["expected_outcome"]
        tokens_input = None
        tokens_output = None
        _ = stub
    elif backend == "anthropic":
        from grader import grade as harmbench_grade  # type: ignore

        prompt = case.get("behavior") or case.get("user_task_prompt", "")
        if not prompt:
            raise SystemExit(
                f"case {case['test_id']!r} missing 'behavior' field; "
                "did you select cases via M11.3.1 Phase A?"
            )
        response_text, tokens_input, tokens_output = _call_anthropic(
            prompt, model
        )
        agent_decision = harmbench_grade(response_text)
        extras["response_text"] = response_text
        extras["grader_kind"] = "heuristic_v1"
        extras["behavior"] = prompt
    elif backend in ("openai", "deepseek"):
        from grader import grade as harmbench_grade  # type: ignore

        prompt = case.get("behavior") or case.get("user_task_prompt", "")
        if not prompt:
            raise SystemExit(
                f"case {case['test_id']!r} missing 'behavior' field; "
                "did you select cases via M11.3.1 Phase A?"
            )
        base_url = "https://api.deepseek.com" if backend == "deepseek" else None
        response_text, tokens_input, tokens_output = _call_openai(prompt, model, base_url)
        agent_decision = harmbench_grade(response_text)
        extras["response_text"] = response_text
        extras["grader_kind"] = "heuristic_v1"
        extras["behavior"] = prompt
    else:
        raise ValueError(f"unsupported backend: {backend!r}")

    elapsed_ms = int((time.perf_counter() - started) * 1000)

    record = runner_common.EvalRecord(
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
    return record, extras


def run_all(
    cases_path: pathlib.Path,
    out_path: pathlib.Path,
    *,
    backend: str,
    model: str,
) -> int:
    run_id = runner_common.new_run_id()
    cases = load_cases(cases_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    # We need to merge `extras` (response_text + grader_kind + behavior)
    # into each JSONL line. The schema-clean path is `EvalRecord` only,
    # so we serialize manually here.
    failed = 0
    n = 0
    with out_path.open("a", encoding="utf-8") as f:
        for i, case in enumerate(cases):
            record, extras = run_one_case(
                case, backend=backend, model=model, run_id=run_id
            )
            line = json.loads(record.to_json_line())
            line.update(extras)
            f.write(json.dumps(line, separators=(",", ":"), sort_keys=True))
            f.write("\n")
            n += 1
            if not record.passed:
                failed += 1
            if backend == "anthropic" and (i + 1) % 10 == 0:
                print(
                    f"harmbench: {i + 1}/{len(cases)} cases…",
                    file=sys.stderr,
                )

    print(
        f"harmbench: {n} cases, {failed} failed, run_id={run_id}",
        file=sys.stderr,
    )
    # Score is data, not a gate: it measures MUR's hooks x the model x the
    # corpus, and a release controls only the first. Infrastructure failure
    # still exits non-zero — there is no blanket `except` here, so an API
    # error propagates and crashes the run. See the policy note in
    # .github/workflows/eval.yml.
    return 0


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
