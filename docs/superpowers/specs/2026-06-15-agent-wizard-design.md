# Agent Wizard — `mur agent wizard` CLI + Hub Specialist flow

Date: 2026-06-15
Status: Design (approved in brainstorming; pending spec review)

## Context

We built a team of specialized MUR agents by hand — `rustsmith`, then `pm` / `qa` /
`repo-manager` — following a repeatable method: define the role → research 2026 best practice →
author 4–6 dense skills → write a "definition-of-done" system prompt with HITL gates → set
least-privilege entitlements → create/attach/start → run a test→record→fix eval loop. That method
is currently captured only as a human-followed skill (`specialized-agent-builder` in
`~/.mur/skills/`) and as ad-hoc operator commands.

We want to **productize the method as a first-class feature** with two surfaces that share one
engine:

1. **`mur agent wizard`** — a CLI command that walks an operator through building a specialized
   agent (interactive, plus a non-interactive mode for scripting/CI).
2. **MUR Hub** — the existing "create Agent" flow is already a wizard (`Persona → Name → Style →
   Behavior → Photo → Render`), focused on appearance/companion identity. The specialist
   dimension must fold **seamlessly** into that flow, and the research + eval stages must run
   **automatically inside the Hub** with live progress.

The intended outcome: a user can create a high-quality, least-privilege specialized agent — with
researched skills, a DoD system prompt, scoped entitlements, and a passing eval — from either the
terminal or the Hub, without hand-assembling YAML.

## Goals

- One shared engine in `mur-core`; CLI and Hub are thin drivers (no duplicated logic).
- Custom roles are first-class; curated role presets are accelerators, not a closed list.
- LLM-powered research, skill authoring, DoD-prompt drafting, and eval, with **graceful
  degradation** when no model / no web-search tool is available.
- A **human draft-review gate before any agent is created**.
- Eval that auto-fixes and re-runs, plus security suites for high-risk agents.

## Non-Goals

- Replacing the existing companion/appearance wizard steps (they stay; specialist is additive).
- Retiring the `specialized-agent-builder` skill — it is retained as the *methodology guide for
  Claude*; the wizard is the *executable tool*. They complement.
- Running the created agent's own day-to-day work, or relaxing its runtime HITL gates.
- A general workflow/DAG editor.

## Architecture

### Single engine, two drivers

```
 CLI  `mur agent wizard`            Hub  Specialist branch (Step 0 fork)
 (interactive prompts +            (Tauri cmds wizard_spec_* +
  progress printer; --headless)     wizard-progress events)
            \                                   /
             \                                 /
        mur-core::agent_wizard  (state-machine + async stage runner)
              progress via callback/channel (mirrors render_agent_expressions)
                                  |
   depends on existing capability:
   model registry → cc-proxy · mur skill new/validate · agent create/perm/skill add ·
   install-service · mur agent send (eval driver) · mur agent eval (AgentDojo/HarmBench) ·
   role catalog (YAML manifests)
```

The Hub already depends on `mur-core`, so both surfaces call the same engine functions. The
engine exposes a state machine whose stages each return a structured result and stream progress
through a callback/channel — the same async-progress pattern the Hub already uses for expression
rendering (`render_agent_expressions` → `agent-render-progress`).

**Rejected alternatives:** (a) delegate LLM work to a transient "builder agent" — more
indirection, harder to make a deterministic backbone + progress; (b) CLI and Hub each reimplement
— guaranteed drift. Both rejected in favor of the shared `mur-core` module.

### Stages

The runner executes ordered stages; LLM stages are skippable and emit progress:

1. **Define role** — `name`, `display_name`, one-sentence charter, **risk level**. Risk drives
   HITL defaults, the entitlement preset, and whether security suites run in eval.
2. **Research** *(LLM, optional)* — the configured model drafts the 4–6 skill topics from the role
   description + its knowledge. If a **search MCP server** is wired into the runtime, augment with
   live research using the 2026 two-layer pattern: an LLM-native search API for discovery →
   crawl-and-extract for clean source text → grounded skills with citations. **Provider-agnostic**
   (Tavily / Exa / Brave / Firecrawl all ship MCP servers; no hardcoded provider — default
   documented reference is Tavily for its citation-first design). Skipped (clearly flagged) when no
   model or no search MCP.
3. **Author skills** *(LLM, optional)* — generate 4–6 `skill.yaml` drafts (imperative rules each
   with a *why*, trigger-rich descriptions); validate each with `mur skill validate`.
4. **DoD system prompt** *(LLM, optional)* — persona + operating-discipline gate + HITL rules +
   "never fabricate output" honesty rule + narration.
5. **Entitlement preset** — apply least-privilege read/write/spawn/host/tool scoped to the role &
   risk level (deny `~/.ssh ~/.aws ~/.gnupg`).
6. **★ Human draft-review gate (HITL, blocking)** — present the generated skills + DoD prompt +
   entitlements for the human to **review, edit, and approve BEFORE anything is created**. Nothing
   is written to `~/.mur/agents/` until approval. CLI: print drafts + open `$EDITOR` / accept;
   Hub: a review screen with editable sections + an Approve button.
7. **Create + attach + start** — only after approval: `agent create` (set `model_ref`), write
   `sys_prompt.md`, apply entitlements, `skill add` each skill, `install-service` + start.
8. **Eval** *(LLM, optional)* — generate ~3 role tasks (kept lean to avoid eval-overfitting) and
   drive the new agent via `mur agent send`. Score with **per-dimension graders**, not one
   monolithic judge: *safety* (forbidden-action probe) and *uses-its-skills* are **deterministic
   checks**; *in-role correctness* and *honesty* use an LLM judge. **Pass bar: every rubric
   dimension ≥ 4/5 AND overall ≥ 0.90 AND zero safety violations** (safety is a hard,
   non-negotiable gate). **On a miss, auto-revise the offending skill/prompt and re-run — capped at
   N = 2 rounds** (the empirical self-correction sweet spot; beyond ~2 rounds gains diminish and
   risk regressing correct output). For high-risk agents also run the existing `mur agent eval`
   security suites (AgentDojo / HarmBench). Stream scores; after the cap, **never claim success** —
   surface remaining failures to the human and offer keep/discard. The passing task set becomes the
   agent's **regression set** (guarded near 100% thereafter). Records land in
   `~/.mur/agents/<name>/eval-runs/`.

### Role model — custom-first + extensible catalog

- **Custom role is the primary path:** describe any role in words → stages 2–5 generate
  everything.
- **Presets are accelerators, stored as data, not code:** YAML role manifests under
  `~/.mur/wizard/roles/` plus shipped defaults (in `mur-core` resources). Users/community add roles
  by dropping a manifest — **no hardcoded list** (honors CLAUDE.md Rule 1).
- A role manifest references a skill set + DoD-prompt template + entitlement preset + risk level +
  suggested eval tasks. Seed a categorized starter catalog (~10): PM, QA, Repo Manager, RustSmith,
  DevOps/SRE, Security reviewer, Tech writer, Data/ML, Frontend, Support-triage.

### CLI surface

- `mur agent wizard` — interactive: prompts for role (pick a catalog preset or "custom"), runs
  stages with a progress printer, **pauses at the draft-review gate** (show drafts; edit via
  `$EDITOR`; confirm), then creates + evals.
- `mur agent wizard --config <role.yaml>` / `--headless` — non-interactive for scripting/CI; the
  draft-review gate becomes an explicit `--yes` acknowledgement or a dry-run + apply two-step.
- `mur agent wizard --no-llm` — force scaffold-only (skip research/author/eval LLM stages).

### Hub integration (Step 0 fork)

- The create-Agent wizard gains a **Step 0: "What are you creating?"** → Companion (existing flow,
  unchanged) / Specialist / Both.
- Specialist inserts steps: **Role** (catalog or custom + risk) → **Generating** (research →
  skills → prompt → entitlements, async with `wizard-progress`) → **★ Review drafts** (editable
  skills/prompt/entitlements + Approve) → **Create** → **Eval** (live scores; auto-fix rounds).
  "Both" then continues into the existing appearance steps.
- New Tauri commands `wizard_spec_*` (e.g. `wizard_spec_start`, `wizard_spec_review`,
  `wizard_spec_approve`, `wizard_spec_cancel`) call the same `mur-core::agent_wizard` engine and
  emit `wizard-progress` events (mirroring the render pattern). Pure-companion path is untouched.

## Data flow

Role input (preset or custom description) → engine stages 1–5 produce in-memory **drafts**
(skills, prompt, entitlements) → progress events to the driver → **draft-review gate** (human
edits/approves) → stage 7 writes to `~/.mur/agents/<name>/` (profile.yaml, sys_prompt.md,
skills/*, entitlements) and starts the service → stage 8 eval drives the live agent via
`mur agent send`, writes records + scores → driver shows the result. No disk writes to the agent
dir before approval.

## Error handling & safety

- **Graceful degradation:** no model → skip LLM stages, fall back to catalog/template drafts,
  clearly flagged; no web-search tool → model-knowledge drafting only. Never fabricate research,
  skill content, or eval results.
- **Draft-review gate is mandatory and blocking** — nothing is created until the human approves;
  edits are honored verbatim.
- **Eval honesty:** after N auto-fix rounds, do not claim success; surface remaining failures and
  scores to the human; offer to keep or discard the agent.
- **Validation:** every generated skill must pass `mur skill validate` (schema + security scan)
  before it can appear in the review gate.
- **Runtime HITL preserved:** the created agent keeps its own HITL gates (e.g. repo-manager's
  merge/release confirmations); the wizard's scope ends at create + start + eval.
- **Least privilege:** entitlement preset is scoped to the role/risk; deny sensitive paths by
  default.

## Testing

- Engine state machine + stage result types: unit tests (`cargo nextest run`).
- Stage runner with **mock model + mock eval providers** (pattern: `MockImageGenProvider`) for
  deterministic offline tests of research/author/eval flows and the draft-review gate.
- Role catalog loading/merge (shipped + `~/.mur/wizard/roles/`): unit tests, including
  no-hardcoded-list extensibility.
- CLI `--headless --config` end-to-end smoke (creates a throwaway agent, asserts files + skills).
- Hub `wizard_spec_*` command shells: thin-wrapper tests that the engine is invoked and events
  emitted.
- Graceful-degradation paths (`--no-llm`, no web tool) explicitly tested.

## Implementation phasing (for the plan)

1. `mur-core::agent_wizard` engine: state machine, stage result types, progress channel,
   entitlement presets, role-manifest loader (catalog). Deterministic stages 1, 5, 6(gate), 7.
2. CLI `mur agent wizard` (interactive + `--headless`/`--config`/`--no-llm`) driving the engine.
3. LLM stages 2–4 (research/author/prompt) via model registry → cc-proxy, with mock providers +
   graceful skip.
4. Eval stage 8: rubric loop + auto-fix + AgentDojo/HarmBench wiring; records.
5. Hub: Step 0 fork + Specialist steps + `wizard_spec_*` commands + `wizard-progress` UI +
   draft-review screen.
6. Seed the starter role catalog; keep `specialized-agent-builder` skill as the guide.

## Resolved decisions (researched 2026-06-15)

- **Eval auto-fix rounds: N = 2.** Reflexion / Self-Refine evidence shows gains concentrate in the
  first 1–2 self-correction rounds; beyond ~2 the cost/latency rises and already-correct output can
  regress. Cap at 2.
- **Pass bar: every rubric dimension ≥ 4/5 AND overall ≥ 0.90 AND zero safety violations.** Mirrors
  the literature's "deterministic = pass AND judge > 0.90" pattern; raised from the initial 0.8 to
  0.9. Per-dimension graders; deterministic checks for safety + skill-usage; LLM judge only for
  subjective dimensions. Suite kept lean (~3 tasks) and reused as the agent's regression set.
- **Research search source: provider-agnostic search MCP, two-layer (discovery → extract).**
  Wired in phase 3; default documented reference Tavily; no hardcoded provider; graceful skip when
  no search MCP is present.

## Open questions

- None blocking. Concrete search-MCP provider selection is a phase-3 configuration choice, not a
  design blocker (graceful skip covers its absence).
