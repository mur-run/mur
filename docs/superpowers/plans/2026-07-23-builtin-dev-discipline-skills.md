# Builtin Dev-Discipline Skills Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship 17 built-in dev-discipline skills (1 hub + 16 leaves) internalized from obra/superpowers and mattpocock/skills, with never-shadow install semantics and superpowers-aware index suppression.

**Architecture:** Pure-data YAML skills embedded via `include_str!` in `ensure_mur_skill` (mur-core/src/cmd/sync_cmd.rs), validated by the existing disclosure-budget test pattern. Two small code changes: a config enum + suppression filter in the session-start hook (cmd/hook.rs), and a publisher-based never-shadow guard in the installer. Zero skill-schema changes.

**Tech Stack:** Rust (edition 2024), serde YAML, `regex`, `tempfile` (dev-dep, already used), cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-07-23-builtin-dev-discipline-skills-design.md`

## Global Constraints

- Disclosure budgets (test-enforced): `description` ≤ 120 chars, `content.abstract` ≤ 50 words, body (`content.context`) ≤ 150 lines.
- Every new YAML: `publisher: human:mur-official`, `version: 1.0.0`, content mode `context`.
- All 16 leaves carry `visibility: on_demand`; ONLY the hub `mur-dev` is Indexed (no `visibility:` key) and ONLY the hub has a `session_start` trigger.
- Categories: `workflow` for all, except `mur-dev` and `mur-skill-authoring` = `meta`, `mur-domain-modeling` = `context`.
- Skill bodies in English; trigger keyword regexes bilingual (English + zh-TW). Brand spelled "MUR" in prose.
- Trigger patterns: single-quote YAML strings; no regex backslash escapes (the patterns below are pre-sanitized).
- No changes to any type in `mur-common/src/skill/` (D5). The only shared-type change is adding a defaulted field to `SkillsConfig` in `mur-common/src/config.rs` (Task 2), guarded by a cross-crate literal grep.
- Test env (required): `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Use `cargo nextest run`, never bare `cargo test`. If `cargo` is missing from PATH: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`.
- Every commit message ends with: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- Branch: `feat/builtin-dev-discipline-skills` (already exists; spec committed as 34fc36b5).

## File Structure

**Created (17 skill YAMLs, one per skill):**
`mur-core/src/skills/mur_dev.yaml`, `mur_grilling.yaml`, `mur_brainstorm.yaml`, `mur_domain_modeling.yaml`, `mur_writing_plans.yaml`, `mur_tickets.yaml`, `mur_executing_plans.yaml`, `mur_delegate_dev.yaml`, `mur_worktree.yaml`, `mur_tdd.yaml`, `mur_debugging.yaml`, `mur_code_review.yaml`, `mur_receiving_review.yaml`, `mur_verification.yaml`, `mur_finishing_branch.yaml`, `mur_merge_conflicts.yaml`, `mur_skill_authoring.yaml`

**Created (docs):** `docs/ATTRIBUTIONS.md`

**Modified:**
- `mur-common/src/config.rs` — `DevDisciplineIndex` enum + `SkillsConfig.dev_discipline_index` field + default + tests (Task 2)
- `mur-core/src/cmd/hook.rs` — detection fns + hub suppression before the `format_l0` call at line 388 + tests (Task 3)
- `mur-core/src/cmd/sync_cmd.rs` — never-shadow guard (Task 4); 17 registry entries; test-array rows + new trigger-compile test (Tasks 5–12)
- `README.md` — dev-discipline section (Task 13)

---

### Task 1: Attributions document

**Files:**
- Create: `docs/ATTRIBUTIONS.md`

**Interfaces:**
- Consumes: nothing
- Produces: nothing (docs only)

- [ ] **Step 1: Write the file**

Create `docs/ATTRIBUTIONS.md` with exactly this content:

````markdown
# Attributions

MUR's built-in dev-discipline skills (`mur-dev` and the `mur-*` leaves listed
below) are derived from two MIT-licensed skill collections. Full license
notices are reproduced here as required by the MIT license.

## Sources

| MUR skill | Derived from |
|---|---|
| mur-dev | obra/superpowers `using-superpowers` |
| mur-grilling | mattpocock/skills `grilling`, `grill-me` |
| mur-brainstorm | obra/superpowers `brainstorming` + mattpocock/skills `grill-with-docs` |
| mur-domain-modeling | mattpocock/skills `domain-modeling` |
| mur-writing-plans | obra/superpowers `writing-plans` |
| mur-tickets | mattpocock/skills `to-tickets` (+ `to-spec` notes) |
| mur-executing-plans | obra/superpowers `executing-plans` |
| mur-delegate-dev | obra/superpowers `subagent-driven-development` (rewritten for MUR delegation) |
| mur-worktree | obra/superpowers `using-git-worktrees` |
| mur-tdd | obra/superpowers `test-driven-development`, `testing-anti-patterns` + mattpocock/skills `tdd` |
| mur-debugging | mattpocock/skills `diagnosing-bugs` + obra/superpowers `systematic-debugging` |
| mur-code-review | mattpocock/skills `code-review` + obra/superpowers `code-reviewer` rubric |
| mur-receiving-review | obra/superpowers `receiving-code-review` |
| mur-verification | obra/superpowers `verification-before-completion` |
| mur-finishing-branch | obra/superpowers `finishing-a-development-branch` |
| mur-merge-conflicts | mattpocock/skills `resolving-merge-conflicts` |
| mur-skill-authoring | obra/superpowers `writing-skills`, `persuasion-principles` + mattpocock/skills `writing-great-skills` |

Design record: `docs/superpowers/specs/2026-07-23-builtin-dev-discipline-skills-design.md`.

## MIT License — obra/superpowers

Copyright (c) 2025 Jesse Vincent

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## MIT License — mattpocock/skills

Copyright (c) 2026 Matt Pocock

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
````

- [ ] **Step 2: Commit**

```bash
git add docs/ATTRIBUTIONS.md
git commit -m "docs: MIT attributions for dev-discipline skill sources

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `dev_discipline_index` config key

**Files:**
- Modify: `mur-common/src/config.rs` (SkillsConfig struct ~line 1590, its manual `Default` impl ~line 1625, tests module `skills_config_tests` ~line 2175)

**Interfaces:**
- Consumes: existing `SkillsConfig` struct + manual `Default` impl.
- Produces: `mur_common::config::DevDisciplineIndex` enum (`Auto | Always | Never`, `Default = Auto`, serde `lowercase`) and field `SkillsConfig.dev_discipline_index: DevDisciplineIndex`. Task 3 matches on this.

- [ ] **Step 1: Write the failing test**

Append inside the existing `skills_config_tests` module in `mur-common/src/config.rs`:

```rust
    #[test]
    fn dev_discipline_index_defaults_auto_and_parses() {
        use crate::config::DevDisciplineIndex;
        let cfg: Config = serde_yaml::from_str("").unwrap_or_default();
        assert_eq!(cfg.skills.dev_discipline_index, DevDisciplineIndex::Auto);
        let cfg: Config =
            serde_yaml::from_str("skills:\n  dev_discipline_index: never\n").unwrap();
        assert_eq!(cfg.skills.dev_discipline_index, DevDisciplineIndex::Never);
        let cfg: Config =
            serde_yaml::from_str("skills:\n  dev_discipline_index: always\n").unwrap();
        assert_eq!(cfg.skills.dev_discipline_index, DevDisciplineIndex::Always);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common dev_discipline_index`
Expected: COMPILE ERROR — `DevDisciplineIndex` not found (that is the RED).

- [ ] **Step 3: Implement**

In `mur-common/src/config.rs`, directly above `pub struct SkillsConfig`:

```rust
/// Whether the `mur-dev` discipline hub appears in the session-start learning
/// index on the AI-tool (CLI hook) surface. Runtime injection for MUR agents
/// is never affected by this setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DevDisciplineIndex {
    /// Suppress the hub when a superpowers plugin install is detected (default).
    #[default]
    Auto,
    /// Always list the hub, even when superpowers is installed.
    Always,
    /// Never list the hub on the CLI surface.
    Never,
}
```

Add to `SkillsConfig` (with the other fields):

```rust
    /// See [`DevDisciplineIndex`]. Key: `skills.dev_discipline_index`.
    #[serde(default)]
    pub dev_discipline_index: DevDisciplineIndex,
```

Add to the manual `impl Default for SkillsConfig` block:

```rust
            dev_discipline_index: DevDisciplineIndex::default(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common dev_discipline_index`
Expected: PASS (1 test).

- [ ] **Step 5: Cross-crate literal guard (Tauri gotcha)**

Run: `rg -n "SkillsConfig \{" --glob '*.rs' mur-common mur-core mur-daemon mur-mcp-server mur-agent-runtime mur-gui-core mur-hub-gui mur-agent-gui`
Expected: matches ONLY inside `mur-common/src/config.rs` (the Default impl). Any struct literal elsewhere must be updated in the same commit (workspace-excluded Tauri crates do not compile in CI's main job — a missed literal breaks their build silently).

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): skills.dev_discipline_index (auto|always|never)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Superpowers detection + hub suppression in the session-start hook

**Files:**
- Modify: `mur-core/src/cmd/hook.rs` (suppression before the `format_l0` call at line 388; helper fns + tests at module level)

**Interfaces:**
- Consumes: `mur_common::config::DevDisciplineIndex` (Task 2); `CapabilityIndex { entries: Vec<CapabilityEntry>, .. }` from `mur-core/src/inject/index.rs` (each entry has a `name: String` field — confirm the exact field name by reading the `CapabilityEntry` struct at the top of `inject/index.rs`).
- Produces: `pub(crate) const MUR_DEV_HUB_NAME: &str = "mur-dev"`; `fn superpowers_plugin_present(user_home: &Path) -> bool`; `fn dev_hub_suppressed(idx_cfg: DevDisciplineIndex, user_home: &Path) -> bool`.

- [ ] **Step 1: Write the failing tests**

Append to the existing test module in `mur-core/src/cmd/hook.rs` (the one containing the `## mur learning index` assertion at ~line 178 of `inject/index.rs`'s counterpart — put these in `cmd/hook.rs`'s own `#[cfg(test)] mod`):

```rust
    #[test]
    fn superpowers_detection_and_suppression_matrix() {
        use mur_common::config::DevDisciplineIndex as D;
        let home = tempfile::tempdir().unwrap();
        // No plugin dirs at all → not present.
        assert!(!super::superpowers_plugin_present(home.path()));
        assert!(!super::dev_hub_suppressed(D::Auto, home.path()));
        // Marker dir two levels under plugins/cache → present.
        let plug = home
            .path()
            .join(".claude/plugins/cache/claude-plugins-official/superpowers");
        std::fs::create_dir_all(&plug).unwrap();
        assert!(super::superpowers_plugin_present(home.path()));
        assert!(super::dev_hub_suppressed(D::Auto, home.path()));
        // Config overrides beat detection.
        assert!(!super::dev_hub_suppressed(D::Always, home.path()));
        assert!(super::dev_hub_suppressed(D::Never, home.path()));
        // `Never` suppresses even without a plugin present.
        let empty = tempfile::tempdir().unwrap();
        assert!(super::dev_hub_suppressed(D::Never, empty.path()));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core superpowers_detection`
Expected: COMPILE ERROR — the two fns don't exist yet.

- [ ] **Step 3: Implement the helpers**

Add near the top of `mur-core/src/cmd/hook.rs` (below the existing internal helpers):

```rust
/// Skill name of the dev-discipline hub (spec 2026-07-23, §6).
pub(crate) const MUR_DEV_HUB_NAME: &str = "mur-dev";

/// Directory-name marker identifying a superpowers plugin install.
const SUPERPOWERS_MARKER: &str = "superpowers";

/// True if a Claude-Code superpowers plugin install exists under
/// `<user_home>/.claude/plugins/{cache,repos,marketplaces}` (dir whose name
/// contains "superpowers", checked one and two levels deep).
fn superpowers_plugin_present(user_home: &std::path::Path) -> bool {
    let has_marker = |p: &std::path::Path| {
        p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.to_ascii_lowercase().contains(SUPERPOWERS_MARKER))
    };
    for sub in ["plugins/cache", "plugins/repos", "plugins/marketplaces"] {
        let base = user_home.join(".claude").join(sub);
        let Ok(level1) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in level1.flatten() {
            let p = entry.path();
            if has_marker(&p) {
                return true;
            }
            if p.is_dir()
                && let Ok(level2) = std::fs::read_dir(&p)
            {
                for e2 in level2.flatten() {
                    if has_marker(&e2.path()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether the `mur-dev` hub must be dropped from the CLI learning index.
/// Never affects runtime (agent) injection — this is only called on the
/// session-start hook path.
fn dev_hub_suppressed(
    idx_cfg: mur_common::config::DevDisciplineIndex,
    user_home: &std::path::Path,
) -> bool {
    use mur_common::config::DevDisciplineIndex as D;
    match idx_cfg {
        D::Always => false,
        D::Never => true,
        D::Auto => superpowers_plugin_present(user_home),
    }
}
```

- [ ] **Step 4: Wire the filter at the format_l0 call site**

At `mur-core/src/cmd/hook.rs:388` the index is rendered:

```rust
    let output = crate::inject::index::format_l0(&index, crate::inject::index::L0_BUDGET_CHARS);
```

Immediately before that line, filter the hub (make the `index` binding `mut`; reuse the `Config` and user-home values already in scope in that function — the hook loads config earlier; verify with `rg -n "Config\|home_dir\|dirs::home" mur-core/src/cmd/hook.rs`. If the function has no user-home in scope, obtain it the same way the rest of the file does):

```rust
    if dev_hub_suppressed(cfg.skills.dev_discipline_index, &user_home) {
        index.entries.retain(|e| e.name != MUR_DEV_HUB_NAME);
    }
```

If `cmd/hook.rs` emits any per-skill Layer-2 abstract content for `session_start`-triggered skills on this same path (check: `rg -n "session_start" mur-core/src/cmd/hook.rs mur-core/src/inject`), apply the same `MUR_DEV_HUB_NAME` filter there too. If no such CLI-side path exists, only the index filter is needed.

- [ ] **Step 5: Run tests to verify they pass**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core superpowers_detection`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/hook.rs
git commit -m "feat(hook): suppress mur-dev hub on CLI when superpowers detected

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Never-shadow guard in `ensure_mur_skill`

**Files:**
- Modify: `mur-core/src/cmd/sync_cmd.rs` (the canonical-write loop at ~line 1209 `for (name, content) in skills` and the symlink loop below it; new consts + helper + test)

**Interfaces:**
- Consumes: `mur_common::skill::parse_canonical` (already used in this file's tests); existing `ensure_mur_skill(home, mur_root)` signature.
- Produces: `const NEW_DEV_SKILL_NAMES: &[&str]` (all 17 names, exact list below) — Tasks 5–12 do NOT touch it (it is complete from day one); `fn dev_skill_shadowed_by_user(dir: &Path, name: &str) -> bool`.

- [ ] **Step 1: Write the failing test**

Append a new `#[cfg(test)]` module in `mur-core/src/cmd/sync_cmd.rs`:

```rust
#[cfg(test)]
mod never_shadow_tests {
    /// A user-authored skill occupying a dev-discipline name must survive
    /// `ensure_mur_skill` untouched (spec 2026-07-23 §6 never-shadow).
    #[test]
    fn user_skill_with_dev_name_is_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let mur_root = home.path().join(".mur");
        let dir = mur_root.join("skills").join("mur-tdd");
        std::fs::create_dir_all(&dir).unwrap();
        let user_yaml = "name: mur-tdd\nversion: 0.0.1\npublisher: human:alice\n\
                         description: my own tdd notes\ncategory: workflow\n\
                         content:\n  abstract: mine\n  context: keep me\n";
        std::fs::write(dir.join("skill.yaml"), user_yaml).unwrap();

        super::ensure_mur_skill(home.path(), &mur_root).unwrap();

        let after = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
        assert_eq!(after, user_yaml, "user-authored skill must not be clobbered");
    }

    /// Unparseable existing YAML is treated as user-authored (fail-safe skip).
    #[test]
    fn unparseable_existing_dev_skill_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let mur_root = home.path().join(".mur");
        let dir = mur_root.join("skills").join("mur-tdd");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("skill.yaml"), ": not yaml {{{{").unwrap();

        super::ensure_mur_skill(home.path(), &mur_root).unwrap();

        let after = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
        assert_eq!(after, ": not yaml {{{{");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core never_shadow`
Expected: FAIL — `user_skill_with_dev_name_is_not_overwritten` currently fails only AFTER Task 5 registers `mur-tdd`… it does not: `mur-tdd` is not yet in the registry, so nothing overwrites it and the test passes vacuously. To get a true RED now, the guard test must target the mechanism, not the roster. Therefore ALSO add this direct unit test of the helper:

```rust
    #[test]
    fn shadow_predicate_publisher_rules() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("skill.yaml");
        // Foreign publisher → shadowed (skip).
        std::fs::write(&f, "name: mur-tdd\nversion: 0.0.1\npublisher: human:alice\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  context: c\n").unwrap();
        assert!(super::dev_skill_shadowed_by_user(dir.path(), "mur-tdd"));
        // MUR publisher → not shadowed (update as usual).
        std::fs::write(&f, "name: mur-tdd\nversion: 0.0.1\npublisher: human:mur-official\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  context: c\n").unwrap();
        assert!(!super::dev_skill_shadowed_by_user(dir.path(), "mur-tdd"));
        // Non-dev names never shadow (existing builtin semantics unchanged).
        assert!(!super::dev_skill_shadowed_by_user(dir.path(), "mur-run"));
        // No file on disk → nothing to shadow.
        std::fs::remove_file(&f).unwrap();
        assert!(!super::dev_skill_shadowed_by_user(dir.path(), "mur-tdd"));
    }
```

Run again: COMPILE ERROR — `dev_skill_shadowed_by_user` does not exist. That is the RED.

- [ ] **Step 3: Implement**

Add above `ensure_mur_skill` in `mur-core/src/cmd/sync_cmd.rs`:

```rust
/// Dev-discipline builtin names (spec 2026-07-23). Installation never
/// overwrites a same-named skill the user authored themselves.
const NEW_DEV_SKILL_NAMES: &[&str] = &[
    "mur-dev",
    "mur-grilling",
    "mur-brainstorm",
    "mur-domain-modeling",
    "mur-writing-plans",
    "mur-tickets",
    "mur-executing-plans",
    "mur-delegate-dev",
    "mur-worktree",
    "mur-tdd",
    "mur-debugging",
    "mur-code-review",
    "mur-receiving-review",
    "mur-verification",
    "mur-finishing-branch",
    "mur-merge-conflicts",
    "mur-skill-authoring",
];

/// Publishers whose on-disk copies we own and may update in place.
const MUR_OFFICIAL_PUBLISHERS: &[&str] = &["human:mur-official", "human:mur"];

/// Never-shadow (spec 2026-07-23 §6): true when `name` is a dev-discipline
/// builtin AND `dir/skill.yaml` exists but was not published by MUR.
/// ponytail: publisher-based check; origin_hash edit detection if users
/// report clobbered local edits.
fn dev_skill_shadowed_by_user(dir: &std::path::Path, name: &str) -> bool {
    if !NEW_DEV_SKILL_NAMES.contains(&name) {
        return false;
    }
    let Ok(existing) = std::fs::read_to_string(dir.join("skill.yaml")) else {
        return false;
    };
    match mur_common::skill::parse_canonical(&existing) {
        Ok(m) => !MUR_OFFICIAL_PUBLISHERS.contains(&m.publisher.as_str()),
        Err(_) => true,
    }
}
```

In the canonical-write loop (`for (name, content) in skills` at ~line 1209), insert as the FIRST statement of the body, and collect skipped names:

```rust
    let mut shadowed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, content) in skills {
        let dir = mur_skills_dir.join(name);
        if dev_skill_shadowed_by_user(&dir, name) {
            tracing::info!(skill = name, "skipping builtin install: user-authored skill of the same name exists (never-shadow)");
            shadowed.insert(name);
            continue;
        }
        // … existing create_dir_all / write skill.yaml / write SKILL.md …
    }
```

In the symlink loop below, skip shadowed names the same way:

```rust
        for (name, _) in skills {
            if shadowed.contains(name) {
                continue;
            }
            // … existing symlink_skill_dir call …
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core never_shadow -- shadow_predicate`
Then the full pair: `… cargo nextest run -p mur-core never_shadow`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): never-shadow guard for dev-discipline builtin names

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## YAML batch tasks (5–12) — shared mechanics

Every batch task follows the same cycle (referenced below as "the batch cycle"):

1. **Register first (RED):** append the `("name", include_str!("../skills/<file>.yaml"))` tuples to the `skills` array in `ensure_mur_skill` (insert before the closing `];` at ~line 1186, after the `deep-research-verify` entry), append rows to the `cases` array in `builtin_skill_tests::new_builtin_skills_parse_and_respect_disclosure_budgets` (`(name, include_str!, expect_on_demand)`), and append the same `include_str!` lines to the `yamls` array in `dev_skill_trigger_tests` (created in Task 5).
2. Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core builtin_skill`
   Expected: COMPILE ERROR — `couldn't read ../skills/<file>.yaml` (that is the RED).
3. **Create the YAML file(s)** exactly as given in the task.
4. Run the same command plus the trigger test:
   `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core -E 'test(builtin_skill) + test(dev_skill_keyword_triggers)'`
   Expected: PASS — parse, name, visibility, all three budgets, regex compile.
5. **Commit** with the message given in the task.

Budget reminders while transcribing: description ≤120 chars, abstract ≤50 words, context ≤150 lines. The YAMLs below are pre-sized; do not pad them.

---

### Task 5: Hub skill `mur-dev` + trigger-compile test

**Files:**
- Create: `mur-core/src/skills/mur_dev.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (registry + budget-test row + new test module)

**Interfaces:**
- Consumes: batch cycle; `NEW_DEV_SKILL_NAMES` already lists `mur-dev` (Task 4).
- Produces: test module `dev_skill_trigger_tests` with array `yamls` that Tasks 6–12 extend; budget-test row pattern `("mur-dev", …, false)` — the ONLY row with `expect_on_demand = false` in this plan.

- [ ] **Step 1: Registry + tests (RED)**

Registry tuple: `("mur-dev", include_str!("../skills/mur_dev.yaml"))`.
Budget-test row: `("mur-dev", include_str!("../skills/mur_dev.yaml"), false)`.
New test module (place after `builtin_skill_tests`):

```rust
#[cfg(test)]
mod dev_skill_trigger_tests {
    /// Every dev-discipline keyword trigger must be a valid regex — the
    /// runtime trigger matcher compiles them with `regex::Regex::new`.
    #[test]
    fn dev_skill_keyword_triggers_compile() {
        let yamls: &[&str] = &[include_str!("../skills/mur_dev.yaml")];
        for y in yamls {
            let m = mur_common::skill::parse_canonical(y).expect("parse");
            for t in &m.triggers {
                if let Some(p) = &t.pattern {
                    regex::Regex::new(p).unwrap_or_else(|e| {
                        panic!("{}: trigger regex fails to compile: {e}", m.name)
                    });
                }
            }
        }
    }
}
```

Run the batch-cycle step 2 command. Expected: COMPILE ERROR (missing `mur_dev.yaml`).

- [ ] **Step 2: Create `mur-core/src/skills/mur_dev.yaml`**

```yaml
name: mur-dev
version: 1.0.0
publisher: human:mur-official
description: 'Dev-discipline hub: routes coding tasks to the right mur-* skill before any code is written.'
category: meta
content:
  abstract: |
    Before any dev action, check this routing map: if a discipline skill below
    might apply, load it with `mur skill show <name>` FIRST and follow it.
    Building something new: mur-brainstorm. Fixing anything: mur-debugging.
    Announce which skill you are using.
  context: |
    # Dev discipline — the routing rule

    If there is even a small chance one of these skills applies, load it BEFORE
    acting — before clarifying questions, before exploration, before code.
    Announce it: "Using mur-tdd to ...". User instructions always beat skills.

    ## Route by situation

    - Build or change something → `mur-brainstorm` (design gate) →
      `mur-writing-plans` → execute (`mur-delegate-dev` when MUR delegation is
      available, else `mur-executing-plans`) → `mur-finishing-branch`.
    - Any bug, test failure, unexpected behavior → `mur-debugging`. Never fix
      before root cause.
    - Writing or changing code → `mur-tdd`. Failing test first, always.
    - About to claim done/fixed/passing → `mur-verification`. Evidence first.
    - Reviewing a diff → `mur-code-review`. Feedback arrived → `mur-receiving-review`.
    - Sharpening an idea or decision → `mur-grilling`. Vocabulary drifting →
      `mur-domain-modeling`.
    - Slicing a plan into dispatchable pieces → `mur-tickets`.
    - Merge/rebase conflict → `mur-merge-conflicts`.
    - Starting feature work in a shared checkout → `mur-worktree` first.
    - Writing or editing a MUR skill → `mur-skill-authoring`.
    - Parallel fan-out topology questions belong to `parallel-decompose` /
      `parallel-topology-guide`; `mur-delegate-dev` executes ONE plan via
      delegation. Different jobs.

    ## Execution backends (no sub-agents here)

    MUR agents cannot spawn sub-agents. Where a methodology says "dispatch a
    subagent": run it in-context sequentially by default; upgrade to MUR
    delegation (fleet member, parallel_jobs, workflow delegate) only when one is
    observably available (e.g. `mur agent list` shows a running agent). Track
    progress with checkboxes in the plan/ticket file on disk, never in memory.
    *Why: files survive context compaction; recollection does not.*

    ## Red flags — you are rationalizing

    | Thought | Reality |
    |---|---|
    | "Just a simple question/task" | Simple tasks are where assumptions bite. Check the map. |
    | "I need context first" | The skill tells you HOW to gather context. Load it first. |
    | "The skill is overkill" | Overkill is cheaper than rework. Use it. |
    | "I remember what it says" | Skills evolve. Load the current version. |
    | "I'll just do this one thing first" | Check BEFORE doing anything. |
    | "No time for process right now" | Discipline is faster than thrashing. Especially now. |
tags:
- dev
- discipline
- hub
triggers:
- type: session_start
- type: keyword
  pattern: 'implement|build|fix|bug|refactor|feature|test|review|debug|實作|開發|修復|除錯|重構|測試|審查'
- type: manual
```

- [ ] **Step 3: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/skills/mur_dev.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-dev discipline hub builtin

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Questioning & design batch — `mur-grilling`, `mur-brainstorm`, `mur-domain-modeling`

**Files:**
- Create: `mur-core/src/skills/mur_grilling.yaml`, `mur_brainstorm.yaml`, `mur_domain_modeling.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (3 registry tuples; 3 budget rows, all `true`; 3 trigger-test lines)

**Interfaces:**
- Consumes: batch cycle; `dev_skill_trigger_tests.yamls` array (Task 5).
- Produces: skill names `mur-grilling`, `mur-brainstorm`, `mur-domain-modeling` referenced by hub routing (already written in Task 5) and by later YAML cross-references.

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2 for the three files.

- [ ] **Step 2: Create `mur-core/src/skills/mur_grilling.yaml`**

```yaml
name: mur-grilling
version: 1.0.0
publisher: human:mur-official
description: 'Interview the user one question at a time, each with a recommended answer, until the plan is sharp.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Relentless interview protocol: one question per turn, ship a recommended
    answer with each, look up facts yourself and ask only decisions, resolve
    decision dependencies in order, stop when the user confirms shared
    understanding.
  context: |
    # Grilling — sharpen thinking by interview

    - ONE question at a time; wait for the answer.
      *Why: multiple questions at once is bewildering, and the answers arrive
      shallow.*
    - Every question ships with your recommended answer, so the user can accept
      it in a word or push back.
    - Fact/decision split: anything discoverable from the environment
      (filesystem, git, tools, docs) is a fact — look it up, never ask.
      Decisions are always the user's.
    - Walk the decision tree in dependency order: a question whose answer
      depends on an unsettled one waits its turn.
    - No question cap. The user steers with natural language ("wrap up",
      "summarise what we have").
    - Do not act until the user confirms shared understanding.

    ## Surfaces

    - murmur / agent runtime: present choices via `suggest_replies` structured
      options `{text, description}`; never hand-number options.
    - CLI or plain chat: one short message = the single question + your
      recommendation.
tags:
- questioning
- design
triggers:
- type: keyword
  pattern: 'grill|stress.?test (my|this|the)|challenge my|烤問|質問|盤問|挑戰我'
- type: manual
```

- [ ] **Step 3: Create `mur-core/src/skills/mur_brainstorm.yaml`**

```yaml
name: mur-brainstorm
version: 1.0.0
publisher: human:mur-official
description: 'Turn an idea into an approved written design before any code. Use before creating or changing behavior.'
category: workflow
visibility: on_demand
content:
  abstract: |
    HARD GATE: no code, scaffolding, or implementation until a design is
    presented and approved — every project, however simple. Decompose oversized
    requests, question via mur-grilling, propose 2-3 approaches, present the
    design in sections, write and commit the spec.
  context: |
    # Brainstorm — idea to approved design

    ## The gate

    Do NOT write code, scaffold projects, or start implementation until the
    user approves a presented design. "Too simple to need design" is the
    anti-pattern: simple projects are where unexamined assumptions waste the
    most work. A simple project's design may be three sentences — but it gets
    presented and approved.

    ## Process

    1. Scope check: a request spanning multiple independent subsystems is
       decomposed into sub-projects first; design the first one only.
    2. Explore project context (files, docs, recent commits) before questioning.
    3. Question with the `mur-grilling` protocol (one at a time, recommendation
       attached).
    4. Propose 2-3 approaches with trade-offs; lead with your recommendation.
    5. Present the design in sections scaled to complexity; get approval per
       section. Cover architecture, components, data flow, error handling,
       testing.
    6. Write the spec to `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`
       (follow the project's own convention if it differs) and commit it.
    7. Self-review the spec: placeholders, contradictions, ambiguity, scope.
       Fix inline.
    8. Ask the user to review the written spec. On approval, hand off to
       `mur-writing-plans`.

    ## Notes

    - New vocabulary or a landed decision mid-design → `mur-domain-modeling`.
    - Design for isolation: each unit has one purpose and a well-defined
      interface, understandable without reading its internals.
tags:
- design
- spec
triggers:
- type: keyword
  pattern: 'brainstorm|design (this|a|the)|from scratch|new feature|發想|從想法|設計一個'
- type: manual
```

- [ ] **Step 4: Create `mur-core/src/skills/mur_domain_modeling.yaml`**

```yaml
name: mur-domain-modeling
version: 1.0.0
publisher: human:mur-official
description: 'Build and sharpen the project glossary (CONTEXT.md) and ADRs when terms are fuzzy or decisions land.'
category: context
visibility: on_demand
content:
  abstract: |
    CONTEXT.md is a glossary and nothing else; docs/adr/ holds decisions.
    Create files lazily, challenge conflicting terms immediately, sharpen fuzzy
    language into one canonical term, update inline the moment a term resolves,
    offer ADRs sparingly.
  context: |
    # Domain modeling — glossary + ADRs

    - `CONTEXT.md` = glossary ONLY: term, 1-2 sentence definition, and an
      `Avoid:` list of banned synonyms. Project-specific concepts only; no
      implementation detail, no spec content.
    - `docs/adr/` = decision records. Multi-context repos: a root
      `CONTEXT-MAP.md` points at per-context `CONTEXT.md` files.
    - Create files lazily — only when there is something to write.

    ## Active moves

    - Challenge against the glossary: a term used against its definition gets
      called out immediately ("the glossary says X means A; you seem to mean
      B — which is it?").
    - Sharpen fuzzy language: a vague or overloaded term → propose one precise
      canonical and record it.
    - Stress-test with concrete edge-case scenarios that force precision at
      concept boundaries.
    - Cross-reference with code: when the user's claim and the code disagree,
      surface it.
    - Update CONTEXT.md inline the MOMENT a term resolves. Never batch.
      *Why: batched glossary edits get lost when the session ends.*

    ## ADR gate — offer only when ALL three hold

    1. Hard to reverse. 2. Surprising without context. 3. Result of a real
    trade-off. Any missing → skip the ADR.
tags:
- glossary
- adr
triggers:
- type: keyword
  pattern: 'glossary|terminology|ubiquitous language|ADR|architecture decision|詞彙|術語|決策紀錄'
- type: manual
```

- [ ] **Step 5: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skills/mur_grilling.yaml mur-core/src/skills/mur_brainstorm.yaml mur-core/src/skills/mur_domain_modeling.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-grilling, mur-brainstorm, mur-domain-modeling builtins

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Planning batch — `mur-writing-plans`, `mur-tickets`

**Files:**
- Create: `mur-core/src/skills/mur_writing_plans.yaml`, `mur_tickets.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (2 registry tuples; 2 budget rows `true`; 2 trigger-test lines)

**Interfaces:**
- Consumes: batch cycle.
- Produces: names `mur-writing-plans`, `mur-tickets` (referenced by hub and by `mur-executing-plans`/`mur-delegate-dev` in Task 8).

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2.

- [ ] **Step 2: Create `mur-core/src/skills/mur_writing_plans.yaml`**

```yaml
name: mur-writing-plans
version: 1.0.0
publisher: human:mur-official
description: 'Write an implementation plan a zero-context engineer can execute mechanically. Use after a spec exists.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Plans assume a skilled engineer with zero codebase knowledge and poor test
    taste: exact paths, complete code in steps, exact commands with expected
    output, Interfaces blocks between tasks, Global Constraints copied
    verbatim, no placeholders anywhere.
  context: |
    # Writing plans — spec to mechanical execution

    Audience model: skilled developer, zero project context, questionable
    taste. If they could do it wrong, the plan must make wrong impossible.

    ## Structure

    - Header: Goal (one sentence), Architecture (2-3 sentences), Tech stack,
      and **Global Constraints** — spec-wide requirements copied verbatim, one
      line each; every task implicitly includes them. Add a banner naming the
      execution skill (`mur-delegate-dev` or `mur-executing-plans`).
    - File-structure section BEFORE tasks: every file created/modified, one
      responsibility each. Decomposition is locked in here.
    - A task is the smallest unit with its own test cycle, worth a fresh review
      gate. Steps are 2-5 minutes each, checkbox syntax: write failing test /
      watch it fail / minimal code / watch it pass / commit.
    - Per-task **Interfaces** block: Consumes (from earlier tasks) and Produces
      (what later tasks rely on) with exact names and signatures. An
      implementer sees only their own task; this block is how neighboring
      types stay consistent. When a plan becomes a MUR workflow DAG, this
      block is the depends_on output threading.

    ## No placeholders — these are plan failures

    "TBD", "add appropriate error handling", "write tests for the above",
    "similar to Task N" (repeat the code instead), steps that describe without
    showing, references to types no task defines.

    ## Self-review before handoff

    Spec-coverage walk (every requirement points at a task), placeholder scan,
    cross-task type-consistency check (clearLayers() in Task 3 vs
    clearFullLayers() in Task 7 is a bug).
tags:
- planning
triggers:
- type: keyword
  pattern: 'implementation plan|write (the|a|an) plan|實作計畫|寫計畫'
- type: manual
```

- [ ] **Step 3: Create `mur-core/src/skills/mur_tickets.yaml`**

```yaml
name: mur-tickets
version: 1.0.0
publisher: human:mur-official
description: 'Slice a spec or plan into tracer-bullet tickets with blocking edges, each sized to one context window.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Vertical slices only: each ticket cuts a narrow complete path through
    every layer, demoable alone, sized to a single fresh context window.
    Declare blocking edges, work the frontier, use expand-contract for wide
    refactors, quiz the user before publishing.
  context: |
    # Tickets — tracer bullets with edges

    ## Slice rules

    - Vertical, never horizontal: a narrow but COMPLETE path through every
      layer (schema, logic, interface, tests), demoable on its own.
    - Sized to ONE fresh context window.
      *Why: each ticket is executed by a fresh agent session; oversized
      tickets die mid-context.*
    - Prefactoring first: make the change easy, then make the easy change.
    - Every ticket declares its blocking edges. No blockers = the frontier =
      startable now.

    ## Wide-refactor exception — expand-contract

    One mechanical change with codebase-wide blast radius: an expand ticket
    (new form beside old) → migration-batch tickets (each blocked by expand,
    CI green throughout) → a contract ticket (delete the old form; blocked by
    every batch). Batches that cannot stay green alone share an integration
    branch, all blocking one final integrate-and-verify ticket.

    ## Publishing

    - Quiz the user first: numbered list of Title / Blocked-by / What it
      delivers; ask about granularity, edges, merge/split. Iterate to approval.
    - Default backend: one file per ticket `docs/tickets/<slug>/NN-<slug>.md`
      with `Status:` and `Blocked-by:` header lines. Repos that live on GitHub
      issues: one issue each with native blocking links.
    - Ticket body: end-to-end behavior from the user's perspective plus
      acceptance checkboxes. No file paths, no code snippets (they go stale) —
      except prototype-sourced snippets that encode a decision.
    - MUR tie-in: ticket files are valid fleet job specs (spec-file dispatch).
tags:
- planning
- tickets
triggers:
- type: keyword
  pattern: 'ticket|vertical slice|split (the|this) (plan|spec)|切票|拆票|工單'
- type: manual
```

- [ ] **Step 4: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/skills/mur_writing_plans.yaml mur-core/src/skills/mur_tickets.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-writing-plans, mur-tickets builtins

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Execution batch — `mur-executing-plans`, `mur-delegate-dev`, `mur-worktree`

**Files:**
- Create: `mur-core/src/skills/mur_executing_plans.yaml`, `mur_delegate_dev.yaml`, `mur_worktree.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (3 registry tuples; 3 budget rows `true`; 3 trigger-test lines)

**Interfaces:**
- Consumes: batch cycle; names from Tasks 5–7 referenced in bodies (`mur-writing-plans`, `mur-finishing-branch` — the latter ships in Task 11; a name reference in prose does not need the file to exist yet).
- Produces: names `mur-executing-plans`, `mur-delegate-dev`, `mur-worktree`.

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2.

- [ ] **Step 2: Create `mur-core/src/skills/mur_executing_plans.yaml`**

```yaml
name: mur-executing-plans
version: 1.0.0
publisher: human:mur-official
description: 'Execute a written plan in-context, task by task, stopping on blockers. Default when no MUR delegation exists.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Read the plan critically and raise concerns first; isolate via
    mur-worktree; execute tasks exactly as written, tick checkboxes in the
    plan file, run each task's verifications, stop and ask on any blocker;
    finish with mur-finishing-branch.
  context: |
    # Executing plans — in-context mode

    1. Read the whole plan. Review critically; raise concerns with the user
       BEFORE starting, not mid-way.
    2. Isolate first: `mur-worktree`. Never implement on main/master without
       explicit consent.
    3. Per task, in order: follow the steps exactly as written → run the
       task's stated verifications → tick the `- [ ]` checkboxes in the plan
       file itself.
       *Why: the plan file is the durable ledger; it survives context
       compaction, your recollection does not.*
    4. STOP immediately and ask when: blocked, the plan has a critical gap, an
       instruction is unclear, or a verification fails twice. Never guess,
       never force through, never silently deviate.
    5. The user updated the plan mid-run → re-read it before continuing.
    6. All tasks done → `mur-finishing-branch`.
tags:
- execution
triggers:
- type: keyword
  pattern: 'execute (the )?plan|run the plan|follow the plan|執行計畫|照計畫'
- type: manual
```

- [ ] **Step 3: Create `mur-core/src/skills/mur_delegate_dev.yaml`**

```yaml
name: mur-delegate-dev
version: 1.0.0
publisher: human:mur-official
description: 'Execute a plan by delegating tasks to MUR agents with briefs, a status protocol, reviews, and a ledger.'
category: workflow
visibility: on_demand
content:
  abstract: |
    When a MUR delegation surface exists, a router farms plan tasks to fresh
    delegates: curated brief files (paths, never pasted content),
    DONE/NEEDS_CONTEXT/BLOCKED status protocol, two-stage review per task, one
    fix pass after the final review, ledger on disk.
  context: |
    # Delegate development — MUR's subagent-driven mode

    ## Precondition (observable)

    A delegation surface is available: `mur agent list` shows a running agent,
    you are in a murmur session, or parallel_jobs / workflow delegate is
    wired. None available → use `mur-executing-plans` instead.

    ## Roles

    - Router (you): coordination only. Never implements, never pastes history.
    - Delegate: fresh context per task; receives a brief FILE path plus a
      report contract.

    ## Per task

    1. Extract the task into a brief file: the task text, cross-task
       Interfaces, ambiguity resolutions, and "write your report to <path>".
    2. Dispatch with file paths over A2A, never inline content.
       *Why: pasted context bloats every hop and diverges from the file of
       record.*
    3. The delegate replies with a status line:
       DONE / DONE_WITH_CONCERNS / NEEDS_CONTEXT: <what> / BLOCKED: <why>.
       Handling: CONCERNS → read the report, judge; NEEDS_CONTEXT → answer in
       the brief, re-dispatch; BLOCKED → resolve or escalate to the user.
       Never re-send an unchanged brief to a stuck delegate.
    4. Two-stage review before the next task: spec compliance, then code
       quality (`mur-code-review` rubric; a second delegate when available,
       a self-review pass otherwise). Critical/Important findings are fixed
       now; Minor go to the ledger.
    5. Append to the ledger file (progress.md next to the plan):
       "Task N: complete (commits X..Y, review clean)".

    ## Finish

    Final whole-branch review → collect ALL findings → ONE fix pass with the
    complete list.
    *Why: per-finding fix dispatches cost more than all tasks combined.*

    ## Hard rules

    - After compaction, trust the ledger and git log over memory; NEVER
      re-dispatch a task the ledger marks complete.
    - Verify delegate claims per `mur-verification`: check the diff, not the
      reply.
    - Never pre-judge findings in a reviewer brief ("don't flag X" = rigging
      the review).
    - Model tiering via the model registry (`mur model role`): cheap tier for
      transcription-grade tasks (the plan contains the code), capable tier
      for reviews and architecture.
tags:
- execution
- delegation
triggers:
- type: keyword
  pattern: 'delegate (the )?(plan|tasks|work)|sdd|subagent|委派|派工'
- type: manual
```

- [ ] **Step 4: Create `mur-core/src/skills/mur_worktree.yaml`**

```yaml
name: mur-worktree
version: 1.0.0
publisher: human:mur-official
description: 'Isolate feature work in a git worktree before starting. Use when beginning work in a shared checkout.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Detect existing isolation first; prefer the harness's native worktree
    tool; otherwise git worktree add under .worktrees/ after a git
    check-ignore gate. Auto-detect setup, and require a green test baseline
    before any work starts.
  context: |
    # Worktree — isolate before you build

    Failure this prevents: two agents (or you and the user) mutating one
    checkout.

    1. Detect existing isolation: `git rev-parse --git-dir` differs from
       `git rev-parse --git-common-dir` (and no superproject) → already in a
       worktree; skip creation.
    2. Prefer the harness's native worktree tool when one exists (never fight
       the harness). Otherwise:
    3. `git worktree add`, directory priority: explicit instruction >
       existing `.worktrees/` > existing `worktrees/` > create `.worktrees/`.
       GATE: `git check-ignore .worktrees` must pass BEFORE creating;
       otherwise add it to `.gitignore` and commit that first.
       *Why: an unignored worktree dir turns every status/diff into noise and
       can get committed.*
       (`.worktrees/` is MUR's fleet-track convention; the reconcilers own it.)
    4. Setup by detection: Cargo.toml → cargo check; package.json → install
       deps; pyproject/requirements → venv + install; go.mod → go build ./...
    5. Baseline: run the test suite. It must be GREEN before work starts;
       red → report and ask.
       *Why: on a red baseline, new bugs and pre-existing bugs are
       indistinguishable.*

    Red flags: nested worktrees; skipping the ignore gate; building on a red
    baseline. Paired cleanup: `mur-finishing-branch`.
tags:
- git
- isolation
triggers:
- type: keyword
  pattern: 'worktree|isolated (checkout|workspace)|隔離(開發|環境)'
- type: manual
```

- [ ] **Step 5: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skills/mur_executing_plans.yaml mur-core/src/skills/mur_delegate_dev.yaml mur-core/src/skills/mur_worktree.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-executing-plans, mur-delegate-dev, mur-worktree builtins

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Quality batch — `mur-tdd`, `mur-debugging`

**Files:**
- Create: `mur-core/src/skills/mur_tdd.yaml`, `mur_debugging.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (2 registry tuples; 2 budget rows `true`; 2 trigger-test lines)

**Interfaces:**
- Consumes: batch cycle.
- Produces: names `mur-tdd`, `mur-debugging`.

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2.

- [ ] **Step 2: Create `mur-core/src/skills/mur_tdd.yaml`**

```yaml
name: mur-tdd
version: 1.0.0
publisher: human:mur-official
description: 'Red-green-refactor with pre-agreed seams. Use before writing any implementation code or fixing any bug.'
category: workflow
visibility: on_demand
content:
  abstract: |
    NO production code without a failing test first — watch it fail, minimal
    code to green, refactor only on green. Agree test seams with the user
    first, work vertical slices, mock only true boundaries, never test mock
    behavior.
  context: |
    # TDD — the iron law

    ```
    NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST
    ```

    Violating the letter of the rule is violating the rule. Wrote code before
    its test? Delete it and start over. Delete means delete: not commented
    out, not kept as reference, not "adapted".

    ## Seams first

    Before writing any test, name the public seams you will test at and
    confirm them with the user. Test ONLY at agreed seams — through public
    interfaces, as a caller would. Wanting to test past the interface means
    the interface is the wrong shape.

    ## The loop (one vertical slice at a time)

    1. RED: one minimal test for ONE behavior, named for the behavior. Run
       it. WATCH it fail, for the expected reason (feature missing, not a
       typo). Passes immediately? You are testing existing behavior — fix the
       test.
    2. GREEN: the simplest code that passes. YAGNI: no options, no hooks, no
       speculative parameters. Run it. Watch it pass with the WHOLE suite
       green and output pristine (no new warnings).
    3. REFACTOR only on green: duplication, names. No new behavior.

    Repeat test by test. Never write all tests first: batched tests verify
    imagined behavior; each cycle must respond to what the last one taught.

    ## Anti-patterns (auto-reject)

    - Implementation-coupled test: mocks internal collaborators, touches
      private methods, verifies through a side channel (queries the DB
      instead of the interface). Tell: breaks on refactor without behavior
      change.
    - Tautological test: the assertion recomputes the expected value the way
      the code does — it passes by construction. Expected values come from an
      independent source (known-good literal, worked example, spec).
    - Mocking what you own: mock ONLY true boundaries (external APIs, clock,
      randomness). Never your own classes. Mock setup bigger than the test is
      a smell.
    - Test-only methods on production types → move them to test utilities.

    ## Bug fixes

    Failing reproduction test FIRST, always. No repro test = the bug is not
    fixed, it is hidden.

    | Excuse | Reality |
    |---|---|
    | "Too simple to test" | Simple code breaks; the test costs two minutes. |
    | "I'll add tests after" | Tests-after pass immediately and prove nothing. |
    | "It's just a refactor" | Refactors change behavior more often than you think. |
    | "Deadline pressure" | Debugging untested code is slower than TDD. |
tags:
- testing
- tdd
triggers:
- type: keyword
  pattern: 'tdd|test.?first|red.?green|failing test|先寫測試|測試驅動'
- type: manual
```

- [ ] **Step 3: Create `mur-core/src/skills/mur_debugging.yaml`**

```yaml
name: mur-debugging
version: 1.0.0
publisher: human:mur-official
description: 'Feedback-loop-first debugging. Use on any bug, failure, or regression before proposing a fix.'
category: workflow
visibility: on_demand
content:
  abstract: |
    No fixes without root cause. Phase 1 builds a tight red-capable feedback
    loop — that IS the skill; then minimise, rank 3-5 falsifiable hypotheses,
    probe one variable at a time, regression-test before fixing, stop after
    three failed fixes.
  context: |
    # Debugging — loop first, hypotheses second

    ```
    NO FIXES WITHOUT ROOT CAUSE INVESTIGATION FIRST
    ```

    Especially under time pressure: systematic is faster than thrashing.

    ## Phase 1 — build a feedback loop (this IS the skill)

    A tight signal that goes red on THIS bug: fast (seconds), sharp (asserts
    the exact symptom, not "didn't crash"), deterministic (pin time, seed
    RNG, isolate fs/network). Tactic ladder — first that fits:
    failing test at a reachable seam → curl/HTTP script → CLI run diffed
    against known-good output → replay a captured trace → throwaway harness
    (minimal subset, one call) → git bisect run → differential run (old vs
    new, diff outputs) → a script driving the human, as last resort.
    Flaky bug? Raise the reproduction rate first (loop 100x, stress, inject
    sleeps). Cannot build a loop → STOP: say so, list attempts, ask for an
    artifact (log dump, trace, recording) or access. Do NOT hypothesize
    without a loop.

    ## Phase 2 — confirm and minimise

    Run red; confirm it is the USER'S exact failure mode (wrong bug = wrong
    fix). Cut inputs/config/callers one at a time, re-running each cut, until
    every remaining element is load-bearing. Read the full error text; check
    recent diffs; in multi-layer systems instrument every boundary once to
    find the failing layer.

    ## Phase 3 — hypotheses

    Write 3-5 ranked, falsifiable hypotheses BEFORE testing any: "if X is the
    cause, changing Y makes it disappear." No prediction = a vibe — discard
    it. Show the ranking to the user when present.

    ## Phase 4 — probe

    One variable at a time. Prefer a debugger/REPL over logs; logs only at
    hypothesis-distinguishing boundaries, each tagged [DEBUG-<id>] so cleanup
    is one grep. Never "log everything and grep".

    ## Phase 5 — lock it down, then fix

    Regression test BEFORE the fix, at a correct seam. No correct seam
    exists? That is itself a finding — the architecture prevents locking this
    bug down; flag it. Then: watch the test fail → fix → watch it pass →
    re-run the Phase-1 loop on the original scenario.

    ## Phase 6 — clean up

    Original repro dead; regression test in; every [DEBUG- tag removed
    (grep); the winning hypothesis stated in the commit message.

    ## 3-strike rule

    Three failed fix attempts → STOP. The problem is architectural; question
    the pattern with the user before any fourth attempt.

    | Excuse | Reality |
    |---|---|
    | "Just try changing X" | Guessing burns hours; the loop takes minutes. |
    | "It's obviously X" | Obvious causes are wrong half the time. Prove it. |
    | "No time for process" | 30 systematic minutes beat 3 thrashing hours. |
tags:
- debugging
triggers:
- type: keyword
  pattern: 'debug|diagnose|root cause|why (is|does|did) (it|this|the).{0,30}(fail|break|crash)|除錯|診斷|壞掉'
- type: manual
```

- [ ] **Step 4: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/skills/mur_tdd.yaml mur-core/src/skills/mur_debugging.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-tdd, mur-debugging builtins

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: Review batch — `mur-code-review`, `mur-receiving-review`, `mur-verification`

**Files:**
- Create: `mur-core/src/skills/mur_code_review.yaml`, `mur_receiving_review.yaml`, `mur_verification.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (3 registry tuples; 3 budget rows `true`; 3 trigger-test lines)

**Interfaces:**
- Consumes: batch cycle.
- Produces: names `mur-code-review`, `mur-receiving-review`, `mur-verification`.

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2.

- [ ] **Step 2: Create `mur-core/src/skills/mur_code_review.yaml`**

```yaml
name: mur-code-review
version: 1.0.0
publisher: human:mur-official
description: 'Two-axis diff review: Standards (repo rules + Fowler smells) and Spec faithfulness, with explicit verdicts.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Pin the diff range first. Review two independent axes — Standards
    (repo-documented rules plus the Fowler smell baseline as judgement calls)
    and Spec (faithfulness to the originating spec) — never merged, 400 words
    max each, strengths first, explicit verdict.
  context: |
    # Code review — two axes, one verdict each

    ## Setup

    - Pin the fixed point (ask if unspecified). Diff = git diff
      <fixed>...HEAD (three-dot, merge-base) plus git log <fixed>..HEAD
      --oneline. Validate the ref resolves and the diff is non-empty before
      reviewing anything.
    - Find the spec source: issue refs in commits → a user-provided path → a
      spec under docs/ matching the branch → ask. No spec → the Spec axis
      reports "no spec available", visibly, and is skipped.
    - The reviewer is read-only: never mutate tree, index, or HEAD.

    ## Axis 1 — Standards

    Repo-documented standards (CONTRIBUTING, style docs) PLUS the Fowler
    smell baseline as NAMED judgement-call heuristics: Mysterious Name,
    Duplicated Code, Feature Envy, Data Clumps, Primitive Obsession, Repeated
    Switches, Shotgun Surgery, Divergent Change, Speculative Generality,
    Message Chains, Middle Man, Refused Bequest. The repo overrides the
    baseline; skip anything tooling already enforces.

    ## Axis 2 — Spec

    Missing or partial requirements, scope creep, implemented-but-wrong.
    Quote the spec line for every finding.

    ## Execution

    Two delegate turns when a MUR delegation surface is available; otherwise
    two sequential passes. Each axis stays under 400 words and is reported
    under its own heading — never merged, never cross-ranked.

    ## Output contract (per axis)

    1. Strengths first — accurate praise builds trust in the criticism.
    2. Findings: Critical (bugs, security, data loss) / Important
       (architecture, missing features, test gaps) / Minor (style, docs) —
       each with file:line, what, why, how.
    3. Verdict: Ready to merge — Yes / No / With fixes, plus one sentence.

    Calibration: not everything is Critical; plan deviations get flagged for
    intent confirmation; plan bugs are plan bugs. Never pre-judge findings
    when briefing a reviewer.
tags:
- review
triggers:
- type: keyword
  pattern: 'code review|review (this|the|my) (diff|branch|pr|code)|審查|幫我 review'
- type: manual
```

- [ ] **Step 3: Create `mur-core/src/skills/mur_receiving_review.yaml`**

```yaml
name: mur-receiving-review
version: 1.0.0
publisher: human:mur-official
description: 'Process inbound review feedback with verification, not performative agreement. Use when feedback arrives.'
category: workflow
visibility: on_demand
content:
  abstract: |
    Read everything, restate to confirm understanding, verify each claim
    against the codebase, evaluate fit, then respond — technical
    acknowledgment or reasoned pushback. Unclear items stop everything.
    Implement one item at a time, testing each.
  context: |
    # Receiving review — evaluate, don't obey

    Pattern: READ all → UNDERSTAND (restate or ask) → VERIFY against the
    codebase → EVALUATE for THIS codebase → RESPOND → IMPLEMENT one item at a
    time, testing each.

    ## Forbidden responses

    "You're absolutely right!", "Great point!", gratitude, any performative
    agreement. Actions over words: state the fix ("Fixed — <what changed>")
    or show the code.
    *Why: sycophancy destroys the reviewer's trust in every future reply.*

    ## Rules

    - ANY unclear item → stop entirely. Do not implement even the understood
      subset; items may be related, and partial understanding produces wrong
      code.
    - External reviewers = suggestions to evaluate, not orders: is it correct
      for this codebase? does it break anything? why is the current code the
      way it is?
    - Human partner = trusted; implement after understanding — still no
      sycophancy.
    - YAGNI check on "implement X properly": grep actual usage first; unused →
      propose removal instead.
    - Push back with technical reasoning when feedback is wrong here (breaks
      behavior, missing context, conflicts with the architecture). Pushed back
      wrongly? Factual correction, no apology spiral.
    - Order: clarify everything → blocking items → simple → complex.
tags:
- review
triggers:
- type: keyword
  pattern: 'review feedback|reviewer (said|says|asked)|address (the )?(review|feedback|comments)|回覆審查|處理意見'
- type: manual
```

- [ ] **Step 4: Create `mur-core/src/skills/mur_verification.yaml`**

```yaml
name: mur-verification
version: 1.0.0
publisher: human:mur-official
description: 'Evidence before completion claims. Use before saying done, fixed, or passing, or trusting a delegate report.'
category: workflow
visibility: on_demand
content:
  abstract: |
    No completion claims without fresh verification evidence in the same
    message: identify the proving command, run it, read the output, then
    claim with evidence attached. Delegate and fleet reports are claims —
    verify the diff, never the self-report.
  context: |
    # Verification — evidence or it didn't happen

    ```
    NO COMPLETION CLAIMS WITHOUT FRESH VERIFICATION EVIDENCE
    ```

    If the proving command did not run in THIS message, you cannot claim it
    passes.

    ## The gate

    1. IDENTIFY the command that proves the claim.
    2. RUN it, fresh and in full.
    3. READ the whole output: exit code, failure count, warnings.
    4. VERIFY the output actually supports the claim.
    5. Only then claim — WITH the evidence shown.

    Skipping any step is lying, not verifying.

    ## Claim → required evidence

    | Claim | Evidence |
    |---|---|
    | Tests pass | Fresh full run, 0 failures — not a previous run |
    | Build works | Build exit code 0 — a green linter proves nothing |
    | Bug fixed | The ORIGINAL symptom re-tested and gone |
    | Regression test works | Red-green proven: revert fix → test FAILS → restore → passes |
    | Delegate/agent finished | The VCS diff or channel evidence — NEVER the agent's own report |
    | Requirements met | Line-by-line checklist against the spec |

    A fleet member emitting its done_when marker or replying DONE is a claim,
    not evidence. Check the diff.

    ## Red flags

    "should work", "probably", "seems to", any satisfaction ("Done!",
    "Perfect!") before the evidence, trusting a sub-report, "just this once".
tags:
- verification
triggers:
- type: keyword
  pattern: 'verify|evidence|double.?check|really (done|fixed|works)|驗證|確認(完成|修好)|真的(好|修好)了'
- type: manual
```

- [ ] **Step 5: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skills/mur_code_review.yaml mur-core/src/skills/mur_receiving_review.yaml mur-core/src/skills/mur_verification.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-code-review, mur-receiving-review, mur-verification builtins

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: Finishing batch — `mur-finishing-branch`, `mur-merge-conflicts`

**Files:**
- Create: `mur-core/src/skills/mur_finishing_branch.yaml`, `mur_merge_conflicts.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (2 registry tuples; 2 budget rows `true`; 2 trigger-test lines)

**Interfaces:**
- Consumes: batch cycle.
- Produces: names `mur-finishing-branch`, `mur-merge-conflicts`.

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2.

- [ ] **Step 2: Create `mur-core/src/skills/mur_finishing_branch.yaml`**

```yaml
name: mur-finishing-branch
version: 1.0.0
publisher: human:mur-official
description: 'Finish a development branch: test gate, then exactly four options — merge, PR, keep, or discard.'
category: workflow
visibility: on_demand
content:
  abstract: |
    The full test suite gates everything. Then detect the environment,
    determine the base branch, and present exactly four options: merge
    locally, push and PR, keep as-is, discard. Merge re-runs tests on the
    merged result; cleanup only touches .worktrees/-owned trees.
  context: |
    # Finishing a branch

    1. Run the FULL test suite. Failures block the menu entirely — no
       merge/PR talk until green.
    2. Detect the environment: normal checkout / linked worktree / detached
       HEAD (`git rev-parse --git-dir` vs `--git-common-dir`).
    3. Determine the base branch (`git merge-base HEAD main`, or ask).
    4. Present EXACTLY these options, no editorializing:
       1. Merge back into <base> locally
       2. Push and create a PR
       3. Keep the branch as-is (user handles it)
       4. Discard this work
       (Detached HEAD: drop option 1.)

    ## Option mechanics

    - Merge: checkout base → pull → merge → RE-RUN the suite on the merged
      result → only then clean up the worktree → delete the branch. That
      order: the branch cannot be deleted while its worktree exists.
    - PR: push -u. NEVER remove the worktree — it is needed to iterate on
      review feedback.
    - Discard: require the user to type "discard"; list the branch, commit
      count, and worktree that will be destroyed.

    ## Worktree cleanup provenance

    Only remove worktrees under `.worktrees/` or `worktrees/`. cd OUT of the
    worktree before `git worktree remove`; run `git worktree prune` after.
    Never force-push unless explicitly asked.
tags:
- git
- finishing
triggers:
- type: keyword
  pattern: 'finish(ing)? (the |this )?(branch|work)|wrap up|merge (this|the) branch|收尾|合併分支|開 PR'
- type: manual
```

- [ ] **Step 3: Create `mur-core/src/skills/mur_merge_conflicts.yaml`**

```yaml
name: mur-merge-conflicts
version: 1.0.0
publisher: human:mur-official
description: 'Resolve an in-progress merge or rebase hunk by hunk from primary sources. Never abort, never invent.'
category: workflow
visibility: on_demand
content:
  abstract: |
    For each conflicting hunk, find why both sides changed (commits, PRs,
    tickets), preserve both intents where possible, pick the merge-goal side
    when incompatible and note the trade-off. Never invent new behavior,
    never abort. Run checks, then finish.
  context: |
    # Merge conflicts — resolve by intent

    1. See the state: `git status`, the conflicting files, and both sides'
       history around them.
    2. Per hunk, find PRIMARY sources for both sides' intent: commit
       messages, PRs, linked tickets.
       *Why: the hunk shows WHAT changed; only the sources say WHY.*
    3. Resolve: preserve both intents where possible. Incompatible → pick the
       side matching the merge's stated goal and note the trade-off in the
       commit message. NEVER invent behavior neither side had. NEVER
       `--abort` — resolve.
    4. Run the project's checks: typecheck → tests → format. Fix what the
       merge broke.
    5. Finish: stage and commit; if rebasing, continue until every commit is
       replayed.
tags:
- git
triggers:
- type: keyword
  pattern: 'merge conflict|rebase conflict|conflict(ed|ing) (file|hunk)|衝突|解衝突'
- type: manual
```

- [ ] **Step 4: Verify GREEN** (batch-cycle step 4 command)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/skills/mur_finishing_branch.yaml mur-core/src/skills/mur_merge_conflicts.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-finishing-branch, mur-merge-conflicts builtins

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: Meta batch — `mur-skill-authoring`

**Files:**
- Create: `mur-core/src/skills/mur_skill_authoring.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (1 registry tuple; 1 budget row `true`; 1 trigger-test line)

**Interfaces:**
- Consumes: batch cycle.
- Produces: name `mur-skill-authoring`. Roster complete: all 17 of `NEW_DEV_SKILL_NAMES` (Task 4) now exist — the sets must match exactly.

- [ ] **Step 1: Registry + tests (RED)** — batch cycle steps 1–2.

- [ ] **Step 2: Create `mur-core/src/skills/mur_skill_authoring.yaml`**

```yaml
name: mur-skill-authoring
version: 1.0.0
publisher: human:mur-official
description: 'Author or edit MUR skills that agents actually follow: trigger-only descriptions, budgets, matched forms.'
category: meta
visibility: on_demand
content:
  abstract: |
    Skills are engineered against observed failures: descriptions carry
    triggers only, form matches the failure mode, iron laws pair with
    rationalization tables, leading words recruit priors, every line must
    beat the no-guidance default, budgets force curation.
  context: |
    # Skill authoring — write skills agents follow

    ## When to write one

    A non-obvious, cross-project technique. NOT one-off fixes, NOT anything a
    linter or hook can enforce (automate those instead), NOT project
    conventions.

    ## MUR budgets (test-enforced)

    description 120 chars max; abstract 50 words max; body 150 lines max.
    Over budget → split into a hub plus on-demand leaves
    (`visibility: on_demand`).

    ## Description = triggers ONLY

    "Use when <symptoms/keywords>". NEVER summarize the workflow in the
    description — agents execute the summary and skip the body (an observed,
    tested regression). Cover symptoms, error text, synonyms. Bilingual
    (English + zh-TW) keyword regexes are house style.

    ## Match the form to the failure

    - Rule skipped under pressure → prohibition + iron law + rationalization
      table (real excuses verbatim, each with its counter) + red-flag list.
    - Wrong-shaped output → positive recipe or contract.
      *Why: prohibitions measurably backfire on output shaping.*
    - Omitted element → a REQUIRED structural slot in a template.
    - Conditional behavior → a condition on an OBSERVABLE predicate ("when
      `mur agent list` shows a running agent"), never "unless it matters".
    No nuance clauses; no exemption clauses — they reopen negotiation.

    ## Compliance toolkit

    Iron law in a code fence; "violating the letter is violating the
    spirit"; delete-means-delete loophole enumeration; an announce-usage
    requirement.

    ## Language

    Leading words — compact, prior-recruiting concepts (tight, red, tracer
    bullet, frontier) used consistently. Prune no-ops sentence by sentence:
    "be thorough" changes nothing; the fix is a stronger word, not more
    prose.

    ## Test before ship

    Fresh-context micro-test with a NO-GUIDANCE control (if the control does
    not fail, do not write the guidance); read every flagged match manually;
    high variance across runs = the wording is not binding.

    ## MUR tie-in

    Harvested proposals (`mur session out`) are curated to this standard;
    LLM-provenance skills stay gated until a human curates them.
tags:
- meta
- authoring
triggers:
- type: keyword
  pattern: '(write|create|edit|improve) (a |the |this )?skill|skill (yaml|manifest)|寫技能|建技能|技能'
- type: manual
```

- [ ] **Step 3: Verify GREEN + roster completeness**

Run the batch-cycle step 4 command, then confirm the roster matches Task 4's const exactly:

Run: `rg -o '"mur-[a-z-]+"' mur-core/src/cmd/sync_cmd.rs | sort -u | rg -c 'mur-(dev|grilling|brainstorm|domain-modeling|writing-plans|tickets|executing-plans|delegate-dev|worktree|tdd|debugging|code-review|receiving-review|verification|finishing-branch|merge-conflicts|skill-authoring)"'`
Expected: `17`

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/skills/mur_skill_authoring.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): mur-skill-authoring builtin (dev-discipline roster complete)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: README section

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the final roster (Task 12).
- Produces: nothing (docs only).

- [ ] **Step 1: Locate the insertion point**

Run: `rg -n "^## |^### " README.md | head -40` and pick the section that documents built-in skills or capabilities (whichever heading exists — e.g. a Skills/Capabilities/Features section). Insert the block below at the end of that section.

- [ ] **Step 2: Insert this block**

````markdown
### Dev-discipline skills (built-in)

MUR ships a curated engineering-discipline pack — internalized from the
MIT-licensed [obra/superpowers](https://github.com/obra/superpowers) and
[mattpocock/skills](https://github.com/mattpocock/skills) (see
`docs/ATTRIBUTIONS.md`), merged and adapted to MUR's runtime (no sub-agents
required; delegation-aware). One hub routes; sixteen on-demand leaves carry
the method:

`mur-dev` (hub) · `mur-grilling` · `mur-brainstorm` · `mur-domain-modeling` ·
`mur-writing-plans` · `mur-tickets` · `mur-executing-plans` ·
`mur-delegate-dev` · `mur-worktree` · `mur-tdd` · `mur-debugging` ·
`mur-code-review` · `mur-receiving-review` · `mur-verification` ·
`mur-finishing-branch` · `mur-merge-conflicts` · `mur-skill-authoring`

- Zero token cost until used: only the hub appears in the session-start
  learning index; leaves load on demand (`mur skill show mur-tdd`).
- Never-shadow: with the superpowers plugin installed, the hub hides itself
  on the CLI surface (`skills.dev_discipline_index: auto|always|never` in
  `~/.mur/config.yaml`); a user-authored skill with the same name is never
  overwritten.
````

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): dev-discipline builtin skills section

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 14: Full verification + PR

**Files:**
- No new files. Runs the whole gate and opens the PR.

**Interfaces:**
- Consumes: everything above.
- Produces: the PR.

- [ ] **Step 1: Format + lint**

```bash
export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo fmt --all
cargo clippy --workspace -- -D warnings
```
Expected: fmt makes no changes (or stage them); clippy exits 0.

- [ ] **Step 2: Full test run**

```bash
RUST_MIN_STACK=33554432 ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-common -p mur-core
```
Expected: all tests pass (the `RUST_MIN_STACK` lift is for pre-existing mur-core bin CLI-parse tests, not this change).

- [ ] **Step 3: Commit any fmt fallout, push, open PR**

```bash
git branch --show-current   # must print feat/builtin-dev-discipline-skills
git push -u origin feat/builtin-dev-discipline-skills
gh pr create --title "feat(skills): builtin dev-discipline pack (superpowers + mattpocock internalized)" --body "$(cat <<'EOF'
## Summary
- 17 built-in dev-discipline skills (1 hub + 16 on-demand leaves) internalized from obra/superpowers and mattpocock/skills (both MIT; notices in docs/ATTRIBUTIONS.md)
- Overlapping territory (TDD / debugging / review / planning / questioning) merged into one canon per topic; no-subagent adaptation via the execution-backend ladder (in-context default, MUR delegation on observable predicate)
- Never-shadow installs: publisher-based skip for user-authored same-name skills
- Superpowers-aware CLI surface: `skills.dev_discipline_index: auto|always|never`; detection hides the `mur-dev` hub from the learning index when the superpowers plugin is installed
- Spec: docs/superpowers/specs/2026-07-23-builtin-dev-discipline-skills-design.md

## Test plan
- [ ] `builtin_skill_tests` budget/visibility suite covers all 17 YAMLs
- [ ] `dev_skill_keyword_triggers_compile` (regex validity)
- [ ] `never_shadow_tests` (user skill survives, unparseable = skip, publisher rules)
- [ ] `superpowers_detection_and_suppression_matrix`
- [ ] `cargo clippy --workspace -- -D warnings` + `cargo fmt --check`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Follow-ups (explicitly out of this plan)

- app.mur.run Documents/Product pages (`mur-server` repo) — run the `update-docs` skill after merge.
- Dogfood in murmur: confirm the hub injects at session start for an agent and keyword triggers route to leaves; tune hub trigger regex from observations.
- Phase 2 candidates per spec §8: wayfinder, prototype, handoff, teach, improve-codebase-architecture, `engineering` capability bundle, behavioral eval harness.
