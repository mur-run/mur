# M7 — Cross-Agent Evolution Scoping Doc

> **This is a scoping doc, not a plan.** M7 is a major scope jump — the first milestone that spans multiple agents. This doc maps the full surface, decomposes it into shippable sub-milestones, and reclassifies a few items I previously tagged `→ M7` in M5b/M6 plans that don't actually belong here.

**Status:** Draft. Authored 2026-05-26. M5a in PR #278, M5b planned (not started). M6a/M6b/M6c/M6c1 fully planned but none started. No M6 code lands before this scoping doc — we're designing ahead of the build queue.

**Spec mapping:** §M7, §15 federated entries, §10.1 evolution tracking (already shipped in M3c), §7 peer transfer (already shipped in M4a/M4b).

---

## 0. What M7 is really about

M0–M6 are single-agent. One agent installs skills, uses them, evolves them, doctors them. The `agent://` transfer protocol (M4a/M4b) lets agents *explicitly* share skills, but sharing is a manual act: `mur skill install agent://lisa/research-prices`.

M7 makes this **implicit and automated**. Agents running on the same host (and optionally on different hosts) observe each other's skill outcomes and propagate what works. A skill that succeeds 30 times on agent A should be *discoverable* by agent B without a human typing `install agent://`.

The four spec bullets:
1. **Genetic sharing** — skills as genes (steps, triggers, requirements, intents) that can be inherited, mutated, and recombined across agents.
2. **Population-level evolution** — fitness-based propagation. Good skills spread; poor skills are pruned.
3. **Credit/reputation** — who contributed what to a skill's lineage.
4. **Cross-agent benchmarking** — "this skill works on agent A but fails on agent B" is itself a signal.

---

## 1. Inbound deferrals — audit of everything tagged `→ M7`

### 1.1 Actually belongs in M7

| Item | Source | Why it fits |
|---|---|---|
| Cross-agent skill evolution / EvoMap genetic model | Spec §M7, M5b | Core M7 |
| Population-level propagation + pruning | Spec §M7 | Core M7 |
| Credit/reputation incentive system | Spec §M7 | Core M7, depends on cross-agent benchmarking |
| Cross-agent skill performance benchmarking | Spec §M7 | Core M7, feeds credit |
| Cross-agent / federated consolidation | M5b | Two agents may have evolved divergent versions of the same skill; cross-agent dedup/merge needs a population view |
| Cross-agent intent vocabulary harmonisation | M6b | Agents accumulate diverging intent strings. A population-level canonicaliser makes cross-agent transfer coherent. |
| Cross-agent MCP capability propagation | M6a | Agent A discovers a working MCP config → agent B can learn from it without reconfiguring. Depends on a shared vocabulary (M6a `SkillCapability`). |

### 1.2 Reclassified — does NOT belong in M7

| Item | Source | Reclassify to |
|---|---|---|
| **Active MCP dispatch** (mur calls MCP tools, doesn't just prompt-inject) | M6b | **M6d "MCP Runtime"** or its own track. This is a single-agent execution-model change, not cross-agent. |
| **Federated registry protocol** (registries mirror and cross-validate) | Spec §15 | **Registry infra, not evolution.** Separate initiative. M7's cross-agent transfer is node-to-node (agent://), not registry-to-registry. |
| **Cross-pattern × skill dedup** (pattern paraphrases a skill) | M6c.1 | **M6c.2** — small consolidation follow-up. No cross-agent dependency. |
| **Per-skill rank hints** for tiebreaker | M6b | **M6b follow-up** — a config field on `ProcedureStep`. Tiny. |

---

## 2. Core architecture: the gene model

M3c already does single-agent evolution: `mur skill evolve`, `mur skill suggest`, `mur skill generate --from-session`. It mutates one skill at a time, on one agent. M7 scales this to a population.

### 2.1 What is a skill gene?

A gene is a **composable, heritable unit** of a skill. Candidates:

| Gene | Source field | Heritability | Mutability |
|---|---|---|---|
| **Trigger gene** | `triggers[*]` | High — matching triggers is the primary "this skill should fire" signal | Keyword → glob, command → keyword |
| **Step gene** | `content.procedure.steps[*]` | Medium — steps are the operational core | Description rewrite, tool change, intent change |
| **Requirement gene** | `requires[*]` | Low — semver-tied to specific deps | Version bump, dep removal |
| **MCP gene** | `mcp_requirements[*]` | Medium — what capabilities the skill needs | Capability change, tool_pattern refinement |
| **Intent gene** | `ProcedureStep.intent` | Medium — what the step accomplishes | Intent string canonicalisation |

The "genome" of a skill is `(trigger_set, step_sequence, requirement_set, mcp_set, intent_sequence)`. Each dimension can recombine independently.

### 2.2 Mutation operators (extending M3c)

M3c mutates within a single skill. M7 adds cross-agent mutations:

| Operator | Input | Output | Example |
|---|---|---|---|
| **Inherit** | Agent A's skill S | Agent B installs a copy of S | `B` runs `install agent://A/S` (already ships in M4b). M7 makes it automatic. |
| **Recombine** | Two skills S1, S2 with overlapping triggers | New skill S3 mixing steps from both | S1 = "search Google", S2 = "search arXiv" → S3 = "search any engine" with both as fallback steps. |
| **Specialize** | Skill S that works on A but fails on B | B creates a specialized variant that succeeds | A's `browser.navigate` skill uses Chrome; B has Firefox → B mutates tool name to `firefox.navigate`. |
| **Generalize** | Two specialized skills on different agents | Merge into one canonical skill with `tool_hint` that resolves to the right tool per agent | `chrome-search` + `firefox-search` → `web-search` with intent `web_search`. |

### 2.3 Fitness function

M5a's `SkillStats` already tracks per-agent, per-skill: `usage_count`, `success_count`, `failure_count`, `last_used_at`, `lifecycle_state`, `decayed_confidence`. M7 extends this to a **population-aggregated fitness**:

```
population_fitness(skill) =
    Σ(agent_i.success_count * agent_i.weight) /
    Σ(agent_i.usage_count * agent_i.weight)
    × decay_factor(agent_i.last_seen)
```

`agent_i.weight` is derived from the credit/reputation system (§5). `decay_factor` handles agents that go offline.

---

## 3. Proposed sub-milestone decomposition

M7 is too large for one PR. Three sub-milestones, each independently shippable:

### M7a — Cross-agent observability

> **Detailed plan:** `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7a.md` (authored 2026-05-26 after M5b shipped in #279). Three open questions resolved (Q1, Q2, Q7-partial); four deferred to M7b/M7c.

The read-only half (mirrors M5a's relationship to M5b).

**Ships:**
1. `mur agent peers` — discover other agents on the same host (scan `~/.mur/agents/`).
2. `SkillStats` aggregation across peers: `mur skill stats <name> --all-agents` shows population-aggregated usage/success/failure.
3. Cross-agent consolidation: `mur skill consolidate --cross-agent` extends the M5b + M6c.1 dedup/contradiction/orphan passes to operate on `(skill_name, agent_name)` pairs, not just `skill_name`.
4. `AgentFitness` — per-agent weight derived from skill success rates. Pure computation, no mutation.
5. `mur agent card <name>` (already exists from A1) extended with a `fitness` section showing this agent's population standing.

**No mutation.** Just visibility.

**Hard dep:** M5b (consolidate exists), M6c.1 (vector dedup exists to make cross-agent dedup work on embeddings).

### M7b — Skill gene model + recombination engine

**Ships:**
1. `SkillGene` — serializable representation of a skill as a gene vector (triggers, steps, requirements, mcp, intents).
2. Gene diff: `diff(S1, S2) -> GeneDiff` — what genes changed between two versions of a skill (including across agents).
3. Recombination: `recombine(S1, S2, strategy) -> SkillManifest` — merge two skills, picking genes per a strategy.
   - `strategy = Union` — keep all triggers from both; concatenate steps.
   - `strategy = Intersection` — keep only shared triggers; interleave steps.
   - `strategy = LLM` — ask the LLM (via M6c's `skill_llm` helper) what the best merge is.
4. `mur skill recombine <skill-a> <skill-b> --strategy=<s> [--dry-run]` — CLI.
5. `EvolutionLog` extension: `EvolutionEvent::Recombined { parent_a, parent_b, strategy, agent }`.

**No automatic propagation yet.** The operator (or a future M7c automation) decides when to recombine. M7b gives us the tool.

**Hard dep:** M6c (for `skill_llm` helper in LLM strategy).

### M7c — Automatic propagation + credit

The write half.

**Ships:**
1. `mur agent propagate` — one-shot propagation sweep. For each skill this agent owns, check fitness across peers; if fitness exceeds a threshold AND a peer lacks the skill, offer to push via `agent://`.
2. Idle-trigger hook `skill-propagate` wired into C6 (same pattern as M5b's `skill-sweep` idle hook).
3. Skill lineage: `transfer_chain` (already in `SkillManifest`) extended with `mutation_events: Vec<MutationEvent>` — every mutation, recombination, and propagation is recorded.
4. Credit ledger: `~/.mur/credit/ledger.jsonl` — append-only log of contributions. Each entry records `agent_id`, `skill_name`, `contribution_type` (author, mutator, propagator), `timestamp`. No token/currency — just reputation.
5. `mur skill credit <name>` — show the contribution lineage for a skill.
6. Cross-agent intent canonicaliser: given intent strings from N agents, pick the most common form as canonical and emit an `intent_canonical` mapping. Agents reference the mapping at inject time (M6b extension).

**Hard dep:** M7a (observability, fitness), M7b (gene model, recombination). Everything M0–M6.

---

## 4. Open questions (for M7a plan to resolve)

1. **Peer discovery scope** — same-host only, or same-host + configured remotes? M4b already has `agent://` wire dial; extending peer discovery to remote agents is a natural extension but adds auth complexity. Start same-host only.
2. **Fitness weight decay** — how fast does an offline agent's weight decay? Suggestion: half-life 7 days, floor at 0.1× original weight. Tunable via config.
3. **Gene diff granularity** — diff at the field level or the token level? Field-level (trigger set diff, step sequence diff) is cheaper and more interpretable. Token-level (embedding diff) catches paraphrasing. Start field-level; vector diff is M7+.
4. **Recombination conflict resolution** — when both parents have a step at index 2, which one wins? Suggestion: `Union` interleaves (A1, B1, A2, B2); `Intersection` picks the higher-fitness parent's step. `LLM` delegates everything.
5. **Credit without currency** — the spec says "credit/reputation incentive system." Without a token, this is a leaderboard. Is a leaderboard enough? If not, what's the minimal incentive mechanism? Suggestion: agent fitness weight (§2.3) is the incentive — high-reputation agents' skills propagate faster. No token needed for v1.
6. **Cross-agent trust** — M0's trust levels (Sandboxed → Verified → Trusted) are per-agent. When agent B inherits a skill from agent A, does the skill's trust transfer? Suggestion: no — it enters B at Sandboxed, same as any `agent://` transfer today (M4b). The trust model is already correct; M7 just increases the transfer volume.
7. **Intent canonicaliser** — who owns the canonical mapping? Suggestion: per-host, stored in `~/.mur/intent_canonical.yaml`. Agents read on inject; any agent can propose a canonical form; conflict resolution is "most frequent wins." LLM assistance optional (M6c pattern).

---

## 5. Dependency & timeline context

```
M0────────────────────────────────────────────── (shipped)
M1────────────────────────────────────────────── (shipped)
M2────────────────────────────────────────────── (shipped)
M3a/b/c───────────────────────────────────────── (shipped)
M4a/b─────────────────────────────────────────── (shipped)
M5a──────────────────── (PR #278, CI check)
M5b──────────────────── (planned, not started)
M6c.1 ──────────────── (planned, independent)
M6a ────────────────── (planned, depends on M5b)
  └─ M6b ──────────── (planned, depends on M6a)
M6c ────────────────── (planned, independent of M6a/b, benefits from M6c.1)
M7a ────────────────── (depends on M5b + M6c.1)
M7b ────────────────── (depends on M6c + M7a)
M7c ────────────────── (depends on M7a + M7b)
```

M7a is the earliest M7 sub-milestone and depends on M5b + M6c.1. In practical terms, M7 coding is at least 3–4 PRs away.

---

## 6. Reclassified items — explicit handoff

| Item | New home | Rationale |
|---|---|---|
| Active MCP dispatch | **M6d "MCP Runtime"** | Single-agent execution model. Not cross-agent. Deserves its own scoping doc. |
| Federated registry protocol | **Registry infra (future)** | Spec §15. Separate from evolution. Registry team owns it. |
| Cross-pattern × skill dedup | **M6c.2** | Consolidation feature. No cross-agent dependency. Tiny — single new doctor check. |
| Per-skill rank hints | **M6b.1** | A `preferred_tool: Option<String>` field on `ProcedureStep`. One Task, one PR. |

---

## 7. Out of scope for the M7 series

Carried forward from previous scoping docs + spec §15:

- Skill A/B testing framework
- Paid/private skill registries
- WASM sandbox for skills
- Cross-platform sandbox compatibility matrix
- Population-level evolution analytics dashboard (separate from the propagation engine itself)
- Token/currency for credit (leaderboard-only in M7c; currency is a product decision, not an engineering one)
- Remote peer discovery (same-host only in M7; remotes via configured `peers.yaml` in M7+)
