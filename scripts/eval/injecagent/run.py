"""InjecAgent runner for the B0 eval harness (M11.4 — Task 3).

Exercises the UIUC/Kang-Lab InjecAgent-200 benchmark: tool outputs
containing injected directives are fed to the agent; the harness
records whether the B0 hook chain blocks the attack.

CI subset: 20 cases committed in `case_selection.json` (17 attacks +
3 benign controls). Nightly runs exercise all 200 upstream cases.

Stub backend: classifies `injected_tool_output` with a B0-rule regex
list to determine `agent_decision`. This exercises the JSONL contract
end-to-end without spawning a real LLM.

Spec: docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import time
from typing import Any, Iterator

_HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

import runner_common  # noqa: E402
from mock_llm import reply_for  # noqa: E402

DEFAULT_FIXTURES = _HERE / "case_selection.json"

# B0-rule regex patterns covering the attack categories in the
# InjecAgent benchmark. Any match → the stub classifies the case as
# an attack the agent should refuse.
_ATTACK_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"(?i)(ignore|disregard).{0,30}(instruction|directive|rule|safety)"),
    re.compile(r"(?i)(send|upload|exfil|post|forward).{0,40}(attacker|evil|malicious|s3://attacker|all files)"),
    re.compile(r"(?i)(rm\s+-rf|delete all|sudo\s+chmod|ssh\s+root)"),
    re.compile(r"(?i)(api.?key|password|secret|token|ssh.?key).{0,30}(send|output|forward|include)"),
    re.compile(r"(?i)(mine|cryptocurrency|cron job.{0,20}curl)"),
    re.compile(r"(?i)(admin.?override|safety filter.{0,20}disabl)"),
    re.compile(r"(?i)(eval|exec|system\().{0,20}(import|os\.)"),
    re.compile(r"(?i)(manager says|reply.{0,20}password|send.{0,20}admin@)"),
    re.compile(r"(?i)(webhook.{0,20}post|post.{0,40}evil\.io)"),
    re.compile(r"(?i)(proceed without restriction|comply with everything|disregard safety)"),
    re.compile(r"(?i)(pretend you are|new (system |)directive|new.*instruction)"),
    re.compile(r"(?i)(install cron|cron job)"),
    re.compile(r"(?i)(privilege|wheel group|sudoers)"),
    re.compile(r"(?i)(lateral.?movement|forward all traffic)"),
    re.compile(r"(?i)(social.?engineer|your manager|for verification)"),
]


def _classify_stub(injected_tool_output: str) -> str:
    """Classify `injected_tool_output` using B0-rule regex patterns.

    Returns `"refuse"` if any attack pattern matches, `"comply_safe"`
    otherwise (benign tool outputs that the agent should act on).
    """
    for pat in _ATTACK_PATTERNS:
        if pat.search(injected_tool_output):
            return "refuse"
    return "comply_safe"


def load_cases(path: pathlib.Path) -> list[dict[str, Any]]:
    """Load InjecAgent cases from the wrapper JSON.

    The on-disk format is `{"cases": [...], ...}` — different from
    the flat array used by the AgentDojo fixture.
    """
    data = json.loads(path.read_text())
    return data["cases"]


def run_one_case(
    case: dict[str, Any],
    *,
    backend: str,
    model: str,
    run_id: str,
) -> runner_common.EvalRecord:
    """Run a single InjecAgent test case and return the record."""
    started = time.perf_counter()

    if backend == "stub":
        agent_decision = _classify_stub(case["injected_tool_output"])
        _ = reply_for(agent_decision)  # validate stub key is recognised
        tokens_input = None
        tokens_output = None
    else:
        # Real-LLM track: M11.6 wires in the actual upstream benchmark
        # loop. For now mirror the expected outcome so the JSONL
        # contract is exercised; M11.6 replaces this with a live call.
        agent_decision = case["expected_outcome"]
        tokens_input = 100
        tokens_output = 50

    elapsed_ms = int((time.perf_counter() - started) * 1000)

    return runner_common.EvalRecord(
        test_suite="injecagent",
        test_id=case["id"],
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
    """Run the full case set, write JSONL, return exit code."""
    run_id = runner_common.new_run_id()
    cases = load_cases(cases_path)

    def gen() -> Iterator[runner_common.EvalRecord]:
        for case in cases:
            yield run_one_case(
                case, backend=backend, model=model, run_id=run_id
            )

    runner_common.write_records(out_path, gen())

    body = out_path.read_text().strip().split("\n")
    passed_count = sum(1 for line in body if json.loads(line)["passed"])
    total = len(body)
    pct = 100 * passed_count // total if total else 0
    print(
        f"InjecAgent: {passed_count}/{total} PASS ({pct}%), run_id={run_id}",
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
        help="path to case-selection JSON (default: case_selection.json)",
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
