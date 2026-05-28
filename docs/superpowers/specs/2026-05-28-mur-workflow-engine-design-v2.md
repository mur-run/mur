# MuR Workflow Engine v2 — Workflow as an Executable Skill

> **Status:** Approved | **Date:** 2026-05-28 | **Supersedes:** `2026-05-28-mur-workflow-engine-design.md` (v1)

## Why v2

v1 proposed building a standalone Workflow Engine: extraction, an evolution state
machine, progressive injection, and team sharing. A codebase audit showed three
problems with that plan:

1. **No execution ledger exists.** `PipelineExecutor` returns an `exit_code` and
   discards it. Nothing persists run history. v1's entire Layer 4 (10 successes →
   Trusted, 3 consecutive failures → Broken, 90 days stale, …) was built on a
   foundation that does not exist and v1 did not design it.
2. **A parallel subsystem already implements ~80% of v1's plan.** The Skill
   subsystem (`mur-common/src/skill/`, 25+ modules) already has classification
   (`Category`, including a `Workflow` variant), 3-layer progressive disclosure,
   agent-runtime dynamic invocation (`trigger_matcher` + `injector`), a
   stats-driven lifecycle state machine, self-evolution (`evolution_log`, gene),
   DSSE signing, a registry, and peer transfer. v1 mentioned none of it.
3. **Steps are linear, with no schema.** `workflow::Step` carries `order: u32`
   and no edges. A Hub DAG editor needs an explicit step graph and a JSON Schema;
   neither exists.

**v2 thesis:** *A workflow is not a new entity. It is a `Skill` with
`category: Workflow` whose `content.procedure` is executable. The Skill subsystem
owns identity, classification, disclosure, evolution, signing, and sharing. The
Workflow engine adds exactly two things on top: (1) a deterministic DAG executor,
and (2) a run-ledger that feeds the skill lifecycle that already exists.*

The Pattern-removal decision from v1 stands (see [Decision: remove Pattern](#decision-remove-pattern)).

## What already exists (audited, do NOT rebuild)

| Capability v1 planned to build | Already in codebase | Location |
|---|---|---|
| Classification / category | `Category { Context, Workflow, Command, Meta }`, `Priority`, `tags` | `skill/types.rs`, `skill/manifest.rs` |
| Progressive disclosure | 3-layer: L2 `abstract` at session start, L3 `procedure` on trigger | `mur-agent-runtime/src/skills/{injector,trigger_matcher}.rs` |
| Agent dynamic invocation | trigger match → layered inject | `mur-agent-runtime/src/skills/` |
| **Evolution state machine** | `next_state(stats, now)` over `LifecycleState { Draft, Emerging, Stable, Canonical, Deprecated, Archived }` with promotion ladder, hysteresis, decay, auto-archive | `skill/lifecycle.rs` |
| Self-evolution | `evolution_log: Vec<EvolutionEvent>`, gene | `skill/{evolution,gene}.rs` |
| Signing / trust | DSSE envelope, `TrustLevel` | `skill/sign.rs`, `skill/manifest.rs` |
| Sharing | registry + `transfer_chain` (peer), team publish/install | `skill/{registry,peers}.rs`, `cmd/workflow.rs` |
| Workflow-level composition | `|` pipe, `&&` seq, `,` parallel | `mur-common/src/pipeline.rs` |
| Deterministic executor | shell exec, exit-code gating, retry, timeout | `mur-core/src/executor/pipeline.rs` |
| Session recording | JSONL append, scrub | `mur-core/src/session/` |

The v1 state-machine map collapses onto the existing one:

| v1 proposed | Existing `LifecycleState` |
|---|---|
| Draft | Draft |
| Active (first success) | Emerging (`PROMOTE_DRAFT_USES = 3` successes) |
| Trusted (10 successes) | Stable (`PROMOTE_EMERGING_USES = 10`, rate ≥ 0.6, age ≥ 7d) |
| Canonical (50 + review) | Canonical (`pinned` + `PROMOTE_STABLE_USES = 30`, rate ≥ 0.8, age ≥ 30d) |
| Broken (3 consecutive failures) | Deprecated (rate < 0.3 w/ usage ≥ 5, or 90d no success) — **add** consecutive-failure fast path |
| Trash | Archived (decay < 0.10 + age > 180d) |

→ Layer 4 is ~90% done. The only missing input is run data.

## What is actually missing (the real work)

1. **Run-ledger** — the only true foundation gap. (§ Layer 4)
2. **Executable + DAG fields on `ProcedureStep`** — `id`, `depends_on`, `command`,
   `on_failure`, `retry`, `timeout_secs`, `needs_approval`. (§ Schema)
3. **JSON Schema generation** for the Hub DAG editor. (§ Schema)
4. **Extraction (LLM judge → produces a `category: Workflow` skill)** — gate +
   extract, with cross-session aggregation and noise filtering. (§ Layer 2)
5. **Unified executor** that runs a `procedure` DAG (command-mode steps
   deterministically; intent-mode steps via the agent). (§ Layer 4)
6. **Migration** of `~/.mur/workflows/*.yaml` → `~/.mur/skills/` and Pattern export.

## Architecture: the converged model

```
Session Recording ──→ Extraction ──→ category:Workflow Skill ──→ Run + Lifecycle
   (existing)          (LLM judge)      (skill = source of truth)   (executor + ledger)
                                              │
                                   ┌──────────┼───────────┐
                                3-layer    DAG editor   evolution
                                inject     (Hub+schema)  (existing)
                                (existing)               (existing)
```

A `category: Workflow` skill whose steps are all command-mode is a classic
runnable workflow. One with intent-mode steps is an agent procedure. Same type,
same lifecycle, same disclosure, same registry. There is one knowledge unit.

### Schema: executable DAG steps

Extend `skill::manifest::ProcedureStep`. All new fields are `Option`/`default`,
so existing manifests parse unchanged.

```rust
// mur-common/src/skill/manifest.rs — ProcedureStep (extended)
pub struct ProcedureStep {
    pub description: String,

    // ── existing agent-mode fields (kept) ──
    pub tool: Option<String>,
    pub intent: Option<String>,
    pub tool_hint: Option<String>,

    // ── NEW: DAG identity ──
    pub id: String,                  // stable kebab id for edges (required; migration auto-assigns s1..sn by file position)
    #[serde(default)]
    pub depends_on: Vec<String>,     // ids that must complete first; empty = root

    // ── NEW: deterministic execution (command-mode) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,     // shell command; presence ⇒ command-mode
    #[serde(default)]
    pub on_failure: FailureAction,   // skip | abort | retry  (reuse workflow::FailureAction)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub needs_approval: bool,
}
```

**Step modes** (validated, mutually exclusive enough to dispatch):
- *command-mode*: `command` is set → deterministic executor runs `sh -c`, gates on
  exit code. Variables interpolate as `{{name}}` (shell-escaped, reuse
  `pipeline::inject_input`).
- *intent-mode*: `command` unset, `intent`/`tool` set → resolved against MCP
  inventory at inject time, executed by the agent (existing behaviour).

**DAG, not a line.** `order: u32` is **removed entirely**. Step ordering is
derived from `depends_on` via topological sort; display order is the topo-sort
result. The executor builds the graph, detects cycles, and runs independent
branches concurrently. This matches GitHub Actions `needs:`, Argo, Airflow,
Temporal. A linear chain is simply `s2.depends_on = [s1]`, `s3.depends_on = [s2]`. This matches GitHub Actions `needs:`, Argo, Airflow, Temporal.
For display/back-compat, a linear chain is just `s2.depends_on = [s1]`, etc.

**JSON Schema.** Derive `schemars::JsonSchema` on `SkillManifest` and emit
`mur skill schema --json` → `schema/skill.schema.json`. The Hub DAG editor uses
it to (a) render a node-per-step / edge-per-dependency graph, (b) generate a
form per step, (c) validate before save. This is the prerequisite the user
identified for DAG editing.

**Signing note.** `ProcedureStep` lives inside `content`, inside the signed
`SkillManifest`. Editing a step re-hashes content and invalidates any publisher
signature — which is correct: extracted and locally-edited skills are unsigned
(they enter at `Sandboxed`/`Local` trust). Re-signing only matters when
re-publishing to a team. The DAG editor must drop the signature on save and mark
the skill unsigned-local.

### Schema: one `Variable` type

Today `workflow::Variable` (typed `VarType` enum, `default_value: Option<String>`)
and `skill::manifest::Variable` (free `var_type: String`,
`default: Option<serde_yaml_ng::Value>`) diverge. Three consumers need the
variable: the shell executor (runtime is a string), the Hub editor (needs a type
to pick a widget + validate), and JSON Schema (type drives validation). A typed
enum serves all three; a free string serves none well. For `default`, the
industry norm (GitHub Actions inputs, Terraform variables, Argo parameters) is
*string-encoded value + type metadata + runtime coercion* — this avoids YAML
scalar surprises (`yes` → bool, `1.0` → float), keeps diffs stable, and matches a
shell executor. Unify on one type in `skill::manifest` (the source of truth):

```rust
// mur-common/src/skill/manifest.rs — Variable (unified)
pub struct Variable {
    pub name: String,
    #[serde(rename = "type", default)]
    pub var_type: VarType,                 // String|Path|Url|Number|Bool|Array (from workflow)
    #[serde(default)]
    pub required: bool,
    #[serde(default, alias = "default_value", skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,           // string-encoded; coerced against var_type at run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,              // enum/select widget + validation; empty = free input
}
```

`VarType` moves from `workflow.rs` into `skill::manifest`. `alias = "default_value"`
lets the migration step read old workflow YAMLs. `serde_yaml_ng::Value` defaults
are dropped. Runtime coercion: `Number`/`Bool` parsed, `Array` decoded as JSON or
comma-separated, others passed through.

### Layer 1: Recording (enrich SessionEvent)

Add execution metadata so extraction has signal. (Unchanged from v1 in intent.)

```rust
// mur-core/src/session/mod.rs — SessionEvent (extended; all Option/default)
pub struct SessionEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub tool: Option<String>,
    pub content: String,
    // ── NEW ──
    pub exit_code: Option<i32>,
    pub working_dir: Option<String>,
    pub git_branch: Option<String>,
}
```

Per-session `detected_vars` is **dropped** from v1: single-session variable
guessing is unreliable. Variables are detected across sessions (see Layer 2).

### Layer 2: Extraction (LLM judge → a `category: Workflow` skill)

**Two manual / automatic entry points, one output type (a draft skill).**

**A. Trigger heuristics → config, not hardcoded.** v1's `≥8 turns`,
`2+ exit 0`, `≥3 tool calls` violate Mandatory Rule #1. Move to
`config.yaml: workflow.extraction.*` with these defaults, all overridable:

```yaml
workflow:
  extraction:
    min_substantive_turns: 8
    completion_exit0_run: 2
    min_tool_steps: 3
```

**B. Noise filter is RETAINED, not removed.** v1 deletes `noise_filter.rs`, but
process-mining practice is unanimous that filtering test runs, exploratory
commands, and failed/aborted operations is the *prerequisite* of extraction, not
optional. Repurpose `capture/noise_filter.rs` to clean the event stream before
the judge sees it. (`exit_code == 0` alone is NOT a completion signal — `ls`/`cat`
spam exits 0; many valuable sessions have no shell at all.)

**C. The judge does ONE thing at a time** (LLM-as-judge best practice:
single-objective, explicit rubric). Split v1's all-in-one call into two:

1. **Gate** (cheap, binary, rubric-scored). "Is this session a repeatable
   procedure?" Rubric checklist, all must hold:
   - steps have a determinate order / dependency
   - commands are parameterizable (not one-off literals)
   - it is not pure exploration (grep/read/ls browsing)
   - the outcome is verifiable (a build, a deploy, a test pass)
   Returns `{repeatable: bool, confidence: f64, reason}`. If false → NOOP.
2. **Extract** (only if gated true). Produce a draft `category: Workflow` skill:
   `name`, `description`, `content.abstract` (Layer 2 text), `content.procedure`
   (steps with `command`/`tool`, inferred `depends_on` from data/order),
   `triggers`, `variables`.

**D. Cross-session aggregation is the real value.** Do not emit one draft per
session. Cluster recordings by step-sequence similarity; when the same procedure
recurs with differing literals (`deploy app_a` / `deploy app_b`), that *diff* is
the strongest variable signal (`{{app_name}}`) and each recurrence raises the
draft's `usage_count`/confidence. This is what turns 5 noisy sessions into 1
high-confidence workflow — and it is where evolution earns its keep.

**Notification** (unchanged from v1):
```
💡 Repeatable workflow detected: "Deploy to Fly.io"
   1. cargo build --release
   2. fly deploy --app {{app_name}}
   3. curl {{health_check_url}}
   [Accept & edit] [Ignore] [Later]
```

### Layer 3: Disclosure & invocation — REUSED, not built

Extracted `category: Workflow` skills automatically get:
- **Layer 2 injection** of `abstract` at session start (`injector.rs`).
- **Layer 3 expansion** of `procedure` on trigger match (`trigger_matcher.rs`).
- **Agent dynamic invocation** through the existing runtime path.

No new injection code. `inject/hook.rs` switches from injecting Patterns to
injecting (a) recommended skills via the existing skill path. This also closes
the v1 gap where the Workflow engine and the agent runtime were two disconnected
worlds.

### Layer 4: Run + Lifecycle

**Unified executor.** `mur run <name>` / `mur workflow run <name>`:
1. Load the `category: Workflow` skill.
2. Build the step DAG from `depends_on`; topologically sort; detect cycles.
3. Execute: command-mode steps run via the existing executor (exit-code gating,
   retry, timeout, `needs_approval` prompt); intent-mode steps require the agent
   runtime — in pure CLI they are **printed as instructions** (the resolved
   intent/tool text) and marked skipped in the ledger, never silently dropped.
4. Independent branches may run concurrently (reuse `pipeline.rs` parallel).
5. Append one record to the run-ledger.

`pipeline.rs` (`|`/`&&`/`,`) stays for **composing multiple skills**; the new DAG
is **within** one skill.

**Run-ledger (THE missing foundation).** Append-only, one file per skill:

```
~/.mur/skills/<name>/runs.jsonl
{"ts": 1730000000, "exit_code": 0, "duration_ms": 8421,
 "failed_step": null, "env_class": "ok", "trigger": "manual"}
```

`env_class` distinguishes *workflow failure* from *environment failure*
(network/credentials/missing-binary classified from stderr), so a flaky network
does not falsely mark a workflow Broken.

**Evolution = feed the ledger into the existing state machine.** A sweep reduces
`runs.jsonl` → `SkillStats` (`usage_count`, `success_count`, `last_success_at`,
`first_successful_use_at`, …) and calls the existing
`skill::lifecycle::next_state(stats, now)`. No new state machine.

Two small additions to `lifecycle.rs`:
- **Broken fast-path**: N consecutive `env_class == "workflow"` failures →
  `Deprecated` immediately + notify user (N in config, default 3). Current rule
  (rate < 0.3 at usage ≥ 5) is too slow for a freshly-broken workflow.
- Confirm `Archived` (decay + age) serves as v1's "Trash"; a separate hard-delete
  sweep removes Archived skills after a configurable grace period.

**Cold-start.** With ~0 real workflows today, absolute-count thresholds (50 runs →
Canonical) are effectively unreachable for a single user. The existing ladder is
already *rate × age × count*, not pure count — keep it, but expose the constants
in config so a solo user can lower them. Bootstrap the first workflows via the
**manual** `mur-in`/`mur-out` path (no LLM variance, immediate output) before
relying on automatic extraction.

## Decision: remove Pattern

Carried from v1, with corrections from the audit:

- **Count is 160 yaml files**, not 42 (`~/.mur/patterns/`), plus `archive/`.
  `injection_count == 0` confirmed across all real patterns (non-zero values exist
  only in test fixtures) — the removal rationale holds.
- **Export before delete.** Dump each pattern to a markdown file under
  `~/.mur/exported-patterns/` before removing, then delete. Never hard-delete user
  data without a backup path.
- Remove: `capture/emergence.rs`, fingerprint extraction, pattern `decay`/
  `maturity` logic in `pattern.rs`, `~/.mur/fingerprints.jsonl`.
- **Keep `capture/noise_filter.rs`** (repurposed for extraction — see Layer 2).
- `inject/hook.rs`, `retrieve/`, `sync.rs`: repoint from Pattern to skill/workflow.

## What we remove / keep / add

**Remove**
- Pattern pipeline: `capture/emergence.rs`, fingerprinting, `evolve/decay.rs`
  half-life guessing, pattern `maturity` in `pattern.rs`.
- Standalone `mur-common/src/workflow.rs` **as a persisted type** — folded into
  `skill` (`category: Workflow` + extended `ProcedureStep`). `FailureAction`,
  `RetryConfig`, `Variable` move/merge into `skill::manifest`.
- `~/.mur/fingerprints.jsonl`, pattern archive (after export).

**Keep / reuse**
- Entire `skill/` subsystem (lifecycle, evolution, gene, signing, registry,
  peers, triggers, mcp_requirements).
- `mur-agent-runtime/src/skills/` 3-layer injector + trigger matcher.
- `mur-common/src/pipeline.rs` composition + `executor/pipeline.rs`.
- `session/` recording + scrub.

**Add**
- `ProcedureStep` DAG/executable fields + JSON Schema emit.
- Run-ledger + stats reducer + Broken fast-path.
- LLM judge (gate + extract) + cross-session aggregation.
- Hub DAG editor.
- Migration: `~/.mur/workflows/` → `~/.mur/skills/`; Pattern export.

## Competitive positioning

Unchanged from v1, with one sharpened claim: the differentiator is **Session →
executable Skill**, and because the executable unit *is* the skill, it inherits
disclosure, evolution, signing, and peer sharing for free. No competitor unifies
extraction, a deterministic DAG executor, and an observable lifecycle on one
shared knowledge object.

## Development phases (re-scoped)

P4 was under-scoped in v1 (no ledger); P5/P6 were over-scoped (skill infra exists).

| Phase | Content | Est. |
|---|---|---|
| **P1: Clean slate** | Export 160 patterns → md; remove pattern pipeline; repoint inject/retrieve/sync; keep noise_filter | 2–3 d |
| **P2: Schema + ledger** | Extend `ProcedureStep` (DAG + exec); JSON Schema emit; SessionEvent enrich; run-ledger + stats reducer | 1.5 wk |
| **P3: Unified executor** | DAG topo-sort + concurrent branches; command/intent dispatch; `needs_approval`; wire run-ledger | 1 wk |
| **P4: Lifecycle wire-up** | Feed stats → `next_state`; Broken fast-path; Archived hard-delete sweep; move ALL thresholds to config (run-driven + existing `lifecycle.rs` `PROMOTE_*`/`DEMOTE_*` constants) | 3–4 d |
| **P5: Extraction** | noise filter prep; LLM judge gate+extract; cross-session aggregation; notification | 1.5 wk |
| **P6: Hub DAG editor** | graph view from schema; per-step form; variable mgmt; test-run in sandbox | 2 wk |
| **P7: Migration + sharing** | `workflows/` → `skills/`; reuse existing registry/transfer for team share | 1 wk |

## Resolved decisions

1. **`order: u32` is removed entirely.** Step ordering derives from `depends_on`
   (topo-sort); display order is the topo-sort result.
2. **Pure-CLI intent-mode steps are printed as instructions** (resolved
   intent/tool text) and marked skipped in the ledger — never silently dropped.
3. **One `Variable` type** in `skill::manifest`: typed `VarType` enum +
   `default: Option<String>` (string-encoded, runtime-coerced) + `choices`.
   `serde(alias = "default_value")` for migration. Skill's free-string /
   `serde_yaml_ng::Value` form is dropped. (See § Schema: one `Variable` type.)
4. **All thresholds go to config in P4** — both the new run-driven ones and the
   existing hardcoded `lifecycle.rs` constants (`PROMOTE_*`, `DEMOTE_*`,
   `AUTO_ARCHIVE_*`), per Mandatory Rule #1.
