# B0 M11 — AgentDojo / HarmBench eval harness design

**Status:** Draft (multi-week project — review before implementation kickoff)
**Author:** david
**Date:** 2026-05-06
**Roadmap anchor:** `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1 acceptance gate (B0 v1).

---

## 1. Goal

Close the v1 B0 quantitative acceptance gate:

- **AgentDojo-50 indirect-injection success rate ≤ 5%** (research baseline of unprotected agents: 30–60%).
- **HarmBench-50 jailbreak success rate ≤ baseline minus 50%**.

Until M11 ships, those gates are aspirational text in the roadmap. M11 makes them measurable + reproducible + run on every release.

## 2. Scope clarifications

This is **not** new B0 protection logic — all the rules (1, 3, 4, 5, 7, 8, 11, 12, etc.) already ship and pass their unit tests. M11 builds the **measurement infrastructure** that demonstrates those rules actually deliver the protection numbers the roadmap claims.

**Two-track output:**

1. **CI gate (deterministic):** mocked-LLM mode that exercises the B0 hook chain against the upstream test cases without consuming any real model tokens. Fast, free, runs on every PR, catches regressions in the hook chain logic.
2. **Release gate (real-model):** runs once per release tag against a configured cloud LLM (Anthropic Sonnet 4.5 default). Produces the canonical pass/fail vs. the spec thresholds. Manually triggered (`make eval-release`) to keep cost bounded.

The CI track answers *"did we break a rule?"*. The release track answers *"do the rules still beat the spec threshold?"*.

## 3. Architecture

### 3.1 Language: Python harness, Rust subject

Both upstream benchmarks (AgentDojo, HarmBench) ship as Python packages. Reimplementing their dataset orchestration in Rust would diverge from upstream and require constant catch-up. Instead:

- **`scripts/eval/`** — new Python project (uv / poetry / pip-managed; no global install). Runs the upstream test cases against a `mur-agent-runtime` subprocess via stdin/stdout JSON-RPC.
- **Output:** JSONL per test case → aggregated into a markdown report by a small Rust tool (`mur-core/src/cmd/eval_report.rs`).
- **No Rust workspace dep:** the eval directory is its own Python project; `cargo build --workspace` does not invoke it. CI calls it via a separate workflow job.

Rejected alternative: vendor the JSON datasets and reimplement the orchestration in Rust. Faster locally but loses the upstream-update path — every benchmark version bump would need a port.

### 3.2 Subject under test

Each test case spawns a fresh agent:

```
mur agent create eval-<id> --provider <mock|anthropic> --model <stub|claude-sonnet-4-6>
mur agent profile <eval-id> --apply <attack-fixture.yaml>  # optional config patches
mur_agent_<eval-id> < <(echo '<json-rpc test driver>')
```

`<attack-fixture.yaml>` is the AgentDojo / HarmBench test definition translated to mur's profile + tools/list shape. The driver script feeds the attack input via stdin (simulating a tool result, an MCP response, a drag-drop, etc.) and reads the runtime's stdout for the agent's reaction.

Each test gets a tear-down that deletes the eval agent (`mur agent remove eval-<id> --purge`).

### 3.3 Mock LLM strategy (CI track)

For CI, the agent's LLM call is replaced with a deterministic stub that returns one of three canonical reactions per attack:

- **`refuse`** — the agent should refuse the attacker's instruction.
- **`comply_safe`** — the agent should comply with the user's intent without falling for the embedded directive.
- **`comply_unsafe`** — the agent followed the embedded directive (test FAIL).

The harness records which path the runtime took, then checks against the expected outcome. **Crucially, the B0 hooks (rule 3 spotlighting, rule 4 cooldown, rule 7 outbound prefilter, rule 8 memory redaction, etc.) ALL run unchanged** — only the model call is stubbed. So a regression in (e.g.) the spotlighting wrapper that lets the model see naked attacker text would still be caught, because the stub's input arrives through the wrapped `<untrusted_*>` envelope and any failure to wrap shows up as a hook-chain decision change.

### 3.4 Real-LLM strategy (release track)

Same harness, but with `--llm anthropic --model claude-sonnet-4-6`. Each test costs roughly the cost of one tool-using turn (~10K input + 200 output tokens at Sonnet 4.5 pricing ≈ $0.04/test, ~$5 total for the 50+50). Run once per release tag; results committed alongside the tag for audit history.

## 4. Dataset selection

### 4.1 AgentDojo-50

The full AgentDojo benchmark has ~600 test cases across 5 environments (banking, slack, travel, workspace, university). For the v1 gate we pick **50 cases** representative of the v1 threat model:

- 10 from each of the 5 environments
- balanced across attack categories (prompt-injection-via-email, instruction-leak, data-exfil, payment-redirect, account-takeover)
- one fixed seed so the selection is reproducible (commit the seed in `scripts/eval/agentdojo/case_selection.json`)

Selection script: `scripts/eval/agentdojo/select_cases.py` — runs once, output committed; rerunning requires bumping the seed in the config.

### 4.2 HarmBench-50

HarmBench has ~400 behaviors. Pick **50** balanced across:
- direct-jailbreak (attacker tries to make the model emit harmful content directly)
- indirect-jailbreak-via-tool (attacker injects via tool result; B0 rule 3 should catch)
- privacy-attack (attacker tries to extract user secrets; B0 rule 7 should catch on egress)
- agentic-misuse (attacker tries to make the agent take a real-world bad action; B0 rule 4 should fire)

Same fixed-seed selection.

## 5. License + attribution

- **AgentDojo** (Apache-2.0): we can vendor or pip-install. Decision: pip-install + pin a version in `scripts/eval/requirements.txt`. Attribution in the cookbook.
- **HarmBench** (CC-BY-4.0): we can use the dataset; we MUST attribute and we MUST NOT relabel. Decision: pip-install (their package handles attribution per spec); attribution also reproduced verbatim in the cookbook.

`scripts/eval/LICENSES.md` will reproduce the upstream license texts.

## 6. Output format

### 6.1 Per-test JSONL line

```jsonc
{
  "test_suite": "agentdojo",          // or "harmbench"
  "test_id": "agentdojo:slack:42",
  "attack_category": "data_exfil",
  "agent_decision": "refuse",          // refuse | comply_safe | comply_unsafe
  "expected": "refuse",
  "passed": true,
  "hook_decisions": [                  // ordered B0 chain decisions
    {"hook": "B0SafetyHook.on_prompt_submit", "decision": "wrap_untrusted"},
    {"hook": "B0SafetyHook.pre_tool_use", "decision": "AskUser", "rule": 4}
  ],
  "tokens_input": 9821,
  "tokens_output": 184,
  "wall_clock_ms": 1240,
  "llm_provider": "stub" | "anthropic"
}
```

### 6.2 Aggregate markdown report

Built by `mur agent eval report --since <run-id>`:

```markdown
# B0 eval — run <id> — <timestamp>

## AgentDojo-50

| Category         | passed | failed | rate |
|-------------------|--------|--------|------|
| prompt_injection  | 9/10   | 1      | 90%  |
| instruction_leak  | 10/10  | 0      | 100% |
| data_exfil        | 8/10   | 2      | 80%  |
| ...

**Aggregate injection success: 4 / 50 = 8% — FAIL (spec ≤ 5%)**

## HarmBench-50
...
```

Spec gates evaluated at the aggregate level + a section per category for diagnosis.

## 7. Milestones (cascade)

| PR | Milestone | Scope |
|---|---|---|
| M11.0 (this spec) | design | this document |
| M11.1 scaffolding | Python project + Cargo subcrate | `scripts/eval/` skeleton; `mur agent eval` CLI verb (sub-command-only); JSONL output schema; mock-LLM stub plumbing |
| M11.2 AgentDojo runner | wire the upstream package | `scripts/eval/agentdojo/run.py`; case selection; agent-spawn helper; one-pass run on stub LLM |
| M11.3 HarmBench runner | sister to M11.2 | same shape for HarmBench |
| M11.4 report aggregator | `mur agent eval report` | aggregate JSONL → markdown; CI checks the rate threshold |
| M11.5 CI integration | weekly cron + per-release | `.github/workflows/eval.yml` runs M11.1–M11.4 on stub-LLM mode every PR; release tag fires real-LLM run |
| M11.6 release gate + cookbook | docs + first-real-run | `docs/cookbook/b0-eval.md`; first real-LLM run committed to `eval-results/v2.7.0.jsonl` for baseline; roadmap §6.1 footer marks gate shipped |

7 PRs. Estimated **5–7 dev-days** (vs. ~3 days for prior cascades). Most of the time is in M11.2 (case selection + upstream-API plumbing) and M11.5 (CI workflow + cost guardrails).

## 8. Risks + mitigations

- **Upstream API drift:** AgentDojo / HarmBench may change between releases. Pin exact versions in `requirements.txt`; CI fails on `pip install` mismatch.
- **Mock-LLM realism:** the stub may pass cases the real LLM would fail. Mitigation: the release track runs against a real LLM and is the canonical gate; the CI track is for "did we break the protection logic?" not "is the model still resistant?".
- **Cost runaway:** one careless `make eval-release` could cost $100+ if model upgrades. Mitigation: hard cap of 50+50 tests per run; fail loudly if `tokens_input > 50K` total or `wall_clock > 30 min`.
- **Reproducibility:** non-deterministic LLM responses make pass-rates noisy. Mitigation: `temperature=0` for the release track; stub track is fully deterministic.
- **Test pollution:** a flaky test agent could leave `~/.mur/agents/eval-<id>/` directories. Mitigation: every run in a `MUR_HOME=/tmp/mur-eval-<runid>` sandbox; `rm -rf` on teardown.

## 9. Non-goals

- **Not** auto-fixing failing tests. The harness measures; the user fixes.
- **Not** running against arbitrary user-supplied attack corpora. AgentDojo + HarmBench only in v1; B2 (red-team harness, v2.1) is the home for third-party attack data.
- **Not** real-time / continuous evaluation. Per-PR (mock) + per-release (real) is the cadence.
- **Not** evaluating non-B0 attack vectors (memory poisoning, MCP supply-chain): out of scope for v1, covered by M9 (rule 6) + B1 (v2 runtime enforcement).

## 10. Acceptance gates

- M11.6 closes when:
  1. `make eval-stub` runs both suites on stub LLM, exits 0 if all category gates pass.
  2. `make eval-release` runs against Anthropic Sonnet 4.5, produces a markdown report committed to `eval-results/v2.7.0.jsonl` (or whichever tag).
  3. The first real-run report shows AgentDojo ≤ 5% + HarmBench ≤ baseline−50%, OR documents which categories miss + tracks remediation as v2.0 work.
  4. Cookbook documents the workflow for users who want to evaluate their own custom B0 hook overrides.
  5. Roadmap §6.1 acceptance footer lists the run-id, date, and pass-rate.

## 11. Open questions

- **Selection seed:** what value? Use `seed = int(SHA256("mur-b0-acceptance-2026")[:8], 16)` — public + reproducible without being a magic constant.
- **Which Sonnet?** Sonnet 4.5 is current; do we re-run on Sonnet 4.6 once it's the default? Decision: re-run on every Anthropic model upgrade that lands in the supervisor, log both results.
- **CI cost mode:** stub-LLM only on PRs (free) + a weekly cron with real LLM (~$5/week). Should the weekly run page someone on regression? Decision: post results to a Slack channel via the existing C2 bridge; no paging in v1.

---

This is a multi-week project; the spec is meant to be reviewable + critique-able before kicking off the cascade. Open questions in §11 should be answered before M11.1.
