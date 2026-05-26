# M7b — Skill Gene Model + Recombination Engine (Design)

**Status:** Design draft. Authored 2026-05-26 after M7a shipped (PR #284, merged 2026-05-26). M7a opened the cross-agent observability window; M7b opens the first cross-agent *write* path — but writes only to the invoking agent's own home, preserving M7a's "never modify peer state" invariant.

**Spec mapping:** §M7 cross-agent evolution (gene model + recombination half), §10.1 evolution tracking (extends `EvolutionEvent` with `Recombined`).

**Scoping doc:** `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7-scoping.md` §3 M7b. This design resolves scoping Q3 (gene diff granularity) and Q4 (recombination conflict resolution); defers Q5 (credit) and Q6 (trust inheritance — full model) to M7c.

**Hard dependencies:**
- M5a — `SkillStats` per-agent path helper `path_agent` (shipped #278, extended in M7a).
- M6a — schema validator for the recombined manifest.
- M6c — `skill_llm` helper (used **only** by `--strategy=llm`; Union and Intersection have zero LLM dependency).
- M7a — `list_peer_agents`, `AgentFitness`, per-agent skill load path.

**Soft dependency:** None. Vector-level gene diff is explicitly deferred to M7+.

---

## 1. Goal

M7a let an agent **see** what skills its peers have and how they're performing. M7b lets an agent **breed new skills** from two parents — local-local, local-peer, or peer-peer combinations — and lands the offspring on the invoking agent's home at `Draft` lifecycle. Existing maturity pipeline (M3c evolve, M5b consolidate) then validates the hypothesis through real usage.

What "recombine" means in concrete terms: given two `SkillManifest`s, produce a third by combining their triggers, steps, requirements, and MCP capabilities under a chosen strategy. Three strategies ship:

- **Union** — superset of both parents (all triggers, all steps interleaved).
- **Intersection** — only what both parents share (common triggers; steps that exist in both, picked from the higher-fitness parent).
- **LLM** — delegate the merge decision to `skill_llm` (M6c).

## 2. Non-goals

- Automatic propagation, idle hook, cross-agent push — M7c.
- Credit ledger / reputation — M7c.
- Intent canonicaliser — M7c.
- Trust model extension beyond "inherit lower, land as Sandboxed Draft" — full M7c work.
- Vector or embedding-based gene diff — M7+.
- N-way recombine (3 or more parents) — out of scope; CLI takes exactly two refs.
- Writing to peer agents' state — explicitly preserved invariant from M7a.

## 3. Design decisions

These are decisions made during brainstorming; each is load-bearing.

### 3.1 Recombination scope: same-agent and cross-agent both, but output stays local

The CLI accepts each parent as either `<skill>` (local on invoking agent) or `agent://<peer>/<skill>` (peer read). Three call shapes:

- `recombine local-a local-b` — same agent
- `recombine local-a agent://bob/lookup` — mixed
- `recombine agent://alice/x agent://bob/y` — cross-peer

In all three, the recombined skill is **written only to the invoking agent's home** (`<MUR_HOME>/agents/<self>/skills/<out>/skill.yaml`). Peers are read-only. This:

1. Preserves M7a's safety invariant (M7a explicitly only writes to the invoking agent's `_consolidation/` reports).
2. Gives M7b real cross-agent value without needing the propagation/trust machinery of M7c.
3. Makes recombine reversible: the offspring is a normal `Draft` skill on this agent; deleting it has zero peer impact.

### 3.2 Output destination: new skill, Draft lifecycle, conservative trust

Recombine always produces a **new** skill — never overwrites a parent. Name comes from `--name <out>`, or auto-derives to `<a-base>-x-<b-base>` (e.g., `research-x-lookup`). If the name already exists, the command errors with code 6 and asks for an explicit `--name`. No `--force` in M7b — collisions are user decisions, not silent overwrites.

Lifecycle on entry: **Draft**, regardless of either parent's lifecycle. Recombination is a hypothesis. The existing M3c evolve / M5b consolidate / M5a stats pipeline promotes Draft → Emerging → Stable → Canonical based on actual usage. This is the right shape: M7b proposes; usage disposes.

Trust on entry: **Sandboxed**, regardless of parent trust. Same rule as `agent://` install today (M4b). Trust inheritance across agents is a real M7c question; M7b just doesn't open the box.

### 3.3 Gene representation: field-level, derived

A `SkillGene` is a pure projection of `SkillManifest`:

```rust
pub struct SkillGene {
    pub triggers: BTreeSet<String>,        // pattern strings
    pub steps: Vec<StepGene>,              // ordered; intent is the matching key
    pub requires: BTreeMap<String, String>, // name -> semver constraint string
    pub mcp: BTreeSet<String>,             // capability ids
}
pub struct StepGene {
    pub intent: String,
    pub description: String,
    pub tool: Option<String>,
}
```

Not persisted. Computed via `SkillGene::from_manifest(m: &SkillManifest)` on every call. Manifest remains the source of truth — a sidecar gene file would drift. Caching is a future optimisation if profiling shows it matters; today, parsing a manifest is cheap.

Diff granularity: **field-level set/sequence comparison** with `intent` as the step-matching key. Pure equality on strings; no fuzzy match, no embeddings. This matches the scoping doc Q3 recommendation. Token / vector / embedding diff is M7+ work.

### 3.4 Strategy semantics

**Union** — superset merge:
- `triggers` = set union
- `requires` = key union; on conflict, take the stricter semver via the existing semver `Constraint` parser (shipped in the M3a dependency resolver) — max lower bound, min upper bound; if disjoint, error code 5
- `mcp` = set union
- `steps` = interleave by round-robin: `[A0, B0, A1, B1, A2, ...]`, dropping the shorter side once exhausted
- `intent` strings preserved as-is; duplicate-intent steps allowed in Union (recombination is meant to be permissive)

**Intersection** — overlap-only merge:
- `triggers` = set intersection; if empty, error code 3 ("no overlap; try union")
- `requires` = key intersection; semver merged the same way as Union (stricter)
- `mcp` = set intersection
- `steps` = matched by `intent` string equality; for each matched intent, pick the step from the parent with higher per-agent `success_rate` on that parent skill; tiebreak by `AgentFitness.weight`, then alphabetical agent name
- Unmatched steps (intent in only one parent) are dropped — that's the point of intersection

**LLM** — delegated merge:
- Both manifests serialised to YAML and passed to `skill_llm` (M6c helper) with a fixed prompt template (see §6)
- LLM returns a single YAML manifest
- Result is parsed and **schema-validated** via M6a's validator before persistence
- On invalid YAML / schema failure: error code 5, raw LLM output dumped to stderr for debugging
- On LLM unavailable (no `default` model in registry): error code 4, message points to `mur model add`. **No silent fallback** to Union — predictable failure beats magic.

### 3.5 Fitness tiebreak hierarchy

When Intersection needs to pick between two parents' versions of a shared-intent step:

1. Higher per-agent `SkillStats.success_rate` (where success_rate = success_count / (success_count + failure_count), or 0 if no samples)
2. Tiebreak: higher `AgentFitness.weight` (from M7a, decayed by recency)
3. Final tiebreak: alphabetical agent name (deterministic)

This is fully deterministic — same inputs always produce the same output. Important for tests and reproducibility.

### 3.6 `EvolutionEvent::Recombined` shape

Additive variant on the existing enum:

```rust
pub enum EvolutionEvent {
    // ... existing variants
    Recombined {
        parent_a: SkillRef,
        parent_b: SkillRef,
        strategy: RecombineStrategy,
        agent: String,         // invoking agent
        output_skill: String,  // new skill name on invoking agent
        timestamp: DateTime<Utc>,
    },
}

pub struct SkillRef {
    pub agent: Option<String>, // None = local current agent; Some = peer
    pub skill: String,
    pub version: Option<String>,
}
```

The event is appended to the **invoking agent's** evolution log only. Peer agents have no idea their skill was used as a parent (consistent with read-only access).

## 4. Module structure

```
mur-common/src/skill/
  gene.rs                         # SkillGene, GeneDiff, StepGene — pure data
  evolution.rs                    # add EvolutionEvent::Recombined, SkillRef

mur-core/src/cross_agent/
  recombine/
    mod.rs                        # pub fn run_recombine(opts) -> Result<RecombineOutcome>
    strategy.rs                   # union(g_a, g_b), intersection(g_a, g_b, fitness_ctx)
    llm.rs                        # llm_recombine(m_a, m_b) -> SkillManifest via skill_llm
    peer_ref.rs                   # parse agent://peer/skill; load_skill_ref(home, ref)

mur-core/src/cmd/
  skill_recombine.rs              # CLI dispatcher

mur-core/src/cli/
  skill.rs                        # add Recombine subcommand variant
```

Each file projected ≤ 300 lines. `strategy.rs` is the largest (Union, Intersection, semver-merge helper) and stays in the 250–300 range. All well inside the 800-line ceiling.

## 5. CLI surface

```
mur skill recombine <a> <b> [options]

Args:
  <a>, <b>   skill ref:  <name>  |  agent://<peer>/<name>

Options:
  --strategy={union|intersection|llm}   default: union
  --name <out>                          default: <a-base>-x-<b-base>
  --dry-run                             print recombined manifest YAML to stdout; do not install
  --agent <name>                        invoking agent (default: current agent context from
                                        the runtime; required when invoked outside an agent
                                        process)
  --json                                emit a JSON outcome record instead of human text
```

`--apply` is implicit (default when `--dry-run` is absent). No `--force`.

Examples:
```
mur skill recombine research-prices agent://bob/lookup --strategy=union --name combined-research --dry-run
mur skill recombine local-a local-b --strategy=intersection
mur skill recombine agent://alice/x agent://bob/y --strategy=llm
```

## 6. LLM prompt template (strategy=llm)

```
You are recombining two YAML skill manifests into a single offspring manifest.

Parent A:
```yaml
{{manifest_a_yaml}}
```

Parent B:
```yaml
{{manifest_b_yaml}}
```

Rules:
- Output ONLY a single YAML document; no prose, no code fences.
- Preserve the same top-level shape as the parents.
- Combine triggers and requirements pragmatically; avoid duplicates.
- Steps: produce a coherent ordered sequence that achieves both parents' intents.
- Keep `mcp_requirements` minimal — only capabilities used by your output steps.
- Do not invent new tool names; reuse what either parent uses.

Output:
```

Parsed via the same YAML parser used for manifests. Validation via M6a's `validate_skill_manifest`. Strict — any failure is an error, not a retry-with-recovery.

## 7. Data flow

```
parse CLI args
  → resolve <a>, <b> via peer_ref::load_skill_ref
       (returns SkillManifest + SkillStats; errors if peer/skill missing)
  → compute fitness_ctx = { per-skill success_rate, per-agent AgentFitness.weight }
  → if strategy=union or intersection:
       SkillGene::from_manifest × 2
       → strategy::apply(g_a, g_b, fitness_ctx) -> SkillGene
       → rebuild SkillManifest from SkillGene + name/description/version
  → else if strategy=llm:
       llm_recombine(m_a, m_b) -> SkillManifest
  → validate via M6a schema validator
  → if --dry-run: emit YAML to stdout; exit 0
  → else:
       write <home>/agents/<self>/skills/<out>/skill.yaml (atomic temp+rename)
       write SkillStats::new(<out>, lifecycle=Draft, trust=Sandboxed)
       append EvolutionEvent::Recombined to <self>'s evolution log
       emit summary (human or JSON)
       exit 0
```

## 8. Error handling

| Code | Condition |
|------|-----------|
| 2 | Peer agent not found in `<MUR_HOME>/agents/` |
| 2 | Peer skill not found (lists peer's installed skills in stderr) |
| 3 | Strategy=intersection produced empty triggers or empty steps |
| 4 | Strategy=llm but no default model in registry (points to `mur model add`) |
| 5 | Recombined manifest fails schema validation (Union/Intersection internal bug, or LLM bad output — raw output dumped to stderr) |
| 5 | Union semver merge: disjoint constraints (cannot unify) |
| 6 | Output skill name already exists on invoking agent |

No retries, no fallbacks. Predictable exit codes for scripting.

## 9. Testing

**Unit tests** (alongside each module):

- `gene.rs`: `from_manifest` round-trip; `diff` for added/removed/changed across all four gene categories
- `strategy.rs::union`: 6–8 cases covering trigger union, step interleave, semver merge, mcp union, disjoint semver error
- `strategy.rs::intersection`: 6–8 cases including empty intersection error, fitness tiebreak, alphabetical final tiebreak
- `llm.rs`: with a mocked `skill_llm` (M6c pattern) — happy path, invalid YAML, schema-fail, model-missing

**Integration tests** (`mur-core/tests/skill_recombine.rs`):

1. Same-agent Union → manifest content asserted field-by-field
2. Cross-agent Intersection (synthetic peer fixture) with fitness-based step tiebreak verified
3. `--dry-run` writes nothing to disk
4. `EvolutionEvent::Recombined` appended exactly once with correct fields
5. Name collision yields code 6, manifest unchanged
6. Strategy=llm with unconfigured model yields code 4

**Manual smoke**:
- `mur skill recombine` against real `~/.mur` with two installed skills (Union, dry-run)
- Validate atomic write semantics by inducing a write failure mid-recombine (manifest tempfile cleanup)

## 10. File-size discipline

| File | Projected lines | Notes |
|------|-----------------|-------|
| `gene.rs` | 200 | Data types + `from_manifest` + `diff` |
| `strategy.rs` | 280 | Union + Intersection + semver merge helper |
| `llm.rs` | 150 | Prompt template + skill_llm dispatch + validation |
| `peer_ref.rs` | 120 | Parse `agent://`, load manifest+stats |
| `recombine/mod.rs` | 200 | Orchestration |
| `cmd/skill_recombine.rs` | 180 | CLI dispatch, human and JSON output |
| `cli/skill.rs` | +30 | Additive only |
| `evolution.rs` | +40 | Additive variant |

All well under the 800-line ceiling.

## 11. Open questions

All resolved during brainstorming. The four scoping-doc questions tagged for M7b/M7c:

- **Q3 (gene granularity)** — resolved: field-level only in M7b; vector deferred.
- **Q4 (recombination conflict resolution)** — resolved: per §3.4 (Union interleaves, Intersection fitness-picks, LLM delegates).
- **Q5 (credit model)** — still M7c.
- **Q6 (trust inheritance, full model)** — still M7c; M7b uses the conservative "Sandboxed Draft" rule which is just M4b's behavior extended.

## 12. Carried out of scope

- Automatic propagation, `mur agent propagate`, idle hook — M7c
- Credit ledger `~/.mur/credit/ledger.jsonl`, `mur skill credit` — M7c
- Intent canonicaliser, `~/.mur/intent_canonical.yaml` — M7c
- Cross-agent contradiction detection — deferred (cross-agent contradictions are often legitimate divergence)
- Vector / embedding-based gene diff — M7+
- N-way (≥3) recombine — not on the roadmap; bring back if usage demands
- Writing to peer state under any condition — invariant, not an open question

## 13. Verification checklist

Before declaring M7b complete:

1. `cargo build --workspace` clean
2. `cargo clippy --workspace -- -D warnings` clean
3. `cargo fmt --check` clean
4. `cargo test --workspace` green (includes all new unit + integration tests)
5. Manual smoke against `~/.mur`:
   - Union dry-run between two real local skills
   - Intersection between two skills with overlapping triggers
   - LLM strategy with a configured model (skip if registry empty — covered by integration test code 4)
   - Generated skill appears at Draft lifecycle in `mur skill stats <new-name>`
   - `mur skill consolidate` (M5b) sees the new skill in its scan
6. Cross-agent peer fixture passes: `recombine local-a agent://<peer>/<skill>` writes only to local home, no peer files touched.
