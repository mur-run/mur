# Review: MuR Skill Ecosystem Design — Commander-Side Feedback

**Date**: 2026-05-24
**Reviewer**: mur-commander team (David)
**Target document**: `~/Projects/mur/docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md`
**Status**: Review with concrete change proposals
**Related**:
- `~/Projects/mur-commander/docs/specs/2026-05-24-commander-shared-skills-decision.md` (decision memo: Option C+)
- `~/Projects/mur-commander/docs/specs/2026-05-24-p1-agent-orchestration-foundation.md` (commander's manifest schema)

---

## TL;DR

The skill spec is excellent and largely lands the right design. Four substantive gaps surfaced when we read it alongside mur-commander's P1 agent orchestration spec and the "Coordination Layer" podcast (Ona / Lou Bichard, AI Engineer Podcast, 2026-05). All four affect M0 deliverables, so addressing them before M0 ships is the cheap moment.

| # | Gap | Severity | Where to fix |
|---|---|---|---|
| 1 | Coordination responsibility for skill execution is unstated | **High** — without this, every host re-invents step orchestration | §1 (Motivation) preamble or new §1.5 |
| 2 | No shared `ManifestLoader` between mur and other hosts (commander, future GUI) | **High** — guarantees schema drift within 6 months | §3 + new §3.3 |
| 3 | Token budget for skill injection is too simplistic to defeat Context Rot | Medium — symptoms visible in production, not at small scale | §4.2 |
| 4 | Trace event naming will collide with commander's existing journal | Medium — fixable by renaming early | §10.1 |

Plus two smaller suggestions:

| # | Suggestion | Where |
|---|---|---|
| 5 | Add `hosts:` field to `SkillManifest` to enable cross-host skill use | §2 / §3 |
| 6 | Trust store at `~/.mur/trust/skills.json` should be writable by multiple hosts safely | §2.5 |

Each item has a concrete proposed edit at the bottom.

---

## Gap 1: Coordination Responsibility Is Unstated

### Observation

The skill spec defines **what a skill is** (manifest + procedure) and **what runs it** (procedure executor in `task_runner.rs`), but never says **who is responsible for cross-step coordination** when a skill's procedure contains multiple steps.

Concretely: skill `research-prices` has 5 steps. Step 3 fails. Who decides whether to retry, reroute, or escalate? Whose journal records the failure? When the host restarts mid-execution, who resumes?

The skill spec implicitly assumes the agent runtime handles all of this, but says nothing explicit. This creates two problems:

1. **Commander integration is undefined**. When commander (a non-mur-runtime host) executes a skill via its own plan-and-execute layer (P2 of agent orchestration), it must decide whether to defer coordination to a shared protocol or invent its own. Currently it would have to invent.
2. **Skills cannot be tested in isolation from a runtime**. A skill that assumes the runtime retries on transient errors is silently broken if a host doesn't retry.

### Why this matters now

The "Coordination Layer" podcast (Lou Bichard, Ona CEO, May 2026) argues that **coordination is the missing primitive for agent swarms** — runtime and orchestration are solved problems; coordination is not. He identifies three failure modes:

- **Context Rot** — agents forget early instructions as context fills up.
- **Sycophancy** — agents skip steps to please the user.
- **Determinism** — agents can only approximate SDLC microsteps.

A skill ecosystem that doesn't pin down which layer owns coordination ships these failure modes by default. They're invisible at the skill author's desk and explode in production at multi-step workflow boundaries.

### Proposed addition

Insert a new §1.5 ("Coordination Boundary") between §1 (Motivation) and §2 (Security):

> ### 1.5 Coordination Boundary
>
> A skill describes **what to do**, never **how the host should recover when it fails**. Cross-step coordination — retry policy, microstep journaling, replay-on-restart, deterministic ordering — is the **host's** responsibility, not the skill's.
>
> This boundary is intentional. Skills are portable across hosts (mur agent runtime, mur-commander, future hosts) precisely because they do not encode host-specific recovery logic. Each host implements a coordination protocol that meets a shared contract (see *MuR Coordination Protocol v0*, separate spec).
>
> The contract requires every host to:
>
> 1. Journal each skill load and each procedure step with a stable `skill_id@version` reference.
> 2. Treat skill procedure steps as **microsteps** within the host's larger plan, so they appear in the host's coordination journal alongside non-skill steps.
> 3. Apply the host's failure-recovery policy (retry / reroute / escalate) to skill step failures, classified by the `FailureCategory` taxonomy (Knowledge / Tool / Clarification / Style / Transient — same taxonomy as §8.2).
>
> Skills that need behaviour beyond what the host coordination protocol guarantees (e.g. a skill that must run inside a transaction) MUST declare this in `capabilities_declared` and a host that cannot meet the requirement MUST refuse to load the skill.

This is the single most important addition because it lets commander land its own coordination layer (P2 of agent orchestration) **knowing** the skill spec is not going to compete with it. Without this paragraph, two coordination layers will collide.

---

## Gap 2: No Shared ManifestLoader

### Observation

The skill spec §4.4 says `mur-common/src/skill.rs` gets a `Skill` struct and parser, and §6.2 lists CLI commands that operate on `~/.mur/skills/`. But there is no abstraction for a *host* (other than the mur agent runtime) to enumerate and load skills. Each new host (commander, future `mur-gui`) would re-invent the directory-walk + YAML-parse + trust-check pipeline.

This guarantees schema drift within 6 months: commander adds a field, mur adds a different field, the parsers diverge.

### Why this matters now

Commander's P1 agent orchestration spec already defines a `ManifestLoader` in `crates/engine/src/agent/manifest.rs` for *agent* manifests. It walks four scopes (builtin / skill-pack / marketplace / user) with priority resolution. The decision memo (Option C+) proposes that commander's plugin/skill loading reuse the same trait once it exists in `mur-common`.

If skill spec M0 ships a private `Skill` parser without a `SkillLoader` trait, commander either:

- Wraps mur's `Skill::from_str` in its own loader (lossy, brittle).
- Duplicates the entire parser (drift accumulates).
- Adds a `mur-common::skill::SkillLoader` trait **after the fact**, which is harder because users already have skills installed.

### Proposed addition

Modify §3 (Storage) heading to mention loader; add a new §3.3:

> ### 3.3 Loader API
>
> Skills are loaded via a host-implemented `SkillLoader` trait, defined in `mur-common`:
>
> ```rust
> pub trait SkillLoader {
>     /// Scopes this host knows about, in priority order (lowest first).
>     fn scopes(&self) -> Vec<SkillScope>;
>
>     /// Walk all scopes and return resolved manifests (later scopes override earlier).
>     fn load_all(&self) -> Vec<(SkillScope, SkillManifest)>;
>
>     /// Filter manifests to those compatible with this host id (uses §3.4 `hosts:`).
>     fn filter_for_host(&self, host: HostId) -> Vec<SkillManifest>;
>
>     /// Verify a manifest's signature and content hash (uses §2.4).
>     fn verify(&self, manifest: &SkillManifest) -> Result<TrustVerdict, LoadError>;
> }
>
> pub enum SkillScope {
>     /// Built-in skills shipped with the host binary.
>     Builtin,
>     /// Global skills at `~/.mur/skills/`.
>     Global,
>     /// Host-private skills (e.g. `~/.mur/commander/skills/`).
>     Host(PathBuf),
>     /// Per-agent skills (mur agent runtime only).
>     Agent { agent: String },
>     /// Marketplace install at `~/.mur/marketplace/`.
>     Marketplace,
> }
> ```
>
> mur agent runtime provides `MurAgentSkillLoader`. mur-commander provides `CommanderSkillLoader`. The trait is the **single** integration point for hosts.
>
> Skills cannot bypass the loader — there is no public `Skill::from_str` for arbitrary skill execution. This forces every load to pass trust verification.

The trait is small (4 methods). It can ship in M0 without delaying anything.

---

## Gap 3: Token Budget Is Too Simple to Defeat Context Rot

### Observation

§4.2 says:

```yaml
skills:
  max_skills_in_prompt: 5
  max_tokens: 2000
  priority_order: [global, agent]
```

This is a static budget. It doesn't account for:

- The conversation's existing token usage (a 90%-full context with 5 skills injected pushes useful history out).
- The cost per skill in absolute tokens vs marginal value (a 500-token skill that fires once an hour is dead weight).
- **Skill drift** — over a long conversation, even small per-skill injections accumulate.

"Context Rot" (a term from the podcast) describes the failure mode where, as context fills up, the LLM increasingly ignores earlier instructions including those injected by skills. A naive "always inject these 5 skills" rule is the maximally efficient way to *cause* Context Rot.

### Why this matters now

Commander tested this in production (mur-commander v0.9 with its skill-packs system) and observed measurable instruction-following degradation past ~30 turns when 4+ skills were injected. The fix on the commander side is dynamic budget management — but if the skill spec hardcodes static budgeting in M0, commander cannot follow without forking.

### Proposed addition

Replace §4.2 with:

> ### 4.2 Token Budget
>
> Skill injection is **adaptive** — the budget changes based on conversation state:
>
> ```yaml
> skills:
>   # Static caps (upper bounds)
>   max_skills_in_prompt: 5
>   max_total_tokens: 2000
>   priority_order: [global, agent]
>
>   # Adaptive policy
>   adaptive:
>     # Reduce skill budget proportionally as context fills.
>     # Formula: effective_budget = max_total_tokens * (1 - context_fill_ratio)^decay
>     context_fill_decay: 1.5
>     # Below this threshold, skip skill injection entirely.
>     min_remaining_context_ratio: 0.20
>     # Promote a skill's priority if it fired in the last N turns.
>     recent_fire_boost_turns: 5
> ```
>
> Layer 3 (body) is excluded from this budget — it loads on trigger match, replacing the skill's Layer 2 abstract in context.
>
> Hosts MAY refuse to inject a skill when remaining context is below `min_remaining_context_ratio`, recording a `skill_skip_context_full` event in the journal. This is observable and tunable; hardcoded static budgets are not.

Backward-compatibility: omitting the `adaptive:` section gets the v1 behaviour (static caps only).

---

## Gap 4: Trace Event Naming Will Collide

### Observation

§10.1 proposes these event names:

- `skill_loaded`
- `skill_step_start`
- `skill_step_complete`
- `skill_step_error`
- `skill_complete`

Commander's P1 spec defines journal events with these names:

- `workflow_started`
- `step_started`
- `step_completed`
- `step_failed`
- `workflow_completed`

When a skill executes inside a commander workflow, two parallel event streams describe overlapping reality:

```
workflow_started (commander)
  step_started (commander, step_id="step_001")
    skill_loaded (skill, name="research-prices")
      skill_step_start (skill, step=1)
        ... tool calls ...
      skill_step_complete (skill, step=1)
    skill_complete (skill)
  step_completed (commander, step_id="step_001")
workflow_completed (commander)
```

This is two independent journals describing the same execution. Replay logic has to merge them (or pick one and ignore the other). Tooling has to learn both vocabularies.

### Proposed addition

Use **attribute namespacing on existing event names** rather than parallel event types. Replace §10.1 examples with:

> ### 10.1 Skill Execution Trace
>
> Skill execution emits the **same** journal event types as the host (`step_started`, `step_completed`, `step_failed`, etc.) with skill-specific attributes. Skill steps are microsteps within host steps.
>
> ```jsonl
> {"event":"step_started","plan_id":"...","step_id":"step_001","microstep":"skill.research-prices.1","skill_id":"research-prices@1.1.0","skill_step":1,"tool":"browser.navigate","trust":"verified"}
> {"event":"step_completed","plan_id":"...","step_id":"step_001","microstep":"skill.research-prices.1","duration_ms":1230}
> {"event":"step_started","plan_id":"...","step_id":"step_001","microstep":"skill.research-prices.2","skill_id":"research-prices@1.1.0","skill_step":2,"tool":"browser.fill"}
> {"event":"step_failed","plan_id":"...","step_id":"step_001","microstep":"skill.research-prices.2","skill_id":"research-prices@1.1.0","skill_step":2,"error":"Element not found: #search-input","recovery_action":"retry","retry":1}
> ...
> ```
>
> The `microstep` attribute uses the convention `skill.<skill_name>.<step_index>`. The host's plan_id and step_id remain the primary keys; skill steps are nested microsteps within the host step that loaded them. This produces ONE coherent journal that replay tooling can read end-to-end.
>
> Skill-specific lifecycle events (`skill_loaded`, `skill_skip_context_full`) are emitted as standalone events when they don't correspond to a procedure step.

This collapses the two streams into one without losing any information. Commander already plans to emit `microstep` and `phase` attributes (P1 revision), so the format already exists.

---

## Suggestion 5: Add `hosts:` Field

### Observation

The skill spec assumes one host (mur agent runtime). It does not have a way to declare which hosts may load a given skill. The decision memo (`2026-05-24-commander-shared-skills-decision.md` Option C+) proposes a `hosts: Vec<HostId>` field.

### Proposed addition

Modify the canonical YAML example in §2.2:

```yaml
name: research-prices
version: 1.0.0
publisher: human:david
description: Search and compare product prices across e-commerce sites
category: workflow

# Which hosts may load this skill (omit = [all])
hosts: [mur-agent, mur-commander]

# ... rest as today ...
```

`HostId` enum lives in `mur-common::skill::HostId`:

```rust
pub enum HostId {
    MurAgent,
    MurCommander,
    All,                  // both / any future host
    Custom(String),       // reserved for future hosts
}
```

Default value of `hosts` (empty/missing) is `[all]` for backward compatibility. Hosts call `SkillLoader::filter_for_host(HostId::MurCommander)` to scope to their own list.

---

## Suggestion 6: Shared Trust Store Concurrency

### Observation

§2.5 places `SkillTrustStore` at `~/.mur/trust/skills.json` with 0o600 perms and atomic writes. With multiple hosts (mur agent runtime + commander) running concurrently and both writing trust decisions, file-level locking is needed.

### Proposed addition

Add to §2.5:

> ### 2.5.1 Concurrent-Host Safety
>
> Multiple hosts (mur agent runtime + mur-commander + future hosts) may concurrently write to `~/.mur/trust/skills.json`. The trust store uses:
>
> 1. **File locking** via `fs2::FileExt::lock_exclusive()` before reads-modify-write.
> 2. **Atomic rename** for writes (`write to .tmp`, `fsync`, `rename`).
> 3. **Per-skill entry** records `approved_by_host: Vec<HostId>` so different hosts can hold different opinions about a skill's trust level. Trust is per (skill, host); not global.
>
> Reuse the atomic-write + constant-time-checksum primitives from mur-commander `engine/src/trust/store.rs` to avoid implementing this twice.

---

## Concrete Action Items for the MuR Project

The four high/medium-severity gaps suggest these specific PRs against the skill spec:

| PR | Description | Lines touched in spec |
|---|---|---|
| **1 — Coordination Boundary** | Insert new §1.5 as proposed above | ~30 lines added |
| **2 — Shared Loader** | Insert new §3.3; modify §3 heading | ~50 lines added |
| **3 — Adaptive Budget** | Replace §4.2 body | ~25 lines changed |
| **4 — Event Naming** | Rewrite §10.1 examples; remove `skill_*` event prefixes | ~20 lines changed |
| **5 — Hosts Field** | Add `hosts` to canonical YAML in §2.2; add `HostId` enum to data model | ~15 lines added |
| **6 — Trust Concurrency** | Insert §2.5.1 | ~20 lines added |

Total: ≈ 160 lines of spec changes, all of which can be done in M0 without affecting the milestone schedule.

We are happy to write these PRs if useful — they reflect implementation work commander needs to do regardless. Let us know if you want us to draft any of them.

---

## What Commander Will Do Independently

Regardless of whether mur adopts these proposals, commander's P1 spec has already been updated (`2026-05-24-p1-agent-orchestration-foundation.md` Revision Notes) to include:

- `publisher`, `publisher_signature`, `content_sha256`, `trust_level`, `hosts` fields on `AgentManifest`
- `max_context_tokens`, `determinism` fields on `RuntimeSection`
- `microstep`, `phase` fields on `JournalEvent` step variants
- `FailureCategory` taxonomy expansion to `RecoveryAction`
- A namespace clarification README

If mur adopts Suggestions 5–6 (hosts field + concurrent-host-safe trust store), commander will use the shared `mur-common::skill::SkillLoader` trait from Phase 1 of the migration plan (decision memo §6).

If mur rejects them, commander will still ship the same fields under its own namespace (`mur-commander::skill::*`) and the two systems will continue to diverge — which is exactly the outcome the decision memo argues against.

---

## Closing Note

This review is fundamentally collaborative — every item exists because we want commander to integrate cleanly with mur skills, not because the skill spec is wrong. The single most important thing to add is §1.5 (Coordination Boundary). Everything else is mechanical.

Happy to discuss any of this synchronously. The decision memo (`2026-05-24-commander-shared-skills-decision.md`) is the recommended pre-read.
