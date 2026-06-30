# Parallel Decomposition Lens — Design

**Status:** design (brainstormed + research-grounded). **Next:** writing-plans → Phase 1 implementation.
**Related:** `parallel-tracks` P1–P3, `2026-06-29-parallel-tracks-p3-concurrent-merge-design.md`,
`spike1-overlap-rate.md`, the existing `mur-core/src/skills/parallel_code.yaml`.

## 1. Problem & intent

MUR already has the parallel **mechanism** — Tier 1 fleet worktree execution
(`MUR_PARALLEL_EXEC=1 mur fleet run`), the `parallel_jobs` MCP tool (ephemeral fan-out), and
three write-side reconcilers (speculative judge/cherry, partition, concurrent StructuralMerger).
What's missing is the **judgment**: agents treat parallelism as a per-domain special case
("is this a *coding* task? a *research* task?") instead of a general capability.

**The reframe (this design):** parallelism is a **default decomposition lens** applied to *every*
task by its **topology**, not its semantic domain. The agent should answer *why a task **can** be
parallelized* before *how*. "Find 5 papers on X" should natively become a fan-out of isolated
read jobs reconciled by union — without being handled as a "research" special case; "design a CLI
and its underlying library" should be **refused** naive parallelization and run as a phased
sequential plan with checkpoint handoffs.

**Goal:** a governing judgment layer that classifies any task's topology, routes it to the right
execution shape, and — critically — selects the right **recombination** strategy, gated on the
**read-parallelize / write-serialize** axis.

## 2. The decision: a topological classifier

The literature converges on one criterion: *can the task decompose into independent nodes of a DAG
with no edges between them?* If yes, outputs are **additive** ("embarrassingly parallel") → aggregate.
If subtasks share context or depend on each other → sequential, or **competitive parallelism**
(N attempts at the same task → an evaluator *selects*). This is the same regime split this repo's
Spike-1 established empirically (disjoint ≈ 0% overlap; same-task ≈ 100%, select-not-merge).

Every task classifies into one of four topologies:

| Topology | Output relation | Route | Recombination |
|----------|-----------------|-------|---------------|
| **explore** (read / gather) | additive | fan out freely | collect (union/dedupe) **or** reduce (synthesize) |
| **compete** (best-of-N, same goal) | competitive | fan out N attempts | independent judge → **select** |
| **coupled-write** | conflicting | serialize **unless** disjoint + contract-frozen | escalate overlaps (never silent-merge) |
| **coherence-bound** | dependent / shared-taste | **do not parallelize** | single coherent writer |

The classifier is small — it is this 4-way test plus the existing `parallel-code` gate as the
write branch. It is **not** a large decision tree; it will be refined empirically on real tasks.

## 3. Encoding: a layered stack (skill + workflow + MCP + fleet)

The research answer to "skill vs system-prompt vs MCP vs workflow" is **not "pick one"** — it is a
layered assignment, each layer owning a different concern:

| Layer | Owns | MUR mechanism |
|-------|------|---------------|
| **Governor** | the judgment + recombination-strategy choice | **Skill** — a *new* umbrella skill following the `parallel-code` encoding precedent; its write branch *delegates* to `parallel-code`. Keystone. |
| **Predictable split** | a known decomposition shape | **Workflow** (`mur workflow`) |
| **Unpredictable split** | dynamic decomposition (subtasks emerge) | **orchestrator agent + `parallel_jobs` MCP** |
| **Write isolation + write recombination** | per-track worktrees + merge | **fleet / Tier 1 + the reconcilers** |

**System prompt** is used only by a *dedicated orchestrator agent* (Phase 2) — never bloated into
every agent. The skill is triggered (cheap, contextual); the system prompt is always-on (expensive).
The dominant production pattern is **orchestrator-worker** (a lead agent classifies, delegates to
parallel workers, then recombines); tool/skill *descriptions* are load-bearing — well-named
primitives make the model's native selection act as the classifier.

## 4. The three primitives and their recombination contracts

Recombination — not isolation — is the hard part. Worktrees solve filesystem conflicts trivially;
the value and the risk both live in *combining the results*. So each primitive's **coordinator** is a
first-class design object, not an afterthought.

### 4.1 `explore` (additive / read)
Fan out read-only jobs (via `parallel_jobs`; no worktree needed — reads don't conflict). Two
recombination sub-modes:
- **collect** — union + dedupe the raw items (e.g. "find this function signature across 5 repos →
  the 5 locations"). Cheap, near-zero failure risk.
- **reduce** — synthesize an *answer* from the collected items. This is a real failure point:
  naive concatenation makes the synthesizer under-attend the middle inputs ("Lost in the Middle").
  Mitigation contract: **rank/dedupe before synthesis; prefer iterative *refine* over a single
  concat pass when accuracy matters; aggregate structurally (per-source records), not as one prose
  blob.**

### 4.2 `compete` (competitive / best-of-N)
Fan out N attempts at the *same* goal, then **select** the best — never merge competing rewrites.
Recombination = the existing judge/cherry path. Contract:
- The judge must be **independent and heterogeneous** (different model or a grounded/external check)
  to dodge self-preference and correlated-blind-spot bias.
- **No debate primitive.** Out-of-the-box debate does not reliably beat compute-matched
  single-agent + self-consistency and can *degrade* accuracy via persuasion contagion. Best-of-N
  select is the safer, cheaper design. Diversity comes from **model/approach heterogeneity**, not
  persona prompts.
- Losing variants are archived as worktrees (Tier 1 supports this natively).

### 4.3 coupled-write
Reuse the existing `parallel-code` gate: parallelize only if **disjoint files + contract frozen
read-only before fan-out + no sequential dependency + mechanical + ≥3 units + worth the token
premium**; otherwise single writer with checkpoint handoffs. Recombination = the reconcilers:
disjoint hunks auto-merge, **any overlap escalates** to judge/human (never silent interleave). A
semantic conflict is a *correctness* problem, not a merge problem — this is why P3's CRDT was
shelved (Spike-1).

## 5. Guardrails (research-grounded, encoded in the governor)

- **Counter the dominant failure modes.** Multi-agent failures are ~79% *system-design* (clear-spec
  + coordination) problems that don't exist single-agent. The governor enforces: **specific task
  boundaries** (no vague delegation → no duplicated work), **contract-freeze + explicit ownership**
  (no conflicting implicit decisions — the "Flappy Bird" anti-pattern), and **independent
  verification before marking done**.
- **Share context, not state.** Workers receive shared goals/conventions/contracts (read-only) and
  isolated working files. Sharing is *selective and engineered*, not maximal (maximal sharing pays a
  KV-cache penalty and pollutes context).
- **Economics gate.** Fan-out costs ~4× (single agent) to ~15× (multi-agent) tokens; token usage
  explains ~80% of outcome variance. Parallelize only with **≥3 independent units and a task worth
  the premium**. Inherit the concurrency cap (`fanout_cap`, already in Tier 1) and the budget +
  kill-switch (daemon, already external to agents).
- **No silent truncation / no over-spawn.** Cap the fan-out width; log what was dropped.

## 6. Scope

**Phase 1 (this spec → writing-plans):** the keystone.
- The **governor skill** (`skill.yaml` following `SkillManifest`): the 4-way topology classifier +
  routing + the recombination-strategy choice + the guardrail gates. Triggers on task intake; scope
  User (applies everywhere) — to be confirmed.
- The **recombination contracts** for `explore` (collect/reduce, LiM-aware) and `compete`
  (independent judge), wired to **existing** mechanism: `parallel_jobs` for fan-out, the existing
  judge/cherry for compete, the existing `parallel-code` gate for writes.
- The one genuinely new (and thin) build: the **`explore` aggregator** (collect = union/dedupe;
  reduce = the LiM-aware synthesis contract). Everything else is reuse.

**Phase 2 (separate spec/plan, explicitly out of scope here):**
- Canned **Workflow** templates for predictable splits (e.g. a 3-stage research pipeline:
  breadth → deep-read → synthesize).
- A **dedicated orchestrator agent** (its `sys_prompt.md` = the operating procedure) for
  always-on, unattended decomposition.
- Promoting `explore`/`compete` from skill-routed-over-existing-tools to first-class **named MCP
  primitives** — only after Phase 1 validates the routing on real tasks (don't build the abstraction
  ahead of the judgment).

## 7. Reuse vs new

**Reuse (the mechanism is ~95% present):** `parallel_jobs` MCP, fleet / Tier 1 worktree exec, the
three reconcilers, the `parallel-code` skill, `fanout_cap`, the daemon budget/kill guards, signed
channels.
**New (Phase 1, small):** the governor skill; the thin `explore` collect/reduce aggregator; the
routing glue.

## 8. Testing & validation

Consistent with this repo's empirical ethos (Spike-1, the dogfood):
- **Classifier accuracy** — run the governor over a corpus of representative tasks (search,
  find-content, multi-version design, coupled refactor, coherence-bound design) and check it routes
  each to the right topology (and *refuses* the coherence-bound ones).
- **Recombination quality** — for `explore` reduce, verify the LiM mitigation (ranked/structured
  aggregation beats naive concat); for `compete`, verify the judge is independent.
- **Live dogfood** — run a real fan-out end-to-end (the Tier 1 path already proven this session) and
  measure routing + recombination, not just that it executes.
- Unit-test the pure pieces: the topology classification rule, the `explore` aggregator
  (union/dedupe determinism, structured reduce shape).

## 9. Risks & open questions

- **Classifier calibration** — the 4-way rule is a starting point; real tasks are fuzzy
  (mixed-topology). Resolution: keep it small, refine empirically; when ambiguous, **default to the
  safer/cheaper option** (single writer for writes; collect-not-reduce for reads).
- **Where the governor is loaded** — which agents carry the skill, and its `scope` (User vs Project
  vs Fleet). To pin in the plan.
- **`compete` heterogeneity** — needs ≥2 distinct models/approaches to be worth it; if only one
  model is available, `compete` degrades toward self-consistency (still valid, lower lift).

## References

- Anthropic — *How we built our multi-agent research system* (orchestrator-worker; breadth +90.2%;
  ~15× tokens; 80% variance from token usage): https://www.anthropic.com/engineering/multi-agent-research-system
- Cognition — *Don't Build Multi-Agents* (share context not state; the Flappy Bird anti-pattern):
  https://cognition.com/blog/dont-build-multi-agents
- Cemri et al. — *Why Do Multi-Agent LLM Systems Fail?* / MAST (FC1 41.8% / FC2 36.9% / FC3 21.3%):
  https://arxiv.org/abs/2503.13657
- Liu et al. — *Lost in the Middle* (the reduce-step degradation): https://arxiv.org/abs/2307.03172
- Smit et al. — *Should we be going MAD?* (debate ≈/< self-consistency): https://arxiv.org/abs/2311.17371
- *Stop Overvaluing Multi-Agent Debate* / Heter-MAD (heterogeneity > debate): https://arxiv.org/abs/2502.08788
- *Talk Isn't Always Cheap* (debate degradation via persuasion contagion): https://arxiv.org/abs/2509.05396
- *From Generation to Judgment* (LLM-as-judge biases): https://arxiv.org/abs/2411.16594
- CodeCRDT (semantic conflict 5–10%, 20%→80%; "merge ≠ correctness"): https://arxiv.org/pdf/2510.18893
- LangGraph Send / map-reduce (reducers, supersteps, max_concurrency): https://docs.langchain.com/oss/python/langgraph/use-graph-api
- Internal: `spike1-overlap-rate.md` (live 0% disjoint / 100% same-task), `mur-core/src/skills/parallel_code.yaml`.
