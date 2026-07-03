# Daily-Jobs Skills with Progressive Disclosure — Design

Date: 2026-07-03
Status: Draft (pending user review)

## 1. Problem

MUR's session-start learning index is how external AI tools (Claude Code, Gemini CLI, …)
discover MUR capabilities. An audit of the 16 built-in skills against the 26 daily jobs
documented in `docs/tutorials/mur-daily-jobs-cookbook.html` found four operational domains
with **zero skill coverage** — AI tools cannot discover that these features exist:

1. **Fleet operations** — create/run, job queue, guarded `--loop`, budget, kill-switch,
   autorun safety triad, export/import, commander governance (cookbook jobs 14–22)
2. **Workflow authoring** — flat vs DAG-skill distinction, `procedure` schema, `risk:`
   HITL tiers, `mur channel approve`, `delegate_to` (jobs 8, 10–13)
3. **Agent wiring** — model registry + secret (the StubEcho gotcha), `agent mcp
   add/add-remote/login/registry-add`, cron + idle schedules (jobs 0–1, 6–7)
4. **Parallel execution mechanics** — the command-level mapping for worktree tracks,
   `partition-plan`/`merge`, `merge-concurrent`, `parallel_jobs` config (jobs 23–26).
   `parallel-decompose` teaches topology *judgment*; nothing maps a chosen topology to
   MUR commands.

A second audit found the index itself violates progressive disclosure: every non-Archived
skill is indexed unconditionally (`inject/index.rs:105`), and the two newest skills carry
the entire methodology in the always-paid layers (`parallel-decompose`: 464-char
description + 198-word abstract; `parallel-code`: 224 chars + 151 words). 15/16 bodies
lack a deep-dive pointer.

## 2. Goals / Non-goals

Goals:
- AI tools discover and correctly operate the four uncovered domains.
- A platform-level progressive-disclosure mechanism: skills can be **on-demand only**
  (loadable and searchable, but excluded from the always-injected index) — 12 new skills
  land as 4 indexed hubs + 8 hidden leaves so the index grows by exactly 4 lines.
- Written conventions + a lint so future skills stay disclosure-clean.
- Shrink the two offending skills' L0/L1 without losing content.

Non-goals:
- Converting cookbook jobs into executable `category: workflow` procedures (users harvest
  their own; these are teaching skills).
- Registry publication (follow-on once dogfooded).
- Touching the retrieval/scoring pipeline.

## 3. Platform mechanism: `visibility`

New optional `SkillManifest` field:

```yaml
visibility: indexed      # default; omitted on legacy skills → indexed
# or
visibility: on_demand    # never appears in the session-start index
```

Semantics by surface:

| Surface | `indexed` (default) | `on_demand` |
|---|---|---|
| Session-start learning index (`inject/index.rs`) | listed | **excluded** |
| Runtime agent Layer-2 injection (abstract) | injected when relevant | **excluded** (loadable in-conversation) |
| `mur skill show <name>` | works | works (this is the intended access path) |
| Retrieval (`mur search`, `mur_hook_context`, vector index) | retrievable | **retrievable** — hidden from the menu, not from search |
| `mur skill list` | listed | listed with an `[on-demand]` marker |
| Scope predicates (`scope_visible`), lifecycle, doctor | unchanged | unchanged |

Implementation points:
- `mur-common/src/skill/manifest.rs`: `Visibility` enum (`Indexed` default, `OnDemand`),
  `#[serde(default)]` so every legacy manifest parses unchanged; add to the JSON Schema
  emitted by `mur skill schema`.
- `mur-core/src/inject/index.rs`: extend the existing filter (line ~105) with
  `&& s.manifest.visibility != Visibility::OnDemand`.
- Runtime injector: same predicate at the Layer-2 selection point.
- Compat check during implementation: confirm `SkillManifest` deserialization tolerates
  unknown fields (older binaries reading newer skill.yaml). If it uses
  `deny_unknown_fields`, gate rollout on a release note.

## 4. Progressive-disclosure conventions (all built-in skills)

- **L0 — index line**: `description` ≤ 120 chars. States *when to reach for it*, not how.
- **L1 — abstract**: ≤ 50 words. Scope + one safety caveat + "load body for commands".
- **L2 — body**: sectioned `## <daily job>` headings; ≤ 150 lines per skill; command
  tables (command + when + gotcha), no prose essays. Hubs name their leaves explicitly:
  "deep-dive: `mur skill show <leaf>`".
- **L3 — ground truth**: every body footer points to `mur <cmd> --help` and the cookbook
  URL (`https://app.mur.run/tutorials/mur-daily-jobs-cookbook.html`).

Enforcement: new `mur skill doctor --check disclosure` rule warning on desc > 120 chars or
abstract > 50 words (warning, not error — third-party skills stay valid).

## 5. Skill inventory (4 hubs indexed + 8 on-demand leaves + 1 shared guide)

All `category: context` (except the guide note), `publisher: mur`, keyword triggers,
content distilled from the cookbook's English text.

| Skill | Visibility | Content (sections) |
|---|---|---|
| `mur-fleet-manage` (hub) | indexed | create/show, run + job queue, router planning; points to leaves |
| `mur-fleet-loop` | on_demand | guarded `--loop`, `done_when: marker:`, budget/kill-switch, autorun triad (`MUR_FLEET_AUTORUN` + budget>0 + not stopped), `set-loop` triggers |
| `mur-fleet-share` | on_demand | `export --with-members`/`import` trust flow, `commander pin/directive` (kill/resume/budget-ceiling) |
| `mur-workflow-author` (hub) | indexed | flat vs DAG-skill decision table, `procedure` step schema (`id/depends_on/command/on_failure/retry`), `workflow schedule`; points to leaves |
| `mur-workflow-hitl` | on_demand | `risk:` tiers table, `--channel-new`, finding `hitl_id` in `events.jsonl`, `channel approve/--deny`, SHA-256 pin + 300 s fail-closed, resume-by-run-id |
| `mur-workflow-delegate` | on_demand | `delegate_to` + channel requirement, peer-signed replies, one-agent-N-turns, reading the channel event trail |
| `mur-agent-setup` (hub) | indexed | model registry + secret refs + **StubEcho gotcha**, create for cloud vs local; points to leaves |
| `mur-agent-mcp-wire` | on_demand | `mcp add` (spawn-allowlist sync), `add-remote` + bearer/OAuth `login`, `registry-add`, `inspect --probe` |
| `mur-agent-schedule` | on_demand | cron message injection, `next`, idle triggers, contrast with workflow schedules and fleet autorun |
| `mur-parallel-exec` (hub) | indexed | topology→command map (the cookbook matrix), `parallel_jobs` config (`parallel_jobs.targets` deny-by-default); points to leaves + cross-links `parallel-decompose` |
| `mur-parallel-tracks` | on_demand | `parallel:` block schema, `run --worktree`/`MUR_PARALLEL_EXEC`, compare/judge/cherry, worktree layout + collision guard |
| `mur-parallel-merge` | on_demand | partition mode + `partition-plan`/`merge --promote`, `MUR_PARALLEL_CONCURRENT` + `merge-concurrent --stats/--promote` semantics (never-silent overlaps) |
| `parallel-topology-guide` | on_demand (note) | the methodology text currently living in the parallel-decompose/parallel-code abstracts, moved verbatim |

## 6. Fixes to existing skills

- `parallel-decompose`, `parallel-code`: description → ≤120 chars, abstract → ≤50 words;
  displaced methodology moves to `parallel-topology-guide` (content preserved, layer
  changed); both reference the guide and `mur-parallel-exec`.
- No other built-in skill changes (they pass the audit).

## 7. Testing & rollout

- Unit: `Visibility` serde default + round-trip; `index.rs` excludes on_demand (fixture);
  `skill validate` accepts the field; doctor disclosure-lint fires on a fat fixture.
- Content check: every command line in skill bodies exists in `mur <cmd> --help` output
  (manual pass at review; cookbook already code-sourced).
- Dogfood: fresh Claude Code session → index shows exactly +4 lines; ask a fleet question
  → agent pulls hub then leaf; measure tokens vs today.
- Rollout order: mechanism PR (visibility + index filter + doctor rule) → content PR
  (4 hubs + 7 leaves + guide + 2 slim-downs). Two PRs so the platform change reviews alone.

## 8. Risks

- **Old binaries vs new field** — mitigated by serde default; verified in implementation.
- **Hidden-but-searchable confusion** — `[on-demand]` marker in `skill list`; semantics
  table above is the contract.
- **Drift vs CLI evolution** — bodies are terse command tables; release checklist gains
  "re-run disclosure lint + spot-check hub bodies against --help".
