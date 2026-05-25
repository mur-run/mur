# M6 — Skill Ecosystem Scoping Doc

> **This is a scoping doc, not a plan.** It maps M6 surface area into shippable chunks, lists hard dependencies and open questions, and proposes a slice order. Concrete per-task plans (M6a / M6b / …) land afterward.

**Status:** Draft. Authored 2026-05-26 while M5a + M5b implementations are in flight. Schema impact on M5b is the main reason to write this now — if M6 needs new `SkillStats` / manifest fields, it is cheaper to add them before M5b freezes its sidecar v1.

**Spec mapping:**
- §11 (MCP Deep Integration) — currently a "roadmap" section, M6 is when it becomes a milestone.
- §M6 entries in §14: MCP binding, dynamic tool resolution, MCP as execution substrate, propagation graph, registry UI, ratings.
- M5a / M5b deferrals: LanceDB skill vector index, LLM-driven `api-drift`, LLM contradiction adjudication, coverage-gap detection.

---

## 1. Total M6 surface area

Three tracks, originally bundled as one milestone in the spec. They share no code — they can ship in any order, in parallel branches, or skip one entirely.

### Track A — MCP integration (spec §11, §M6 first three bullets)

The headline track. Skills today are procedural text injected at runtime; MCP servers are tools the agent calls. M6 starts wiring the two.

| Sub-feature | Spec ref | Touches |
|---|---|---|
| Skill → MCP capability binding (`mcp_requirements` block) | §11.1 | `mur-common::skill::manifest`, signed manifest schema bump, validator, doctor `tool-availability` check |
| Dynamic tool resolution (`intent` + `tool_hint`) | §11.3 | runtime injector (`mur-core/src/inject/...`), step interpreter, MCP registry lookup |
| MCP as skill execution substrate | §11.2 | `mur-agent-runtime` skill execution path, requires §11.1 + §11.3 |

**Hardest piece:** §11.2. It is a conceptual reframe ("a skill step IS an MCP tool call"), not just plumbing. Needs design before plan.

### Track B — M5 deferrals (lifecycle / consolidation completion)

The "M5 deferred to M6" backlog from M5a §"Out of scope" and M5b §"Out of scope".

| Sub-feature | M5 doc ref | Touches |
|---|---|---|
| LanceDB skill vector index — cosine-similarity dedup | M5b §"Out of scope" #1 | new `mur-core/src/skill_consolidate/dedup_vec.rs`, LanceDB schema for skills, sweep |
| LLM-driven `api-drift` check (currently `Severity::Unknown` stub) | M5a §"Out of scope" #6 | `mur-core/src/skill_doctor/checks/api_drift.rs` (already exists as stub), trace clustering, LLM call |
| LLM contradiction adjudication | M5b §"Out of scope" #2 | `mur-core/src/skill_consolidate/contradiction.rs` (rule-based today), opt-in LLM mode |
| Coverage-gap detection | M5b §"Out of scope" #3 | new check, needs trace failure clustering |

**Common dependency:** A skill-LLM-call abstraction that doctor / consolidate / api-drift can share. Currently `mur-core` does not call out to an LLM for skill maintenance — this is the first such caller.

### Track C — Ecosystem surface (spec §M6 last three bullets)

Public-facing surface: registry UX, social signals.

| Sub-feature | Spec ref | Touches |
|---|---|---|
| Skill propagation graph visualization | §M6 | `mur-server` / dashboard (out of this repo for the front end) |
| Registry web UI | §M6 | `mur-server` / dashboard |
| Skill ratings / usage statistics (server side) | §M6 | `mur-server` API, dashboard, possibly a new `mur skill rate` CLI |

Mostly server / dashboard work. The CLI side is thin (a `mur skill rate <name> <stars>` and a `mur skill stats --remote <name>` that fetches aggregated remote stats). Listed for completeness; not load-bearing on Track A / B.

---

## 2. Dependencies that block planning

Pin these before writing M6a:

| Open question | Why it matters | Who decides |
|---|---|---|
| Does `mcp_requirements` change the signed manifest schema? | If yes → M3-era registries must re-publish. If we add it as optional + skipped-during-sig-check, no break. | M6a plan |
| Is `SkillStats.last_resolved_tools` (or similar) needed so dynamic resolution can be audited / debugged? | If yes → add field in M5b sidecar v1 before freeze, otherwise schema migration in M6. | **Now**, before M5b lands |
| LanceDB skill vector index — replace or augment Jaccard? | "Replace" simplifies code; "augment" preserves M5b behaviour for users without LanceDB built. | M6a (vector dedup sub-plan) |
| LLM call substrate — reuse `mur-core` model registry (`mur model`) or new layer? | Reuse is the obvious answer, but doctor / consolidate need synchronous calls with a small budget; current model registry is async-stream-first. | M6a |
| MCP capability mapping — exact six-capability set from `mur-commander/engine/src/mcp/trust.rs` lines 16-73, or a skill-specific subset? | Need to read that file and decide. Spec §11.1 says mirror, but skills may need fewer capabilities. | M6a |
| Are `intent` / `tool_hint` keys backwards-compatible additions to skill step YAML? | If old skills omit them, runtime must fall back to today's behaviour (literal tool name). | M6a (resolver sub-plan) |

---

## 3. Proposed slice order

Three M6 sub-milestones. **M6a is the only one with a hard scheduling constraint** (the `SkillStats` schema question above).

### M6a — Skill ↔ MCP foundations (Track A.1 + A.3 minimum viable)

Smallest unit that unblocks the conceptual reframe.

Ships:
1. `mcp_requirements` block in skill manifest (optional, behind a manifest schema minor-version bump).
2. Validator + new doctor check `mcp-capability-available` that warns when a declared requirement has no matching MCP server.
3. Capability mapping table (re-export from `mur-commander/engine/src/mcp/trust.rs` if it remains the source of truth, otherwise duplicate with a comment).
4. `SkillStats` schema extension if Track A audit fields are needed (decided at M6a Step 1).

Does NOT ship: dynamic resolution, execution substrate. Those are M6b.

**Branch:** `feat/mur-skill-ecosystem-m6a`. Base: whatever ships M5b.

### M6b — Dynamic tool resolution + MCP execution substrate (Track A.2)

Ships:
1. `intent` + `tool_hint` step keys, parser, runtime resolver.
2. Execution path that calls MCP tools instead of (or alongside) today's procedural text injection.
3. Telemetry — `SkillStepResolved { intent, picked_tool }` event.
4. Backwards-compatibility: skills without `intent` keep today's behaviour.

Hard dep on M6a.

### M6c — LLM-augmented lifecycle (Track B)

Ships:
1. Shared LLM-call helper for `mur-core` maintenance commands.
2. LLM mode for `api-drift` check (M5a stub → real check).
3. LLM adjudication mode for `mur skill consolidate` contradictions.
4. Coverage-gap detection check.
5. **Optional:** LanceDB skill vector index for cosine dedup (could also be M6c-1, see below).

No hard dep on M6a / M6b. Can ship in parallel.

**Sub-question:** LanceDB vector dedup is independently shippable and has no LLM dependency. If you want a quick win between M5b and M6a/b, this is the candidate — call it M6c.1 and ship it as a small standalone PR ahead of M6a if convenient.

### Track C (ecosystem surface)

Not slated to a sub-milestone here — it is mostly `mur-server` work and should be scoped from that repo's side. Listed in §1 for completeness only.

---

## 4. Recommended answers (pending confirmation)

Each item below has a recommended verdict reached by deep-think on 2026-05-26. They are pending only because none has been ratified by writing the corresponding sub-plan yet — but the per-milestone plan can start from these answers, not from open questions.

### 4.1 `SkillStats` schema pre-reservation for M6 — **No, do not pre-reserve.**

- `schema_version` is already in M5a's sidecar. Adding fields later with `#[serde(default)]` is non-breaking — no migration, old files parse fine.
- Pre-reserved fields with no producer create semantic ambiguity (`last_resolved_tools: {}` = "never resolved" or "tracked but empty"?). Worse than absent.
- M6 audit fields are guesses today; wrong guesses become dead schema forever.
- Principle: YAGNI + additive schema evolution. serde defaults make late-add nearly free.

**Action:** M5b PR adds one doc comment in `stats.rs` documenting the additive-only migration pattern for M6+ authors. No schema change.

### 4.2 MCP capability vocabulary — **Mirror commander's six, but via a newtype in `mur-common::skill::mcp`.**

- Spec §11.1 explicitly proposes mirroring; commander's `engine/src/mcp/trust.rs` is the canonical trust-level mapping in production.
- Direct `pub use` propagates every commander change into the signed skill manifest schema — too tight a coupling.
- Newtype (`SkillCapability` with `From<commander::Capability>`) preserves a single vocabulary while giving us a firewall.
- Principle: depend on abstractions, not implementations.

**Action:** M6a Task 1 — `mur-common/src/skill/mcp.rs` defines `SkillCapability` newtype + `From` impl.

### 4.3 `mcp_requirements` required vs optional — **Optional; validator warns (not errors) when a procedural skill omits it.**

- Skills have two modes (spec §3): `context` (markdown injection) and `procedural` (executable steps). Required would force empty declarations on context skills.
- M3-era already-published skills cannot be retroactively invalidated by a schema bump without forcing the entire registry to republish.
- Warning-level finding in `mur skill doctor` is auto-fixable by `--fix --apply` in a future pass.
- Principle: Postel's law — be liberal in what you accept during schema evolution.

**Action:** M6a adds doctor check `mcp-requirements-coverage` at `Severity::Info`.

### 4.4 LanceDB skill vector dedup — **Standalone milestone M6c.1; not bundled into M6c.**

- Zero dependency on LLM substrate (Q5) and zero dependency on MCP work (M6a/b).
- M5b Jaccard stays as-is; vector dedup is opt-in via `--method=vector|jaccard|both`. No regression risk.
- Independent value, independent code path (`skill_consolidate/dedup_vec.rs`), independently shippable.
- Matches the project's existing cadence of small, focused milestones (M3a → M3b.3).

**Action:** Schedule M6c.1 on branch `feat/mur-skill-ecosystem-m6c1-vector-dedup`, can interleave anywhere after M5b.

### 4.5 LLM-call substrate — **New helper `mur-core::skill_llm`, internally consuming `mur model` registry.**

- `mur model` is the discovery/config layer (where to find a model, which key). Adding maintenance-call semantics into it pollutes the config layer.
- Maintenance calls have distinct requirements: one-shot (non-streaming), hard token budget, failure-tolerant (no model → check stays `Unknown`, no panic), cacheable by content hash.
- A dedicated helper is reusable for future maintenance flows (pattern audit, workflow review).
- Principle: separation of concerns — discovery vs calling convention.

**Action:** M6c Task 1 — `mur-core/src/skill_llm/mod.rs`. API skeleton:
```rust
pub async fn maintenance_call(
    prompt: &str,
    model_ref: ModelRef,
    budget: TokenBudget,
) -> Result<Option<String>, SkillLlmError>;
// Ok(None) = model unavailable or budget exhausted — callers degrade gracefully.
```

---

## 5. Updated slice order with recommendations applied

1. **M5b ships first** (already in flight). Includes the one-line doc comment from §4.1.
2. **M6c.1 — LanceDB vector dedup** (optional, can ship any time after M5b; smallest unit, no other deps).
3. **M6a — Skill ↔ MCP foundations** (manifest `mcp_requirements`, `SkillCapability` newtype, doctor `mcp-requirements-coverage` + `mcp-capability-available`).
4. **M6b — Dynamic tool resolution + MCP execution substrate** (hard dep on M6a).
5. **M6c — LLM-augmented lifecycle** (depends on §4.5 helper; independent of M6a/b).

M6c.1 and M6a/b can be done in parallel branches. M6c follows M6c.1 conceptually but does not technically block on it.

---

## 6. Out of scope for M6 entirely

Carried to M7 or beyond — same list as M5b's "M7" deferrals, restated here so M6 planning does not get pulled back into them:

- Cross-agent skill evolution / EvoMap (M7).
- Skill A/B testing framework (deferred per spec §15).
- Paid / private registries, federated registry protocol (deferred per spec §15).
- WASM sandbox for skills (deferred per spec §15).
- Cross-platform sandbox capability matrix (deferred per spec §15).
