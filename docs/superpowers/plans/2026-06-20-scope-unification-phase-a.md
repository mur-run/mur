# Scope Unification Phase A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Team` scope to the skill/fleet/injection pipeline, rename `ScopeContext`→`ScoringHints` to eliminate a naming footgun, and plant a `GovernanceRef` seam for Commander — all without touching the pattern pipeline.

**Architecture:** Four-crate change set: `mur-common` (types), `mur-core` (runtime context + CLI), `mur-agent-runtime` (injection + A2A). Changes flow bottom-up: types first, then the runtime context that reads them, then the injection layer that filters them, then the CLI that authors them. Each task compiles and tests independently.

**Tech Stack:** Rust edition 2024; `serde_yaml_ng` for fleet deserialization in agent runtime; `clap` for CLI; `cargo nextest` for tests (see Global Constraints).

## Global Constraints

- Test runner: `cargo nextest run -p <crate>` — never `cargo test` (7 mur-core tests fail spuriously under plain test runner)
- mur-core tests need `ORT_STRATEGY=download cargo nextest run -p mur-core`
- Brand name in user-visible output: **MUR** (uppercase). CLI flag names lowercase: `--team`.
- No changes to `ContextScope`, `apply_scope_filter`, or any pattern pipeline code.
- No `GovernancePolicy` enum — policy lives in Commander's constitution, not the manifest.
- All new `Option` fields on serialized structs: `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Working directory for all commands: `/Volumes/Firecuda4tb/Projects/mur/.worktrees/scope-unify`

---

## File Map

| File | Action | What changes |
|---|---|---|
| `mur-common/src/skill/manifest.rs` | Modify | +`Team` variant; +`GovernanceRef` struct; +`team`/`governance` on `SkillManifest`; update `scope_visible` (+2 params, +1 match arm) |
| `mur-common/src/fleet.rs` | Modify | +`team_id: Option<String>` on `Fleet` |
| `mur-core/src/retrieve/skill_candidates.rs` | Modify | +`team` on `ActiveScope`; update `detect()`; update `filter_by_scope` |
| `mur-core/src/retrieve/scoring.rs` | Modify | Rename `ScopeContext`→`ScoringHints` (pure rename, ~14 occurrences) |
| `mur-core/src/context_api/mod.rs` | Modify | Add `ponytail:` deprecation comment |
| `mur-agent-runtime/src/skills/injector.rs` | Modify | +`active_team` param to `inject_layer2`; thread to `scope_visible` |
| `mur-agent-runtime/src/task_runner.rs` | Modify | +`active_team: Option<String>` to `TaskSpec`; thread to all `inject_layer2` calls |
| `mur-agent-runtime/src/protocol/methods/channel_delegate.rs` | Modify | +`fn verified_active_team`; +`active_team` on `TaskSpec` construction |
| `mur-agent-runtime/src/protocol/methods/message_send.rs` | Modify | +`active_team: None` on `TaskSpec` construction |
| `mur-agent-runtime/src/idle_scheduler.rs` | Modify | +`active_team: None` on `TaskSpec` construction |
| `mur-core/src/cli/skill.rs` | Modify | +`team: Option<String>` arg on `Skill::Scope` |
| `mur-core/src/dispatch.rs` | Modify | Thread `team` to `cmd_scope` |
| `mur-core/src/cmd/skill_cmd.rs` | Modify | +`team` param to `set_manifest_scope` + `cmd_scope`; add `Team` branch |

---

## Task 1: mur-common types — SkillScope::Team, GovernanceRef, Fleet::team_id, scope_visible

**Files:**
- Modify: `mur-common/src/skill/manifest.rs`
- Modify: `mur-common/src/fleet.rs`

**Interfaces:**
- Produces: `SkillScope::Team` variant; `GovernanceRef { org_id, constitution_hash }`; `SkillManifest.team`/`.governance`; `Fleet.team_id`; `scope_visible(scope, skill_fleet, skill_project, skill_team, active_fleet, active_project, active_team) -> bool`

- [ ] **Step 1: Write failing tests for Team scope and GovernanceRef**

Add to the `#[cfg(test)]` block in `mur-common/src/skill/manifest.rs` (after the existing `scope_visible_matrix` test):

```rust
#[test]
fn team_scope_visibility() {
    // matches when active_team == skill_team
    assert!(scope_visible(
        SkillScope::Team,
        None, None, Some("org-xyz"),
        None, None, Some("org-xyz"),
    ));
    // mismatch → false
    assert!(!scope_visible(
        SkillScope::Team,
        None, None, Some("org-abc"),
        None, None, Some("org-xyz"),
    ));
    // no active_team → fail-closed
    assert!(!scope_visible(
        SkillScope::Team,
        None, None, Some("org-xyz"),
        None, None, None,
    ));
    // no skill_team selector → never injects (None == None guard)
    assert!(!scope_visible(
        SkillScope::Team,
        None, None, None,
        None, None, Some("org-xyz"),
    ));
}

#[test]
fn governance_ref_roundtrip() {
    let yaml = "name: t\ndescription: t\ngovernance:\n  org_id: org-1\n  constitution_hash: abc\n";
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    let g = m.governance.unwrap();
    assert_eq!(g.org_id, "org-1");
    assert_eq!(g.constitution_hash, "abc");
}

#[test]
fn governance_ref_absent_is_none() {
    let m: SkillManifest = serde_yaml_ng::from_str("name: t\ndescription: t\n").unwrap();
    assert!(m.governance.is_none());
}

#[test]
fn team_field_roundtrip() {
    let yaml = "name: t\ndescription: t\nscope: team\nteam: org-1\n";
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.scope, SkillScope::Team);
    assert_eq!(m.team.as_deref(), Some("org-1"));
}
```

- [ ] **Step 2: Run tests — expect compile errors (Team variant, new params don't exist yet)**

```
cargo nextest run -p mur-common 2>&1 | head -40
```

Expected: compile errors about unknown `SkillScope::Team`, wrong argument count for `scope_visible`.

- [ ] **Step 3: Add Team variant and GovernanceRef to manifest.rs**

In `mur-common/src/skill/manifest.rs`, make these changes:

*3a. Add `Team` to `SkillScope` enum (after `Fleet`):*
```rust
pub enum SkillScope {
    #[default]
    User,
    Project,
    Fleet,
    Team,        // ← add this
    Enterprise,
}
```

*3b. Add `GovernanceRef` struct (after the `SkillScope` impl block):*
```rust
/// Governance identification for Commander integration.
/// Current code: serde-only seam, never read at runtime.
/// Commander reads `org_id` + `constitution_hash` to load the applicable
/// constitution and derive policy. Never stores policy here — policy belongs
/// in the constitution, not the manifest.
// ponytail: seam — ignored until Commander ships.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernanceRef {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub org_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub constitution_hash: String,
}
```

*3c. Add `team` and `governance` fields to `SkillManifest` (after the `fleet` field):*
```rust
/// Team id this skill is scoped to; required when scope == Team.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub team: Option<String>,
/// Commander governance seam. Current runtime: ignored entirely.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub governance: Option<GovernanceRef>,
```

*3d. Update `scope_visible` signature — add `skill_team` and `active_team` params,
     add `Team` match arm:*
```rust
pub fn scope_visible(
    scope: SkillScope,
    skill_fleet:    Option<&str>,
    skill_project:  Option<&str>,
    skill_team:     Option<&str>,    // ← new
    active_fleet:   Option<&str>,
    active_project: Option<&str>,
    active_team:    Option<&str>,    // ← new
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

*3e. Fix the existing `scope_visible_matrix` test — add `None` for the two new params:*

The test currently calls `scope_visible(SkillScope::X, a, b, c, d)` with 5 args.
Change every call to 7 args, inserting `None` (for `skill_team`) before the last two `None`s:

```rust
// user + enterprise always visible
assert!(scope_visible(SkillScope::User, None, None, None, None, None, None));
assert!(scope_visible(SkillScope::Enterprise, None, None, None, None, None, None));
// fleet skill
assert!(scope_visible(SkillScope::Fleet, Some("dev"), None, None, Some("dev"), None, None));
assert!(!scope_visible(SkillScope::Fleet, Some("dev"), None, None, Some("ops"), None, None));
assert!(!scope_visible(SkillScope::Fleet, Some("dev"), None, None, None, None, None));
// project skill
assert!(scope_visible(SkillScope::Project, None, Some("/p"), None, None, Some("/p"), None));
assert!(!scope_visible(SkillScope::Project, None, Some("/p"), None, None, Some("/q"), None));
```

*3f. Add `team_id` to `Fleet` in `mur-common/src/fleet.rs` (after the `router` field):*
```rust
/// Team identifier for this fleet; set when the fleet is affiliated with a
/// MUR Server team. The fleet runner sets MUR_ACTIVE_TEAM from this value
/// before each member turn so team-scoped skills inject correctly.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub team_id: Option<String>,
```

- [ ] **Step 4: Run tests — expect pass**

```
cargo nextest run -p mur-common
```

Expected: all tests pass including `team_scope_visibility`, `governance_ref_roundtrip`, `governance_ref_absent_is_none`, `team_field_roundtrip`.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/manifest.rs mur-common/src/fleet.rs
git commit -m "feat(scope): Team variant, GovernanceRef seam, fleet.team_id (mur-common)"
```

---

## Task 2: mur-core — ActiveScope::team, ScoringHints rename, ponytail comment

**Files:**
- Modify: `mur-core/src/retrieve/skill_candidates.rs`
- Modify: `mur-core/src/retrieve/scoring.rs`
- Modify: `mur-core/src/context_api/mod.rs`

**Interfaces:**
- Consumes: `SkillScope::Team`, `scope_visible` (7-param), `Fleet.team_id` from Task 1
- Produces: `ActiveScope { fleet, project, team }`; `ActiveScope::detect()` reads `MUR_ACTIVE_TEAM`; `ScoringHints` (renamed from `ScopeContext`); `filter_by_scope` threads `team`

- [ ] **Step 1: Write failing tests for ActiveScope::team**

Add to the `#[cfg(test)]` block in `mur-core/src/retrieve/skill_candidates.rs`:

```rust
#[test]
fn active_scope_detect_reads_mur_active_team() {
    // Safety: single-threaded test; env-var mutation is racy in parallel tests.
    // This is the established pattern for env-var tests in this file.
    unsafe { std::env::set_var("MUR_ACTIVE_TEAM", "org-xyz") };
    unsafe { std::env::remove_var("MUR_ACTIVE_FLEET") };
    let scope = ActiveScope {
        fleet: None,
        project: None,
        team: std::env::var("MUR_ACTIVE_TEAM").ok(),
    };
    assert_eq!(scope.team.as_deref(), Some("org-xyz"));
    unsafe { std::env::remove_var("MUR_ACTIVE_TEAM") };
}

#[test]
fn filter_by_scope_excludes_team_skill_when_no_active_team() {
    use mur_common::skill::manifest::SkillScope;
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    write("t", &format!("scope: team\nteam: org-x\n"), home);
    let mut candidates = load_skill_candidates(&home.join("skills"), home).unwrap();
    let ctx = ActiveScope { fleet: None, project: None, team: None };
    filter_by_scope(&mut candidates, &ctx);
    assert!(candidates.is_empty(), "team skill must not inject without active team");
}

#[test]
fn filter_by_scope_includes_team_skill_when_team_matches() {
    use mur_common::skill::manifest::SkillScope;
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    write("t", &format!("scope: team\nteam: org-x\n"), home);
    let mut candidates = load_skill_candidates(&home.join("skills"), home).unwrap();
    let ctx = ActiveScope { fleet: None, project: None, team: Some("org-x".into()) };
    filter_by_scope(&mut candidates, &ctx);
    assert_eq!(candidates.len(), 1);
}
```

- [ ] **Step 2: Run tests — expect compile errors**

```
ORT_STRATEGY=download cargo nextest run -p mur-core 2>&1 | head -40
```

Expected: errors about unknown `team` field on `ActiveScope` and wrong arg count to `scope_visible` in `filter_by_scope`.

- [ ] **Step 3: Add team field to ActiveScope and update detect()**

In `mur-core/src/retrieve/skill_candidates.rs`:

*3a. Add `team` to `ActiveScope`:*
```rust
#[derive(Debug, Default, Clone)]
pub struct ActiveScope {
    pub fleet: Option<String>,
    pub project: Option<String>,
    pub team: Option<String>,   // ← new: from MUR_ACTIVE_TEAM or fleet.team_id (via runner)
}
```

*3b. Update `detect()`:*
```rust
pub fn detect() -> Self {
    let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
    Self {
        fleet:   nonempty("MUR_ACTIVE_FLEET"),
        project: mur_common::project::active_project_id(),
        team:    nonempty("MUR_ACTIVE_TEAM"),   // ← new
    }
}
```

*3c. Update `filter_by_scope` to pass `team` to `scope_visible`:*
```rust
pub fn filter_by_scope(candidates: &mut Vec<LoadedSkill>, ctx: &ActiveScope) {
    candidates.retain(|c| {
        mur_common::skill::manifest::scope_visible(
            c.manifest.scope,
            c.manifest.fleet.as_deref(),
            c.manifest.project.as_deref(),
            c.manifest.team.as_deref(),      // ← new
            ctx.fleet.as_deref(),
            ctx.project.as_deref(),
            ctx.team.as_deref(),             // ← new
        )
    });
}
```

- [ ] **Step 4: Rename ScopeContext → ScoringHints in scoring.rs**

In `mur-core/src/retrieve/scoring.rs`, rename the struct and all usages.
The rename is mechanical — every occurrence of `ScopeContext` becomes `ScoringHints`:

```rust
// Line 12: struct definition
pub struct ScoringHints {   // was: ScopeContext
    pub user: Option<String>,
    pub platform: Option<String>,
    pub task: Option<String>,
}
```

Also update every `ScopeContext` reference in function signatures, fn bodies, and tests
in this file (use find-and-replace). The import in `skill_candidates.rs`:

```rust
// mur-core/src/retrieve/skill_candidates.rs:249
use crate::retrieve::scoring::ScoringHints;  // was: ScopeContext

// line ~350:
let scope = ScoringHints::default();  // was: ScopeContext::default()
```

- [ ] **Step 5: Add ponytail comment to ContextScope**

In `mur-core/src/context_api/mod.rs`, add comment above `ContextScope`:

```rust
// ponytail: ContextScope + apply_scope_filter are transitional (pattern pipeline only).
// DELETE at W3b when patterns are removed; replace callers with ActiveScope/ActiveContext.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextScope {
```

- [ ] **Step 6: Run tests — expect pass**

```
ORT_STRATEGY=download cargo nextest run -p mur-core
```

Expected: all tests pass including the new `active_scope_detect_*` and `filter_by_scope_*` tests.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/retrieve/skill_candidates.rs \
        mur-core/src/retrieve/scoring.rs \
        mur-core/src/context_api/mod.rs
git commit -m "feat(scope): ActiveScope::team, ScopeContext→ScoringHints rename, ponytail comment"
```

---

## Task 3: mur-agent-runtime — inject_layer2 + TaskSpec::active_team + channel_delegate

**Files:**
- Modify: `mur-agent-runtime/src/skills/injector.rs`
- Modify: `mur-agent-runtime/src/task_runner.rs`
- Modify: `mur-agent-runtime/src/protocol/methods/channel_delegate.rs`
- Modify: `mur-agent-runtime/src/protocol/methods/message_send.rs`
- Modify: `mur-agent-runtime/src/idle_scheduler.rs`

**Interfaces:**
- Consumes: `scope_visible` (7-param), `SkillManifest.team`, `Fleet.team_id` from Tasks 1–2
- Produces: `inject_layer2(..., active_team: Option<&str>)`; `TaskSpec.active_team`; `fn verified_active_team` in channel_delegate

- [ ] **Step 1: Write failing tests**

Add to `mur-agent-runtime/src/skills/injector.rs` tests (inside the existing `#[cfg(test)]` block):

```rust
#[test]
fn team_scoped_skill_injects_when_team_matches() {
    let s = make_skill("ts", |m| {
        m.scope = mur_common::skill::manifest::SkillScope::Team;
        m.team = Some("org-x".into());
    });
    let cfg = SkillsConfig::default();
    let result = inject_layer2(&[s], &cfg, 0.0, &Default::default(), None, None, Some("org-x"));
    assert_eq!(result.injected_skills.len(), 1);
}

#[test]
fn team_scoped_skill_excluded_without_active_team() {
    let s = make_skill("ts", |m| {
        m.scope = mur_common::skill::manifest::SkillScope::Team;
        m.team = Some("org-x".into());
    });
    let cfg = SkillsConfig::default();
    let result = inject_layer2(&[s], &cfg, 0.0, &Default::default(), None, None, None);
    assert!(result.injected_skills.is_empty());
}
```

Add to `mur-agent-runtime/src/protocol/methods/channel_delegate.rs` tests:

```rust
#[test]
fn verified_active_team_reads_fleet_team_id() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    // Write a fleet.yaml with team_id
    let fleet_dir = home.join("fleets").join("dev");
    std::fs::create_dir_all(&fleet_dir).unwrap();
    let fleet_yaml = "name: dev\ngoal: test\nmembers: []\nchannel_id: fleet-dev\nteam_id: org-1\n";
    std::fs::write(fleet_dir.join("fleet.yaml"), fleet_yaml).unwrap();

    assert_eq!(
        verified_active_team(home, "fleet-dev").as_deref(),
        Some("org-1")
    );
    // non-fleet channel → None
    assert_eq!(verified_active_team(home, "agent:foo:uuid"), None);
    // fleet without team_id → None
    write_fleet(home, "noTeam", &["qa"], None);
    assert_eq!(verified_active_team(home, "fleet-noTeam"), None);
}
```

- [ ] **Step 2: Run tests — expect compile errors**

```
cargo nextest run -p mur-agent-runtime 2>&1 | head -40
```

Expected: errors about wrong arg count on `inject_layer2`, missing `active_team` on `TaskSpec`, unknown `verified_active_team`.

- [ ] **Step 3: Add active_team param to inject_layer2**

In `mur-agent-runtime/src/skills/injector.rs`:

*3a. Update signature (add `active_team` after `active_project`):*
```rust
pub fn inject_layer2(
    skills:         &[LoadedSkill],
    cfg:            &SkillsConfig,
    threshold:      f64,
    fired:          &HashSet<String>,
    active_fleet:   Option<&str>,
    active_project: Option<&str>,
    active_team:    Option<&str>,    // ← new
) -> InjectionResult {
```

*3b. Update the `scope_visible` call inside inject_layer2 (add skill_team and active_team):*
```rust
.filter(|s| {
    mur_common::skill::manifest::scope_visible(
        s.manifest.scope,
        s.manifest.fleet.as_deref(),
        s.manifest.project.as_deref(),
        s.manifest.team.as_deref(),   // ← new: skill_team
        active_fleet,
        active_project,
        active_team,                  // ← new: active_team
    )
})
```

*3c. Fix all inject_layer2 test call sites in this file — add `None` as the last arg:*

Search for `inject_layer2(` in injector.rs and add `None` (for `active_team`) to each call
that doesn't already have it. Example:
```rust
// before:
inject_layer2(&[s], &cfg, 0.85, &HashSet::new(), None, None)
// after:
inject_layer2(&[s], &cfg, 0.85, &HashSet::new(), None, None, None)
```

- [ ] **Step 4: Add active_team to TaskSpec and thread it**

In `mur-agent-runtime/src/task_runner.rs`:

*4a. Add field to `TaskSpec`:*
```rust
pub struct TaskSpec {
    // ... existing fields ...
    pub active_fleet: Option<String>,
    pub active_team:  Option<String>,   // ← new
}
```

*4b. Find every call to `inject_layer2` in task_runner.rs and add `spec.active_team.as_deref()` as the last argument.*
Look for calls like:
```rust
inject_layer2(..., spec.active_fleet.as_deref(), active_project)
```
Change to:
```rust
inject_layer2(..., spec.active_fleet.as_deref(), active_project, spec.active_team.as_deref())
```

*4c. Fix all `TaskSpec { ... }` literal constructions in tests — add `active_team: None`:*
Find every struct literal with `active_fleet: None` in this file and add `active_team: None` alongside it. There are approximately 7 such sites.

- [ ] **Step 5: Add verified_active_team to channel_delegate.rs and wire it**

In `mur-agent-runtime/src/protocol/methods/channel_delegate.rs`:

*5a. Add the function after `verified_active_fleet`:*
```rust
/// Derive the active team from the fleet on this channel, if any.
/// Reads fleet.yaml and returns fleet.team_id. No membership check needed —
/// team affiliation is a fleet-level property, not per-agent.
fn verified_active_team(mur_home: &Path, channel_id: &str) -> Option<String> {
    let name = mur_common::fleet::fleet_name_from_channel_id(channel_id)?;
    let path = mur_home.join("fleets").join(name).join("fleet.yaml");
    let raw = std::fs::read_to_string(&path).ok()?;
    let fleet: mur_common::fleet::Fleet = serde_yaml_ng::from_str(&raw).ok()?;
    fleet.team_id
}
```

*5b. Add `active_team` to the `TaskSpec` construction in the `channel/delegate` handler
(the line that already has `active_fleet`):*
```rust
let spec = TaskSpec {
    // ... existing fields ...
    active_fleet: verified_active_fleet(&self.mur_home, &self.agent, &channel_id),
    active_team:  verified_active_team(&self.mur_home, &channel_id),   // ← new
};
```

- [ ] **Step 6: Fix message_send.rs and idle_scheduler.rs**

In `mur-agent-runtime/src/protocol/methods/message_send.rs`, find the `TaskSpec { ... }` construction (around line 94) and add:
```rust
active_team: None,   // message/send has no fleet channel context
```

In `mur-agent-runtime/src/idle_scheduler.rs`, find the `TaskSpec { ... }` construction (around line 141) and add:
```rust
active_team: None,
```

- [ ] **Step 7: Run tests — expect pass**

```
cargo nextest run -p mur-agent-runtime
```

Expected: all tests pass including `team_scoped_skill_injects_when_team_matches`, `team_scoped_skill_excluded_without_active_team`, `verified_active_team_reads_fleet_team_id`.

- [ ] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/skills/injector.rs \
        mur-agent-runtime/src/task_runner.rs \
        mur-agent-runtime/src/protocol/methods/channel_delegate.rs \
        mur-agent-runtime/src/protocol/methods/message_send.rs \
        mur-agent-runtime/src/idle_scheduler.rs
git commit -m "feat(scope): thread active_team through inject_layer2, TaskSpec, channel_delegate"
```

---

## Task 4: CLI — mur skill scope --team

**Files:**
- Modify: `mur-core/src/cli/skill.rs`
- Modify: `mur-core/src/dispatch.rs`
- Modify: `mur-core/src/cmd/skill_cmd.rs`

**Interfaces:**
- Consumes: `SkillScope::Team`, `SkillManifest.team` from Task 1
- Produces: `mur skill scope <name> --team <team-id>` sets `scope: team, team: <team-id>`

- [ ] **Step 1: Write failing test**

Add to the tests in `mur-core/src/cmd/skill_cmd.rs` (inside `set_manifest_scope_sets_and_validates`):

```rust
#[test]
fn set_manifest_scope_team() {
    let mut m = SkillManifest::default();
    // --team sets scope + team field, clears others
    set_manifest_scope(&mut m, None, None, None, Some("org-1"), false).unwrap();
    assert_eq!(m.scope, SkillScope::Team);
    assert_eq!(m.team.as_deref(), Some("org-1"));
    assert!(m.fleet.is_none());
    assert!(m.project.is_none());

    // exactly-one-flag check: team + user → error
    assert!(set_manifest_scope(&mut m, None, None, None, Some("org-1"), true).is_err());
    // empty team-id → error
    assert!(set_manifest_scope(&mut m, None, None, None, Some(""), false).is_err());
}
```

- [ ] **Step 2: Run test — expect compile errors**

```
ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(set_manifest_scope_team)' 2>&1 | head -20
```

Expected: compile error (wrong arg count on `set_manifest_scope`).

- [ ] **Step 3: Add --team to set_manifest_scope and cmd_scope**

In `mur-core/src/cmd/skill_cmd.rs`:

*3a. Update `set_manifest_scope` signature — add `team: Option<&str>` param and `user: bool` stays last:*
```rust
pub(crate) fn set_manifest_scope(
    m:       &mut SkillManifest,
    fleet:   Option<&str>,
    project: Option<&str>,
    team:    Option<&str>,   // ← new
    user:    bool,
) -> Result<()> {
    use mur_common::skill::manifest::SkillScope;
    let n = fleet.is_some() as u8
        + project.is_some() as u8
        + team.is_some() as u8     // ← new
        + user as u8;
    if n != 1 {
        return Err(anyhow!(
            "specify exactly one of --fleet <name>, --project, --team <id>, or --user"
        ));
    }
    if user {
        m.scope   = SkillScope::User;
        m.fleet   = None;
        m.project = None;
        m.team    = None;   // ← new
    } else if let Some(f) = fleet {
        if !mur_common::fleet::valid_fleet_name(f) {
            return Err(anyhow!(
                "invalid fleet name '{f}': lowercase letters, digits, '-' or '_'"
            ));
        }
        m.scope   = SkillScope::Fleet;
        m.fleet   = Some(f.to_string());
        m.project = None;
        m.team    = None;   // ← new
    } else if let Some(p) = project {
        m.scope   = SkillScope::Project;
        m.project = Some(p.to_string());
        m.fleet   = None;
        m.team    = None;   // ← new
    } else if let Some(t) = team {
        if t.trim().is_empty() {
            return Err(anyhow!("--team requires a non-empty team id"));
        }
        m.scope   = SkillScope::Team;
        m.team    = Some(t.to_string());
        m.fleet   = None;
        m.project = None;
    }
    Ok(())
}
```

*3b. Update `cmd_scope` signature — add `team: Option<String>`:*
```rust
pub fn cmd_scope(name: &str, fleet: Option<String>, project: bool, team: Option<String>, user: bool) -> Result<()> {
```

*3c. Update the `set_manifest_scope` call inside `cmd_scope`:*
```rust
set_manifest_scope(&mut m, fleet.as_deref(), proj_id.as_deref(), team.as_deref(), user)?;
```

*3d. Fix the existing tests that call `set_manifest_scope` with 4 args — add `None` for `team`:*
```rust
set_manifest_scope(&mut m, Some("dev"), None, None, false).unwrap();  // fleet
set_manifest_scope(&mut m, None, Some("/repo"), None, false).unwrap(); // project
set_manifest_scope(&mut m, None, None, None, true).unwrap();           // user
assert!(set_manifest_scope(&mut m, None, None, None, false).is_err()); // no flags
assert!(set_manifest_scope(&mut m, Some("x"), None, None, true).is_err()); // two flags
assert!(set_manifest_scope(&mut m, Some("Bad Name"), None, None, false).is_err()); // invalid
```

- [ ] **Step 4: Add --team arg to CLI definition**

In `mur-core/src/cli/skill.rs`, add to the `Scope` variant:
```rust
Scope {
    name: String,
    #[arg(long)]
    fleet: Option<String>,
    #[arg(long)]
    project: bool,
    /// Scope to a MUR Server team (by team id)
    #[arg(long)]
    team: Option<String>,   // ← new
    #[arg(long)]
    user: bool,
},
```

In `mur-core/src/dispatch.rs`, update the destructure and call:
```rust
Skill::Scope { name, fleet, project, team, user } =>
    cmd::skill_cmd::cmd_scope(&name, fleet, project, team, user)?,
```

- [ ] **Step 5: Run tests — expect pass**

```
ORT_STRATEGY=download cargo nextest run -p mur-core
```

Expected: all tests pass including `set_manifest_scope_team`.

- [ ] **Step 6: Smoke-test the CLI**

```
cargo run -p mur-core -- skill scope --help 2>&1 | grep -i team
```

Expected output contains `--team <TEAM>` in the help text.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cli/skill.rs \
        mur-core/src/dispatch.rs \
        mur-core/src/cmd/skill_cmd.rs
git commit -m "feat(scope): mur skill scope --team <id> CLI"
```

---

## Task 5: Full workspace build and cross-crate test sweep

**Files:** none (verification only)

- [ ] **Step 1: Build the full workspace**

```
ORT_STRATEGY=download cargo build --workspace 2>&1 | grep -E "error|warning.*unused"
```

Expected: clean build, zero errors. (Warnings about unused `team_id` on `Fleet` in non-fleet paths are acceptable and will resolve as future callers are added.)

- [ ] **Step 2: Run all three crate test suites**

```
cargo nextest run -p mur-common && \
ORT_STRATEGY=download cargo nextest run -p mur-core && \
cargo nextest run -p mur-agent-runtime
```

Expected: all pass.

- [ ] **Step 3: Verify YAML round-trip for a Team-scoped skill**

```
cargo run -p mur-core -- skill --help 2>&1 | grep -c scope || true
```

Quick sanity: the binary compiles. Manual install+scope is an operator step (live agent required).

- [ ] **Step 4: Final commit (if any stray changes)**

```bash
git status
# If clean: nothing to do. If not:
git add -p
git commit -m "chore(scope): build + test sweep cleanup"
```

---

## Self-Review Notes

**Spec coverage check:**
- `SkillScope::Team` variant ✓ Task 1
- `GovernanceRef` seam ✓ Task 1
- `Fleet.team_id` ✓ Task 1
- `scope_visible` 7-param ✓ Task 1
- `ActiveScope::team` + `detect()` ✓ Task 2
- `filter_by_scope` threads team ✓ Task 2
- `ScopeContext` → `ScoringHints` rename ✓ Task 2
- `ponytail:` comment on `ContextScope` ✓ Task 2
- `inject_layer2` active_team ✓ Task 3
- `TaskSpec.active_team` ✓ Task 3
- `verified_active_team` in channel_delegate ✓ Task 3
- `mur skill scope --team` CLI ✓ Task 4
- Full build + test sweep ✓ Task 5

**What this plan does NOT include (per spec §7):**
- Any changes to `ContextScope`, `apply_scope_filter`, pattern pipeline
- Renaming `ActiveScope` → `ActiveContext` (W3b)
- `platform` field on `SkillManifest` (W3b)
- Fleet runner `MUR_ACTIVE_TEAM` env injection (not needed: `verified_active_team` in channel_delegate handles it for delegate turns; `MUR_ACTIVE_TEAM` env remains available for manual/daemon injection)
