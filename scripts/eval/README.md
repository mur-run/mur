# mur B0 eval harness

Closes the v1 B0 quantitative acceptance gate via two upstream
benchmarks: AgentDojo (indirect-injection) + HarmBench (jailbreak +
agentic misuse).

This directory is the **measurement infrastructure**, not new B0
protection logic. The B0 hooks are already implemented in
`mur-agent-runtime/src/hooks/` (M7.x + M8.x + M9.x + M10).

## Status

| Milestone | State |
|---|---|
| M11.0 design | shipped (`docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md`) |
| M11.1 scaffolding | this PR |
| M11.2 AgentDojo runner | TODO |
| M11.3 HarmBench runner | TODO |
| M11.4 report aggregator | TODO |
| M11.5 CI integration | TODO |
| M11.6 first real-run baseline | TODO |

The Python harness (M11.2 / M11.3), the report aggregator (M11.4),
and the CI workflow (M11.5) are not yet implemented. This directory
currently holds:

- license attribution for the upstream benchmarks
- pinned dependency requirements
- this README (status + pointers)

## Two-track output

- **CI gate (mock-LLM):** runs on every PR. Free, deterministic.
  Detects regressions in the B0 hook chain.
- **Release gate (real-LLM):** runs once per release tag against
  Anthropic Sonnet 4.5+ at `temperature=0`. Manually triggered to
  cap cost. Result committed to `eval-results/v<X.Y.Z>.jsonl`.

## Selection seed

The 50-case subsets (one each from AgentDojo + HarmBench) are
selected with a fixed seed so the choice is reproducible:

```
seed = int(SHA256("mur-b0-acceptance-2026").hexdigest()[:8], 16)
```

Selection scripts will land in M11.2 / M11.3 and emit
`scripts/eval/{agentdojo,harmbench}/case_selection.json` (committed).

## Costs

Per release-gate run (50 + 50 tests, Anthropic Sonnet 4.5,
~10 K input + 200 output tokens / test): ~$5.

Hard cap enforced by M11.5: fail loudly if `tokens_input > 50K` total
OR `wall_clock > 30 min`.

## See also

- `docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md` (full design)
- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1 (B0 acceptance gate target)
- `LICENSES.md` (upstream attribution)
- `requirements.txt` (pinned upstream packages)
