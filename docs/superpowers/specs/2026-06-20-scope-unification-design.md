# Scope Unification Design

**Status:** design approved (brainstorming 2026-06-20); ready for implementation plan.

**Trigger:** team-shared fleets (Phase A) introduces a fifth scope dimension (`Team`),
which is the tipping point making the existing fragmentation costly enough to address.

**Goal:** Converge four overlapping scope mechanisms into one coherent model that covers
the full lifecycle — personal → project → fleet → team → enterprise — with clean seams
for MUR Commander governance and MUR Server team sharing.

**Seam** (used throughout this doc): a field or type that compiles and serializes today
but is only acted on when the corresponding subsystem ships.

---

## 1. Current State: Four Mechanisms, Three Concerns

The codebase has four "scope" types, named similarly but serving different concerns:

### The four types

| Type | File | Concern | Fields |
|---|---|---|---|
| `manifest::SkillScope` | `mur-common/src/skill/manifest.rs` | skill's declared audience | `User\|Project\|Fleet\|Enterprise` |
| `ActiveScope` | `mur-core/src/retrieve/skill_candidates.rs` | runtime injection context | `fleet, project` |
| `ContextScope` | `mur-core/src/context_api/mod.rs` | legacy pattern API filter | `user, project, task, platform` |
| `ScopeContext` | `mur-core/src/retrieve/scoring.rs` | scoring boost hints | `user, platform, task` |

A fifth type, `loader::SkillScope` (`Agent|Global`), describes where a skill lives on disk —
a storage concern, not a visibility concern — and is **not part of this unification**.

### The three concerns

These four types serve three distinct concerns that must stay separate:

1. **Item declaration** — "I am visible to…" (`manifest::SkillScope`)
2. **Runtime context** — "Right now, the active environment is…" (`ActiveScope`, `ContextScope`)
3. **Scoring hints** — "For this query, prefer…" (`ScopeContext`) — **not a scope at all**

### Root cause

Two problems compound:

**Naming confusion:** `ScopeContext` and `ContextScope` are near-anagrams with completely
different purposes. `ScopeContext` is a scoring input; `ContextScope` is a visibility filter.
This has already caused a naming collision (discovered when implementing fleet scope).

**Parallel pipelines:** Skills and patterns each have their own runtime-context type
(`ActiveScope` vs `ContextScope`). As long as both pipelines coexist this is acceptable,
but they will fully overlap once patterns are removed (W3b+). `ContextScope` also buries
a query hint (`task`) inside a visibility type.

### Pattern filter semantics (important for W3b)

`apply_scope_filter` runs three distinct passes with different semantics:

- `project` → **hard filter** against `pattern.applies.projects` list
- `user` / `platform` → **soft filter** against `pattern.origin.*`; patterns without
  origin always pass through (universal patterns)
- `task` → **not a filter at all** — passed downstream to `ScopeContext` for scoring boosts only

The soft/hard distinction disappears when patterns are removed (W3b). Skills use a pure
hard filter: `scope_visible` returns `false` → skill does not inject, no exceptions.

---

## 2. Target Model

### Concern 1 — Item declaration: `SkillScope` (Phase A) → `ScopeTarget` (W3b rename)

Phase A adds `Team` to the existing `SkillScope` enum. The rename to `ScopeTarget` happens
at W3b when the type moves to `mur-common/src/scope.rs`. All Phase A code continues to use
the name `SkillScope`.

```rust
// Phase A: mur-common/src/skill/manifest.rs
pub enum SkillScope {
    #[default]
    User,         // personal, always visible
    Project,      // git repo, auto-detected from cwd
    Fleet,        // AI agent squad, membership-verified
    Team,         // human org/seats, MUR Server auth-backed   ← NEW in Phase A
    Enterprise,   // always visible, admin bypass
}

// W3b: moved to mur-common/src/scope.rs and renamed
pub enum ScopeTarget { /* same variants */ }
```

`SkillManifest` gains two new fields:

```rust
pub struct SkillManifest {
    // ... existing fields (scope, fleet, project) unchanged ...
    pub team: Option<String>,               // team_id; required when scope == Team
    pub governance: Option<GovernanceRef>,  // seam for Commander; ignored until it ships
}
```

### `GovernanceRef` — identification only, no policy

`GovernanceRef` carries IDENTIFICATION metadata so Commander can load the applicable
constitution. The policy (which skills must inject, which are blocked, what gets audited)
lives in the constitution document, not in the skill manifest. Skills are immutable once
signed — Commander must not modify their manifests to stamp policy.

```rust
// mur-common/src/skill/manifest.rs
// ponytail: GovernanceRef is a seam. Current code: serde-only, never read.
//           Commander reads org_id + constitution_hash to derive applicable policy.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernanceRef {
    pub org_id: String,            // identifies the governing Commander instance
    pub constitution_hash: String, // pinned version; Commander detects drift
}
```

The `GovernancePolicy` enum is intentionally omitted. Policy belongs in the constitution:
- `MustInject` ≡ an Enterprise-scoped skill pushed by Commander — no new field needed
- `MustNotInject` ≡ a blocklist entry in the constitution; modifying the skill is wrong
  (breaks bundle signatures and reverses authorship semantics)
- `AuditOnExecute` ≡ a constitution rule, not a self-declared skill property

### `Enterprise` semantics

Unchanged in Phase A: unconditional bypass, always injects. This models "admin-pushed,
always active." When Commander ships: `scope: Enterprise + governance: Some(...)` = a
governed enterprise skill; bare `Enterprise` (no governance field) = local admin bypass.
The enum value survives without ambiguity because the governance field carries the distinction.

### Concern 2 — Runtime context: `ActiveScope` (Phase A) → `ActiveContext` (W3b rename)

Phase A adds `team` to `ActiveScope`. The rename to `ActiveContext` happens at W3b
alongside adding `platform`.

```rust
// Phase A: mur-core/src/retrieve/skill_candidates.rs
pub struct ActiveScope {
    pub fleet:   Option<String>,
    pub project: Option<String>,
    pub team:    Option<String>,   // ← NEW in Phase A
}

// W3b: renamed + extended
pub struct ActiveContext {
    pub fleet:    Option<String>,
    pub project:  Option<String>,
    pub team:     Option<String>,
    pub platform: Option<String>,  // absorbs pattern.applies.tools after W3b
}
```

#### `team` population — env-only in `detect()`, fleet runner fills it

`ActiveScope::detect()` is called on every hook injection and must stay cheap (pure env reads):

```rust
impl ActiveScope {
    pub fn detect() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        Self {
            fleet:   nonempty("MUR_ACTIVE_FLEET"),
            project: mur_common::project::active_project_id(),
            team:    nonempty("MUR_ACTIVE_TEAM"),  // ← NEW
        }
    }
}
```

**Who sets `MUR_ACTIVE_TEAM`?** The fleet runner, before executing member turns:

```
mur fleet run <name>  →  load fleet.yaml  →  if fleet.team_id.is_some():
    set MUR_ACTIVE_TEAM=<team_id> in the turn environment
  →  fan out to member agents (who inherit the env)
```

`detect()` never does a disk read. The fleet runner owns the lookup (once per run).
Daemon injection (future MUR Server auth path): daemon sets `MUR_ACTIVE_TEAM` from the
active server session, same pattern.

**Fail-closed:** `MUR_ACTIVE_TEAM` absent → `team: None` → `scope: Team` skills do not
inject. Matches existing fleet/project behavior.

### `scope_visible` — add `team` dimension

```rust
// mur-common/src/skill/manifest.rs
pub fn scope_visible(
    scope:          SkillScope,
    skill_fleet:    Option<&str>,
    skill_project:  Option<&str>,
    skill_team:     Option<&str>,    // ← NEW
    active_fleet:   Option<&str>,
    active_project: Option<&str>,
    active_team:    Option<&str>,    // ← NEW
) -> bool {
    match scope {
        SkillScope::User       => true,
        SkillScope::Enterprise => true,
        SkillScope::Project => skill_project.is_some() && active_project == skill_project,
        SkillScope::Fleet   => skill_fleet.is_some()   && active_fleet   == skill_fleet,
        SkillScope::Team    => skill_team.is_some()    && active_team    == skill_team,
    }
}
```

The `.is_some()` guard on each selector field is essential for fail-closed behavior:
`None == None` would be `true` (matching everywhere), so the guard prevents a skill
with a missing selector from accidentally becoming universal.

At W3b this function is superseded when the types move to `mur-common/src/scope.rs`;
the 7-parameter signature will be replaced with `(manifest: &SkillManifest, ctx: &ActiveContext)`.

### Concern 3 — Scoring hints: `ScoringHints` (rename of `ScopeContext`)

Pure rename in Phase A, zero semantic change:

```rust
// mur-core/src/retrieve/scoring.rs
pub struct ScoringHints {  // was: ScopeContext
    pub user: Option<String>,
    pub platform: Option<String>,
    pub task: Option<String>,
}
```

`ContextScope.task` is a scoring hint masquerading as a scope field. At W3b it disappears
with `ContextScope`; callers that need it use `ScoringHints` directly.

---

## 3. Fleet Changes

`Fleet` struct gains `team_id` to carry team affiliation:

```rust
// mur-common/src/fleet.rs
pub struct Fleet {
    // ... existing fields ...
    pub team_id: Option<String>,  // ← NEW
}
```

`team_id` is set in two cases:
1. `mur fleet create --team <team-id>` — user explicitly affiliates the fleet at creation
2. `mur fleet import <file.fleet>` — the bundle's `fleet.yaml` carries `team_id`; preserved on install

The fleet runner reads `fleet.team_id` and sets `MUR_ACTIVE_TEAM` before each member turn
(see §2 `team` population above).

---

## 4. Migration Path

Two natural execution windows. No big-bang rewrite.

### Phase A (this PR): minimum viable scope for team-sharing

**`mur-common` changes:**
- Add `Team` variant to `SkillScope` enum
- Add `team: Option<String>` to `SkillManifest` (serde: `#[serde(default, skip_serializing_if = "Option::is_none")]`)
- Add `GovernanceRef` struct with `org_id` + `constitution_hash` (serde: same attributes on `governance` field)
- Add `team_id: Option<String>` to `Fleet` struct

**`mur-core` changes:**
- Add `team: Option<String>` to `ActiveScope`
- Update `ActiveScope::detect()` to read `MUR_ACTIVE_TEAM`
- Update `scope_visible` signature + `Team` match arm
- Update `filter_by_scope` to thread `team` from `ActiveScope` into `scope_visible`
- Rename `ScopeContext` → `ScoringHints` (pure rename; compiler catches all callers)
- Add `ponytail:` comment to `ContextScope` (see §Seam marker below)
- Update fleet runner (`cmd/fleet/loop_run.rs` or equivalent) to set `MUR_ACTIVE_TEAM` from `fleet.team_id`
- Update `mur skill scope` CLI to accept `--team <team-id>`

**Not touched:** `ContextScope`, `apply_scope_filter`, pattern pipeline. Zero risk to pattern tests.

### Seam marker on `ContextScope`

```rust
// ponytail: ContextScope + apply_scope_filter are transitional (pattern pipeline only).
// DELETE at W3b when patterns are removed; replace callers with ActiveContext.
```

### W3b (pattern removal): convergence

When patterns are removed:

1. Delete `ContextScope` and `apply_scope_filter` (context_api module shrinks)
2. Rename `ActiveScope` → `ActiveContext`; add `platform: Option<String>`
3. Rename `SkillScope` → `ScopeTarget`
4. Create `mur-common/src/scope.rs` as canonical home; move `ScopeTarget`, `ActiveContext`, `scope_visible` there
5. Replace 7-parameter `scope_visible` with `(manifest: &SkillManifest, ctx: &ActiveContext)` signature
6. Add `platform: Option<String>` to `SkillManifest` (absorbs `pattern.applies.tools`)

`ScoringHints` (already renamed in Phase A) stays in `mur-core/src/retrieve/scoring.rs` —
it's not a scope type, so it doesn't move to `scope.rs`.

---

## 5. Commander Seam

Commander (closed crate, future) reads `governance: Option<GovernanceRef>` on `SkillManifest`
to identify which constitution governs a skill. Current code ignores this field completely
via `#[serde(default, skip_serializing_if = "Option::is_none")]` — zero cost until Commander ships.

Commander's own crate derives policy from the constitution:
- Which skills must always be active (Commander deploys them at `Enterprise` scope instead
  of using `MustInject` — `Enterprise` already models this)
- Which skills are blocked (maintained in constitution's blocklist, not in the skill manifest)
- Which skill executions need audit events (constitution rule, not a manifest flag)

Commander never modifies an installed skill's manifest. Governance policy is an overlay,
not an edit.

---

## 6. Platform Dimension (deferred to W3b)

`SkillManifest` does not yet have a `platform` filter (corresponding to `pattern.applies.tools`).
A Claude Code skill should not inject into Gemini CLI sessions. Adding `platform: Option<String>`
is deferred to W3b so it lands atomically with `ActiveContext.platform` and the scope.rs
consolidation — not as a half-complete precursor.

---

## 7. What NOT to Do in Phase A

- Do **not** touch `ContextScope`, `apply_scope_filter`, or `apply_scope_filter` tests — pattern pipeline still lives
- Do **not** rename `ActiveScope` → `ActiveContext` yet — rename belongs with W3b field addition
- Do **not** move types to `mur-common/src/scope.rs` yet — belongs with W3b cleanup
- Do **not** add `platform` to `SkillManifest` yet — deferred to W3b
- Do **not** add `GovernancePolicy` enum — policy belongs in the constitution, not the manifest

---

## 8. Alternatives Considered

**Full unification now (rejected):** Creating `ActiveContext` and deprecating `ContextScope`
in Phase A has too large a blast radius while the pattern pipeline is live. `ContextScope`
has active tests; its soft/hard filter distinction differs from the skill model's pure hard
filter. Forced convergence before pattern removal would encode those differences into the
new type, eliminating the benefit of waiting for W3b.

**Minimal A only, no rename (rejected):** Leaving `ScopeContext` / `ContextScope` without
clarifying comments means every future contributor re-discovers the confusion. The rename of
`ScopeContext` → `ScoringHints` is a pure compiler-guided rename that eliminates a genuine
footgun; the `ponytail:` comment on `ContextScope` makes the W3b deletion mechanical.

**`GovernancePolicy` on manifest (rejected):** `MustNotInject` has inverted authorship
semantics (a skill cannot meaningfully declare itself blocked). `MustInject` is redundant with
`Enterprise` scope. `AuditOnExecute` is a constitution rule that Commander overlays, not a
self-declared property. Putting policy on the manifest would also require Commander to modify
signed bundles to change policy, breaking integrity guarantees.

---

## Summary

| Phase | Key changes | Estimated Δ |
|---|---|---|
| **Phase A** | +`Team` to `SkillScope`; +`team` to `ActiveScope`+`SkillManifest`; +`team_id` to `Fleet`; `GovernanceRef` seam; `ScopeContext`→`ScoringHints` rename; fleet runner sets `MUR_ACTIVE_TEAM`; `ponytail:` comment | ~100 lines |
| **W3b** | Delete `ContextScope`+`apply_scope_filter`; rename `ActiveScope`→`ActiveContext`+`platform`; rename `SkillScope`→`ScopeTarget`; consolidate to `mur-common/src/scope.rs`; simplify `scope_visible` signature | ~150 lines Δ |
| **Commander** | Read existing `governance` field; load constitution from `org_id`+`constitution_hash`; no schema changes needed | closed crate |
