# Scope Unification Design

**Status:** design approved (brainstorming 2026-06-20); ready for implementation plan.

**Trigger:** team-shared fleets (Phase A) introduces a fifth scope dimension (`Team`),
which is the tipping point making the existing fragmentation costly enough to address.

**Goal:** Converge four overlapping scope mechanisms into one coherent model that covers
the full lifecycle — personal → project → fleet → team → enterprise — with clean seams
for MUR Commander governance and MUR Server team sharing.

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

### Pattern filter details

`apply_scope_filter` (context_api) runs three distinct filter passes:
- `project` → **hard filter** against `pattern.applies.projects` list
- `user` / `platform` → **soft filter** against `pattern.origin.*` (patterns without origin pass through)
- `task` → **not a filter** — only used as a `ScopeContext` scoring hint downstream

This means `ContextScope.task` does not belong in the type at all. It was included for
convenience but should be a separate parameter.

---

## 2. Target Model

### Concern 1 — Item declaration: `ScopeTarget` (rename of `SkillScope`)

Add `Team` to the existing enum. No other changes to the declaration model.

```rust
// mur-common/src/skill/manifest.rs  (or future mur-common/src/scope.rs)
pub enum ScopeTarget {
    #[default]
    User,         // personal, always visible
    Project,      // git repo, auto-detected from cwd
    Fleet,        // AI agent squad, membership-verified
    Team,         // human org/seats, MUR Server auth-backed   ← NEW
    Enterprise,   // always visible, admin bypass
}
```

`SkillManifest` gains two new fields to carry the Team selector and the Commander seam:

```rust
pub struct SkillManifest {
    // ... existing fields (scope, fleet, project) unchanged ...
    pub team: Option<String>,               // team_id; required when scope == Team
    pub governance: Option<GovernanceRef>,  // seam for Commander; ignored until it ships
}

pub struct GovernanceRef {
    pub org_id: String,
    pub constitution_hash: String,   // pinned version of the governing constitution
    pub policy: GovernancePolicy,
}

pub enum GovernancePolicy {
    MustInject,      // skill must always inject (commander override)
    MustNotInject,   // skill is blocked (commander override)
    AuditOnExecute,  // emit audit event when skill-triggered tool runs
}
```

**`Enterprise` semantics** stay unchanged for Phase A: unconditional bypass (admin-pushed,
always injects). When Commander ships, `scope: Enterprise + governance: Some(...)` becomes
a *governed* enterprise skill; bare `Enterprise` remains a local admin bypass. The enum
value name survives without ambiguity because the governance field carries the distinction.

### Concern 2 — Runtime context: `ActiveContext` (rename and expansion of `ActiveScope`)

Add `team` field to `ActiveScope`; rename it `ActiveContext` at W3b (see §4).

```rust
// Phase A: still named ActiveScope, add team field
pub struct ActiveScope {
    pub fleet: Option<String>,
    pub project: Option<String>,
    pub team: Option<String>,    // ← NEW: populated from fleet.team_id or daemon auth
}

// W3b: rename to ActiveContext, add platform
pub struct ActiveContext {
    pub fleet:    Option<String>,
    pub project:  Option<String>,
    pub team:     Option<String>,
    pub platform: Option<String>,  // absorbs pattern applies.tools after W3b
}
```

**`team` population sources (Phase A — no server auth required):**
1. The daemon, if it holds a MUR Server session (`MUR_ACTIVE_TEAM` env)
2. The active fleet's `team_id` field — so a bundle-imported fleet propagates its team
   affiliation to the injection context without needing a live server connection

Fail-closed: if both sources are absent, `team` is `None`, and `scope: Team` skills do
not inject. This matches existing fleet/project behavior.

### Concern 3 — Scoring hints: `ScoringHints` (rename of `ScopeContext`)

Pure rename, zero semantic change. Makes clear this type is scoring input, not visibility.

```rust
// mur-core/src/retrieve/scoring.rs
pub struct ScoringHints {  // was ScopeContext
    pub user: Option<String>,
    pub platform: Option<String>,
    pub task: Option<String>,
}
```

`ContextScope.task` (currently unused as a filter) is also a scoring hint masquerading as
scope. At W3b it is removed from `ContextScope`'s successor and passed as `ScoringHints`
instead.

### `scope_visible` update

Add `team` parameter alongside existing `fleet` and `project`:

```rust
pub fn scope_visible(
    scope: ScopeTarget,
    skill_fleet:   Option<&str>,
    skill_project: Option<&str>,
    skill_team:    Option<&str>,   // ← NEW
    active_fleet:  Option<&str>,
    active_project: Option<&str>,
    active_team:   Option<&str>,   // ← NEW
) -> bool {
    match scope {
        ScopeTarget::User       => true,
        ScopeTarget::Enterprise => true,
        ScopeTarget::Project    => skill_project.is_some() && active_project == skill_project,
        ScopeTarget::Fleet      => skill_fleet.is_some() && active_fleet == skill_fleet,
        ScopeTarget::Team       => skill_team.is_some() && active_team == skill_team,
    }
}
```

---

## 3. Fleet Changes

`Fleet` struct gains `team_id` so an imported bundle can carry its team affiliation:

```rust
// mur-common/src/fleet.rs
pub struct Fleet {
    // ... existing fields ...
    pub team_id: Option<String>,   // ← NEW; set by mur fleet import when bundle carries it
}
```

`ActiveScope::detect()` derives `team` from:
1. `MUR_ACTIVE_TEAM` env (daemon-injected)
2. The `fleet.team_id` of the currently-active fleet (if any), loaded from `~/.mur/fleets/`

---

## 4. Migration Path

Phase A and W3b are the two natural execution windows. No big-bang rewrite.

### Phase A (this PR): minimum viable scope for team-sharing

Changes in `mur-common`:
- Add `Team` to `manifest::SkillScope` enum
- Add `team: Option<String>` to `SkillManifest`
- Add `governance: Option<GovernanceRef>` stub to `SkillManifest` (3 lines, `#[serde(default, skip_serializing_if = "Option::is_none")]`)
- Add `GovernanceRef` + `GovernancePolicy` types (unused by current code)
- Add `team_id: Option<String>` to `Fleet`
- Update `scope_visible` for `Team` match

Changes in `mur-core`:
- Add `team: Option<String>` to `ActiveScope`
- Update `ActiveScope::detect()` to populate `team`
- Update `filter_by_scope` to thread `team` into `scope_visible`
- Update `mur skill scope` CLI to accept `--team`

No changes to `ScopeContext`, `ContextScope`, or `apply_scope_filter`. Pattern pipeline
is untouched.

### W3b (pattern removal): convergence

When patterns are removed, `ContextScope` + `apply_scope_filter` become dead code.

1. Delete `ContextScope` and `apply_scope_filter` (context_api module shrinks)
2. Rename `ScopeContext` → `ScoringHints` (pure rename, callers update)
3. Rename `ActiveScope` → `ActiveContext` (pure rename + add `platform: Option<String>`)
4. Add `platform: Option<String>` to `SkillManifest` (absorbs `pattern.applies.tools`)
5. Create `mur-common/src/scope.rs` as the canonical home for `ScopeTarget` + `ActiveContext` + `scope_visible`, re-exporting from `manifest` during transition
6. Rename `manifest::SkillScope` → `ScopeTarget` (at the same time as the module move)

### Phase A seam for W3b

Add a file-level comment to `ContextScope` now to make the W3b deletion mechanical:

```rust
// ponytail: ContextScope + apply_scope_filter are transitional.
// Remove at W3b when pattern pipeline is deleted; replace callers with ActiveContext.
```

---

## 5. Commander Seam

Commander (closed crate, future) interacts with scope via `governance: Option<GovernanceRef>`
on `SkillManifest`. Current code ignores this field via `#[serde(default, skip_serializing_if = "Option::is_none")]`.

Commander reads it to:
- Enforce `MustInject` / `MustNotInject` at install time
- Emit structured audit events when `AuditOnExecute` skills trigger tools

This zero-cost seam means no future flag days — Commander can ship without touching
the skill manifest schema.

---

## 6. Platform Dimension (deferred to W3b)

`SkillManifest` does not yet have a `platform` filter (corresponding to `pattern.applies.tools`).
A Claude Code skill should not inject into Gemini CLI sessions. Adding `platform: Option<String>`
is deferred to W3b (along with `ActiveContext.platform`) so it lands as one atomic change
with the rest of the migration, not as a half-complete precursor.

---

## 7. What NOT to Do in Phase A

- Do **not** touch `ScopeContext`, `ContextScope`, or `apply_scope_filter` — pattern
  pipeline still has tests; the risk/reward is wrong before W3b
- Do **not** move `SkillScope` to `mur-common/src/scope.rs` yet — the rename belongs with
  the W3b cleanup
- Do **not** add `platform` to `SkillManifest` yet — deferred to W3b as described above
- Do **not** wire Commander governance logic — the `GovernanceRef` type is a seam only

---

## 8. Alternatives Considered

**Full unification now (rejected):** Creating `ActiveContext` and deprecating `ContextScope`
in Phase A has too large a blast radius while the pattern pipeline is live. `ContextScope`
has active tests; its filtering semantics (hard vs soft filters, `task` as non-filter) differ
from the skill model. Forced convergence before pattern removal would encode those differences
into the new type, eliminating the benefit of waiting.

**Minimal A only, no rename (rejected):** Leaving `ScopeContext` / `ContextScope` without
clarifying comments means future contributors (and future sessions) re-discover the confusion
from scratch. The `ponytail:` comment on `ContextScope` and the rename of `ScopeContext` →
`ScoringHints` are low-cost and pay off continuously.

---

## Summary

| Phase | Changes | Cost |
|---|---|---|
| **Phase A** | +`Team` to `SkillScope`; +`team` to `ActiveScope` + `SkillManifest`; +`team_id` to `Fleet`; +`governance` stub; rename `ScopeContext`→`ScoringHints`; add `ponytail:` comment to `ContextScope` | ~80 lines |
| **W3b** | Delete `ContextScope`+`apply_scope_filter`; rename `ActiveScope`→`ActiveContext`; move to `mur-common/src/scope.rs`; rename `SkillScope`→`ScopeTarget`; +`platform` | ~150 lines Δ |
| **Commander** | Read existing `governance` field; no schema changes | closed crate |
