# Deep Research UX Simplification — Design

Date: 2026-07-13
Status: Approved (brainstorm)
Related: `2026-07-09-mur-native-deep-research-design.md` (the underlying pipeline; unchanged here)

## Problem

Native deep-research works end-to-end but the operator flow is expert-only:
`provision --grant-egress --model … --yes`, manual worker start, gateway re-pin after
binary swaps, then `run`. Goal: first-time setup via an interactive wizard, daily use
via one command, plus a Hub GUI page. The research pipeline itself (fleet, gateway,
router/worker/verify skills) is out of scope — this is a UX shell over existing code.

## Decisions (from brainstorm)

- Bare `mur deep-research` (configured) → status panel, not a wizard.
- `mur deep-research setup` → interactive wizard; re-runnable, idempotent.
- `mur deep-research "question"` → preflight + auto-repair + run; workers stay
  running afterwards (faster next run).
- Phase 1 = CLI; Phase 2 = Hub GUI page on the same core functions.
- `provision` / `run` subcommands remain unchanged as the advanced path.

## Phase 1 — CLI

### `mur deep-research setup` (wizard)

Terminal Q&A, one question at a time:

1. **Model** — list registry aliases from `models.yaml` (default: concierge's model_ref).
2. **Worker count** — default 4.
3. **Per-run budget (USD)** — default 10; feeds the existing loop budget guard.
4. **Egress consent** — print the same warning text as `--grant-egress`; require the
   literal word `yes`. Never defaulted, never implied, never skippable by wizard flow.

Then call the existing `cmd_provision(...)` + fleet creation with the collected
answers. Answers persist in existing homes: model → worker profiles' `model_ref`,
budget → the fleet's `loop.budget_usd`, worker count → the provisioned set. No new
config file. Idempotent: existing workers are skipped/updated, not duplicated. If stdin
is not a TTY, error out pointing at the flag-based `provision` path.

### Bare command + smart run

- `mur deep-research` (no args): status panel — workers (name, running?), gateway
  pin status, model, egress granted?, last report path. If never set up, print a
  pointer to `setup`.
- `mur deep-research "question"`: preflight then run:
  - workers not running → start them;
  - gateway pin drifted → re-pin (`pin --force` equivalent);
  - egress not granted → STOP with instructions to run `setup` (never auto-grant);
  - then execute the existing run loop with the configured budget; print the report
    path on convergence; leave workers running.

## Phase 2 — Hub GUI page

Sidebar item "Deep Research" with three blocks:

1. **Wizard** — same four questions as §setup; egress consent uses the Hub's explicit
   confirm pattern (a distinct button, not a pre-checked box).
2. **Status card** — same data as the CLI panel.
3. **Run** — question input → progress (loop iteration, spend) → rendered Markdown
   report.

Tauri commands wrap the same mur-core functions used by the CLI (no logic forked into
the GUI crate). i18n keys in both `en.ts` and `zh-TW.ts`; brand rendered as "MUR".

## Safety & error handling

- Egress consent is explicit-only in both surfaces (CLI literal `yes`; Hub explicit
  button). No auto-grant anywhere, including preflight repair.
- Auto-repair is limited to known-safe actions: start workers, re-pin the gateway.
  It never rebuilds the gateway binary and never touches network grants.
- Budget flows into the existing fleet loop budget guard; `mur fleet stop
  deep-research` kill-switch keeps working unchanged.

## Testing

- Preflight/auto-repair decisions extracted as pure functions with unit tests.
- Wizard tested via injected input; non-TTY branch covered.
- Hub components covered by vitest (status card, wizard steps).
- Full convergence remains operator-verified E2E (the fleet loop dials live agent
  sockets and is not automatable — see the deep-research memory/gotchas).
