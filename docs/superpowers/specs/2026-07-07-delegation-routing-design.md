# MUR Delegation Routing — Design Spec

- **Date:** 2026-07-07
- **Status:** Draft (design); grounded in codebase, ready for review → writing-plans
- **Author:** David Chang (with Claude Code)
- **Research basis:** 3 codebase-mapping agents (cost-router/orchestrator spec, parallel-decomposition lens, real delegation primitives) + roster/entitlement inspection of the live `mur` concierge.

## Thesis

When a task arrives, the MUR concierge must decide **who does it and how many** — do it
itself, hand it to one specialist, or fan it out to a squad. Today MUR has *two* routing
frameworks and **neither answers that question**:

- **Cost-router** (`models.yaml` layer) decides *which model tier* runs a sub-task — local
  (cheap) vs frontier (governed external spawn). Orthogonal to who/how-many.
- **Parallel-decomposition lens** (`parallel_decompose.yaml`) decides *parallel vs serial* by
  task topology. Says nothing about *which role* or *single-vs-squad*.

The missing axis — **Scope: self / single specialist / fleet** — is exactly the one users hit
("assign the plan job to `pm`"). This spec models that axis, unifies it with the existing two
into one **soft, layered routing framework**, and — following MUR's `soft-direction-over-hard-
control` principle — encodes it as a governor **skill plus existing primitives**, not a
hard-coded router. It also names the concrete wiring gaps that make delegation unreliable today.

## The three orthogonal axes

| Axis | Question | Existing model | Status |
|------|----------|----------------|--------|
| **A. Scope** | self / single specialist / fleet? | none | ❌ gap (this spec) |
| **B. Topology** | serial / fan-out / best-of-N? | parallel-decomposition lens (4 topologies) | ✓ skill exists |
| **C. Cost/Capability** | local cheap / frontier costly? | cost-router (`RouteDecision::{Local,Escalate}`) | ⚠ decision+ledger only; spawn deferred |

A routing decision is a **point in this 3-axis space**. The concierge picks it softly; hard
gates apply only for safety.

## Current primitives (grounded, with file:line)

| Mechanism | What it is | How the model triggers it today | Status |
|-----------|-----------|--------------------------------|--------|
| **self** | finish in one turn | built-in tools | ✓ |
| **`parallel_jobs`** | ephemeral agent fan-out; all rank-0 `delegate_to` steps, one per job; deny-by-default target allowlist (`executor/jobs.rs:28`, authz `:83`) | **MCP tool, model-callable** (`mur-mcp-server/src/tools.rs:340`) | ✓ only model-callable orchestration tool |
| **`channel/delegate`** | single-target A2A; specialist runs a turn and **signs its own reply** into the channel (`channel_delegate.rs:56`) | **only dialed internally by the DAG executor** (`executor/dag.rs:523`); **not** a model tool | ✓ indirect |
| **`mur fleet run`** | squad on a shared goal; router agent emits a **member-selection DAG** (`fleet/plan.rs:88`), broadcast fallback (`run.rs:62`) | **bash** | ✓ |
| **`mur agent send <name>`** | single-agent send | **bash** | ✓ |
| **frontier spawn** | governed claude/codex/agy subprocess | cost-router | ⛔ deferred (Phase 2, gated on P0b) |

### Why delegation is fragile today (the real constraints)

1. **No model-callable single-delegate tool.** `channel/delegate` is dialed only by the DAG
   executor. To "hand this to `pm`," the concierge must detour through `parallel_jobs` (one
   job), a fleet, a workflow, or bash `mur agent send`.
2. **`mur` is not in the concierge's spawn allowlist** (`processes.spawn.allowed` =
   yt-dlp/deno/ffmpeg/mur-mcp-server). Under strict spawn mode, bash `mur fleet run` /
   `mur agent send` is blocked — so "delegate via bash" is unreliable.
3. **`parallel_jobs` is deny-by-default.** If `config.parallel_jobs.targets` omits pm/qa/…,
   every delegation is rejected before any dial.

**Net:** the concierge's only *reliable* delegation path today is the `parallel_jobs` MCP tool,
and only to pre-authorized targets. Everything else rides unreliable bash. This is the deeper
cause behind "it used to delegate, now it doesn't."

## The decision procedure (soft, layered)

Ordered cheapest-and-most-reversible first. Each rung is model-weighed guidance, not a branch
in code.

```
0. Can I finish this in one turn?                    → do it myself (never delegate trivia)
1. Does it need a specific ROLE's expertise?         → single specialist delegate
     spec/plan → pm · code review → qa · git/PR → repomanager · Rust impl → rustsmith
2. Is it decomposable into independent parallel units? (Axis B lens)
     explore  (read / gather)          → parallel_jobs (ephemeral fan-out, no worktree)
     compete  (best-of-N variants)     → mur fleet run --worktree + judge / cherry
     coupled-write (interdependent)    → parallel-code gate (6 conditions); pass → workflow of
                                          delegate_to steps / partition; else single writer
     coherence-bound (one design)      → single agent
3. Is it a standing / looping shared goal for a squad? → mur fleet run [--loop] (+guards/budget/kill)
4. Is a sub-task too hard for the local model?        → cost-router escalate to frontier (future)
```

### Ambiguity rule (the crux of the best practice)

> **Prefer the cheapest, most reversible option:**
> `self > single delegate > ephemeral parallel_jobs > standing fleet > frontier spawn`.
> On writes, when unsure → single writer. On reads, when unsure → collect, don't reduce.
> (Extends the existing rule in `parallel_decompose.yaml:12`.)

This keeps `fleet` — the heaviest option — reserved for *multiple agents collaborating on one
persistent goal*. A single-role hand-off (brainstorming → `pm`) must **never** escalate to a
fleet.

## Axis B — topology detail (already shipped)

From `parallel_decompose.yaml:14-28`:

- **explore** — independent additive reads → `parallel_jobs` (no worktree).
- **compete** — same goal, best of N heterogeneous attempts → fleet `--worktree` + judge, archive losers.
- **coupled-write** — interdependent edits → `parallel-code` gate (all 6 must hold: disjoint
  files; frozen contract; no sequential dep; mechanical-not-design; ≥3 units worth ~3–15× token
  premium; reviewable as fast as produced) → else single writer.
- **coherence-bound** — one coherent design or a sequential chain → **do not parallelize**, one writer.

Shape rule (`:27`): known decomposition → a saved **Workflow**; emergent → **orchestrator +
parallel_jobs**; not parallel → **single agent**.

## Axis C — cost/capability (partial)

Cost-router (`RouteTier::{Local,Frontier}`, threshold ≥0.55 default / ≥0.75 under PreferLocal;
`DefaultHeuristic` weighting task-type 0.50 + context-size 0.35 + keyword 0.15) decides
local-vs-frontier-**spawn** and writes a per-spawn audit ledger. **Phase 1 = decision + ledger
only; the actual governed spawn (Phase 2) is deferred, gated on the unbuilt P0b agentic loop.**
A2A is explicitly reserved for MUR↔MUR (spec:84), so this axis composes with — never replaces —
fleet/delegate.

## Proposed encoding (phased, minimal-new-code)

**P1 — the governor skill (highest leverage, cheapest).** Author an `orchestration-router`
skill on the concierge encoding §"decision procedure" + the primitive map + the ambiguity rule.
Soft only. This is what actually fixes "agent vs fleet" judgement. Reuses the same
`soft-direction` pattern as the `mur-native-tools` skill.

**P2 — a first-class single-delegate entry.** Either (a) add a `delegate` MCP tool (single
target, reusing `parallel_jobs`' deny-by-default authz), or **(recommended, zero new code)**
canonicalize "single delegate = `parallel_jobs` with exactly one job." Make the skill say so, so
role hand-offs use the one already-authorized, model-callable path.

**P3 — fix the preconditions.** Add pm/qa/repomanager/rustsmith to the concierge's
`parallel_jobs.targets`; restart stale specialists (most of the roster is on a stale runtime);
decide the role of the (currently stopped) dedicated **`orchestrator`** agent — it is the
natural home for Axis-B step 2/3 so the concierge only owns step 0/1.

**P4 — wire Axis C.** When cost-router Phase 2 ships governed spawn, connect step 4.

## Security / governance (hard gates — kept hard on purpose)

Per `soft-direction-over-hard-control`, only these stay hard-coded:

- **Deny-by-default targets** for `parallel_jobs` (and any future `delegate` tool) — `authorize_targets` (`jobs.rs:83`).
- **Fleet budget + kill-switch + iteration/deadline guards**, `MUR_FLEET_AUTORUN` opt-in.
- **HITL risk-tiered gates** on write+ steps.
- **Cost-router audit ledger** for every frontier spawn.

Everything else — which rung, which role, single-vs-squad — is the model's soft call.

## Gaps & open questions

1. **Single-delegate path:** new `delegate` MCP tool vs "1-job `parallel_jobs`"? (Rec: the latter.)
2. **Concierge vs orchestrator division of labour:** should the concierge delegate step-2/3
   orchestration to the standing `orchestrator` agent, or own it? Affects where the router skill lives.
3. **Bash `mur` access:** grant the concierge `mur` in its spawn allowlist, or force everything
   through MCP tools? (MCP-tool path is cleaner — deny-by-default authz already exists.)
4. **Roster health:** delegation needs targets running on a compatible runtime; the current
   fleet of stale specialists will fail proto-gated dials.

## Non-goals

- Not building a hard auto-router that picks scope/role for the model.
- Not changing the cost-router or the parallel-decomposition lens — this spec *unifies and
  extends* them with Axis A.
- Not implementing frontier spawn (that is cost-router Phase 2).

## Appendix — key files

`mur-mcp-server/src/tools.rs:340,712` · `mur-core/src/executor/jobs.rs:28-142` ·
`mur-core/src/cmd/fleet/plan.rs:88-178` · `mur-core/src/cmd/fleet/run.rs:24-74` ·
`mur-core/src/executor/dag.rs:477-540` · `mur-agent-runtime/src/protocol/methods/channel_delegate.rs:56-215` ·
`mur-core/src/skills/parallel_decompose.yaml` · `mur-core/src/skills/mur_parallel_exec.yaml` ·
`docs/superpowers/specs/2026-06-01-cost-router-orchestrator-design.md` ·
`docs/superpowers/specs/2026-06-19-mur-fleet-design.md` ·
`docs/superpowers/specs/2026-06-30-parallel-decomposition-lens-design.md`
