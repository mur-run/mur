# Skill `Retrievable` Impl + Candidate Loader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Skill` retrievable through the generic scorer introduced in `2026-05-29-retrievable-trait-extraction.md` by adding `LoadedSkill` (manifest + stats wrapper), `impl Retrievable for LoadedSkill`, a corpus-wide skill candidate loader, and a thin public generic entry point so callers can score `Vec<LoadedSkill>` end-to-end.

**Architecture:** A new file `mur-core/src/retrieve/skill_candidates.rs` owns `LoadedSkill { manifest: SkillManifest, stats: SkillStats }`, its `Retrievable` impl, and `load_skill_candidates(skills_dir: &Path) -> Vec<LoadedSkill>`. The impl bridges existing skill data to the trait: `Priority` maps to `Tier` (Critical→Core, High/Normal→Project, Low→Session) and to `importance` (1.0 / 0.8 / 0.5 / 0.3); `SkillStats` supplies effectiveness, last activity, and active/inactive. Text comes from `content.abstract + description` (the embed/keyword surface for skills today). A new `pub fn score_and_rank_generic<T: Retrievable>` is added to `scoring.rs` so the generic pipeline is callable from outside the test module — this is what makes the plan "working software" rather than just a building block.

**Tech Stack:** Rust 2024 edition, cargo workspace, existing `mur-common::skill` types (`SkillManifest`, `SkillStats`, `parse_canonical`), `mur-core::retrieve::scoring` (the `Retrievable` trait, `Scored<T>`, `score_and_rank_inner`). No new dependencies.

**Out of scope (sequenced to later plans):** `Note` variant on `Category`/`ContentMode`; per-skill `events.jsonl` + reducer + `mur skill evolve` sweep; `mur skill search` CLI surface; Pattern removal. This plan only proves the trait works for a second consumer and gives downstream plans a public entry point to call.

**Depends on:** `2026-05-29-retrievable-trait-extraction.md` must be merged first. This plan calls `Retrievable`, `Scored<T>`, and `score_and_rank_inner` defined there.

---

## Design decisions (locked in before writing tasks)

1. **`LoadedSkill` lives in `mur-core`, not `mur-common`.** It bundles a manifest with stats, which is a retrieval-side concern. Promote to `mur-common` only if a second crate needs it (YAGNI).
2. **Priority → Tier mapping** (not adding a new `tier` field to `SkillManifest`):

   | Priority | Tier | Importance |
   |---|---|---|
   | Critical | Core | 1.0 |
   | High | Project | 0.8 |
   | Normal | Project | 0.5 |
   | Low | Session | 0.3 |

   Rationale: preserves Priority semantics in the decay/recency pipeline at zero schema cost. A `Tier` field on `SkillManifest` can be added in a later schema bump if finer control is needed.
3. **`created_at` fallback.** `SkillManifest` has no `created_at` field today (verified). For the `Retrievable::created_at` accessor (used as the last-activity fallback) we return `stats.first_successful_use_at.unwrap_or(stats.lifecycle_changed_at)`. Limitation: for a freshly created skill that has never run, this is "stats file creation time" (set in `SkillStats::new`), close enough to "first seen" for retrieval scoring. Adding `created_at: DateTime<Utc>` to `SkillManifest` is a separate, optional schema bump.
4. **Embed/keyword text.** For pre-Note skills, the surface is `content.abstract` + `description`. Once `ContentMode::Note` (separate plan) lands, the Note's body joins the surface. For workflow procedures and context bodies, this plan ships with abstract-only as the conservative default; downstream plans extend.
5. **Effectiveness formula matches `Pattern`'s (and what `lifecycle::next_state` already uses):** `success_count as f64 / usage_count as f64` if `usage_count > 0`, else `0.0`.
6. **Active filter:** `!matches!(stats.lifecycle_state, LifecycleState::Deprecated | LifecycleState::Archived) && !stats.pinned_reason.contains("muted")`. Pinned skills are always retained regardless of state; the analog of `Pattern.lifecycle.muted` does not exist on `SkillStats`, so we use lifecycle state alone for now.
7. **Loader directory layout (route 1 from the spec, agreed):** one directory per skill under `~/.mur/skills/<name>/`, each containing `skill.yaml`. Stats live where `SkillStats::path(mur_home, skill_name)` returns, regardless of subdir layout.

---

## File map

- **Create:** `mur-core/src/retrieve/skill_candidates.rs` — `LoadedSkill`, `impl Retrievable for LoadedSkill`, `load_skill_candidates`.
- **Modify:** `mur-core/src/retrieve/mod.rs` — register and re-export the new module.
- **Modify:** `mur-core/src/retrieve/scoring.rs` — add `pub fn score_and_rank_generic<T: Retrievable>`.

No other files change. No `Cargo.toml` edits.

---

## Task 1: `LoadedSkill` + `Retrievable` impl

**Files:**
- Create: `mur-core/src/retrieve/skill_candidates.rs`

- [ ] **Step 1: Write the failing test**

Create the file with the test module first (the test will fail to compile because the type doesn't exist yet):

```rust
//! Loaded skills (manifest + stats) and their `Retrievable` impl, so the
//! generic scorer in `super::scoring` can rank skills.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::pattern::Tier;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::types::Priority;

use super::scoring::{Retrievable, ScopeContext};

/// A skill loaded together with its runtime stats. The retrieval pipeline
/// scores `Vec<LoadedSkill>` through the generic `score_and_rank_inner`.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub stats: SkillStats,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mur_common::skill::manifest::Content;
    use mur_common::skill::types::Category;

    fn fake_loaded(name: &str, priority: Priority) -> LoadedSkill {
        let manifest = SkillManifest {
            name: name.into(),
            version: "1.0.0".into(),
            publisher: "human:test".into(),
            description: format!("desc for {name}"),
            category: Category::Context,
            hosts: vec![],
            content: Content {
                r#abstract: format!("abstract about {name}"),
                context: Some(format!("body of {name}")),
                procedure: None,
                command: None,
            },
            requires: vec![],
            tags: vec!["alpha".into(), "beta".into()],
            triggers: vec![],
            priority,
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
        };
        let mut stats = SkillStats::new(name, "1.0.0", "", Utc::now() - Duration::days(2));
        stats.usage_count = 4;
        stats.success_count = 3;
        stats.last_success_at = Some(Utc::now() - Duration::days(1));
        stats.first_successful_use_at = Some(Utc::now() - Duration::days(2));
        LoadedSkill { manifest, stats }
    }

    #[test]
    fn retrievable_accessors_reflect_manifest_and_stats() {
        let s = fake_loaded("alpha-skill", Priority::High);
        assert_eq!(s.name(), "alpha-skill");
        assert_eq!(s.description(), "desc for alpha-skill");
        assert_eq!(&*s.text(), "abstract about alpha-skill\ndesc for alpha-skill");
        assert_eq!(s.tag_terms(), vec!["alpha", "beta"]);
        assert_eq!(s.importance(), 0.8);
        assert_eq!(s.tier(), Tier::Project);
        assert!((s.effectiveness() - 0.75).abs() < 1e-9);
        assert!(s.is_active());
        assert_eq!(s.decay_half_life_days(), Tier::Project.decay_half_life_days() as f64);
        assert_eq!(s.last_activity(), s.stats.last_success_at);
    }

    #[test]
    fn priority_critical_maps_to_core_tier_and_importance_one() {
        let s = fake_loaded("k", Priority::Critical);
        assert_eq!(s.tier(), Tier::Core);
        assert_eq!(s.importance(), 1.0);
    }

    #[test]
    fn priority_low_maps_to_session_tier_and_importance_zero_three() {
        let s = fake_loaded("k", Priority::Low);
        assert_eq!(s.tier(), Tier::Session);
        assert!((s.importance() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn effectiveness_is_zero_when_usage_count_is_zero() {
        let mut s = fake_loaded("k", Priority::Normal);
        s.stats.usage_count = 0;
        s.stats.success_count = 0;
        assert_eq!(s.effectiveness(), 0.0);
    }

    #[test]
    fn deprecated_skill_is_not_active() {
        let mut s = fake_loaded("k", Priority::Normal);
        s.stats.lifecycle_state = LifecycleState::Deprecated;
        assert!(!s.is_active());
    }

    #[test]
    fn archived_skill_is_not_active() {
        let mut s = fake_loaded("k", Priority::Normal);
        s.stats.lifecycle_state = LifecycleState::Archived;
        assert!(!s.is_active());
    }

    #[test]
    fn adjust_score_is_identity_for_skills() {
        let s = fake_loaded("k", Priority::Normal);
        let scope = ScopeContext::default();
        assert_eq!(s.adjust_score(0.42, &["q"], Some(&scope), Some("rust")), 0.42);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests::retrievable_accessors_reflect_manifest_and_stats`
Expected: COMPILE ERROR — `Retrievable` is not implemented for `LoadedSkill`.

(Before the test can even run, the `Retrievable` impl must compile. The next step adds it.)

- [ ] **Step 3: Implement `Retrievable for LoadedSkill`**

Insert in `mur-core/src/retrieve/skill_candidates.rs`, before the `#[cfg(test)]` block:

```rust
fn priority_to_tier(p: Priority) -> Tier {
    match p {
        Priority::Critical => Tier::Core,
        Priority::High | Priority::Normal => Tier::Project,
        Priority::Low => Tier::Session,
    }
}

fn priority_to_importance(p: Priority) -> f64 {
    match p {
        Priority::Critical => 1.0,
        Priority::High => 0.8,
        Priority::Normal => 0.5,
        Priority::Low => 0.3,
    }
}

impl Retrievable for LoadedSkill {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn text(&self) -> std::borrow::Cow<'_, str> {
        // Pre-Note skills: abstract + description is the keyword/embed surface.
        // Extended once ContentMode::Note (separate plan) lands.
        std::borrow::Cow::Owned(format!(
            "{}\n{}",
            self.manifest.content.r#abstract, self.manifest.description
        ))
    }

    fn tag_terms(&self) -> Vec<&str> {
        self.manifest.tags.iter().map(String::as_str).collect()
    }

    fn importance(&self) -> f64 {
        priority_to_importance(self.manifest.priority)
    }

    fn effectiveness(&self) -> f64 {
        if self.stats.usage_count == 0 {
            0.0
        } else {
            self.stats.success_count as f64 / self.stats.usage_count as f64
        }
    }

    fn tier(&self) -> Tier {
        priority_to_tier(self.manifest.priority)
    }

    fn created_at(&self) -> DateTime<Utc> {
        // Manifest has no created_at; use the earliest stats anchor we have.
        self.stats
            .first_successful_use_at
            .unwrap_or(self.stats.lifecycle_changed_at)
    }

    fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.stats.last_success_at
    }

    fn decay_half_life_days(&self) -> f64 {
        self.tier().decay_half_life_days() as f64
    }

    fn is_active(&self) -> bool {
        !matches!(
            self.stats.lifecycle_state,
            LifecycleState::Deprecated | LifecycleState::Archived
        )
    }

    // adjust_score uses the trait default (identity) — skills have no
    // Pattern-specific scope/lang/kind boosts.
}
```

- [ ] **Step 4: Make the module visible so the test runs**

In `mur-core/src/retrieve/mod.rs`, append:

```rust
pub mod skill_candidates;
```

- [ ] **Step 5: Run the test module to verify it passes**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests`
Expected: all 7 tests in the module PASS.

- [ ] **Step 6: Run the full retrieve test suite to confirm no regression**

Run: `cargo test -p mur-core retrieve::`
Expected: existing scoring tests still PASS.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/retrieve/skill_candidates.rs mur-core/src/retrieve/mod.rs
git commit -m "feat(retrieve): impl Retrievable for LoadedSkill (manifest + stats)

Priority maps to Tier (Critical=Core, High/Normal=Project, Low=Session) and
to importance (1.0/0.8/0.5/0.3). Effectiveness = success/usage. Stats drive
last_activity and is_active. adjust_score uses the identity default."
```

---

## Task 2: `load_skill_candidates` corpus scanner

**Files:**
- Modify: `mur-core/src/retrieve/skill_candidates.rs` — add the loader function and its tests.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `mur-core/src/retrieve/skill_candidates.rs`:

```rust
#[test]
fn load_skill_candidates_reads_two_well_formed_skills() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    let mur_home = tmp.path();

    // Write two well-formed skill directories.
    for name in ["alpha", "beta"] {
        let dir = skills_dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:test\n\
             description: desc for {name}\ncategory: context\n\
             content:\n  abstract: a\n  context: c\n"
        );
        fs::write(dir.join("skill.yaml"), yaml).unwrap();
    }

    let loaded = load_skill_candidates(&skills_dir, mur_home).unwrap();
    let names: Vec<_> = loaded.iter().map(|s| s.name().to_string()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
}

#[test]
fn load_skill_candidates_skips_directories_without_skill_yaml() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    fs::create_dir_all(skills_dir.join("not-a-skill")).unwrap();

    let loaded = load_skill_candidates(&skills_dir, tmp.path()).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn load_skill_candidates_skips_malformed_yaml_with_warning() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    fs::create_dir_all(skills_dir.join("broken")).unwrap();
    fs::write(skills_dir.join("broken").join("skill.yaml"), "{ not valid yaml").unwrap();

    // Loader must not propagate the parse error; return Ok(empty).
    let loaded = load_skill_candidates(&skills_dir, tmp.path()).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn load_skill_candidates_returns_empty_if_skills_dir_missing() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let skills_dir = tmp.path().join("does-not-exist");
    let loaded = load_skill_candidates(&skills_dir, tmp.path()).unwrap();
    assert!(loaded.is_empty());
}
```

Note: `tempfile = "3"` is already in `mur-core/Cargo.toml` under `[dev-dependencies]` — no Cargo.toml edit needed for this plan.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests::load_skill_candidates_reads_two_well_formed_skills`
Expected: COMPILE ERROR — `cannot find function 'load_skill_candidates' in this scope`.

- [ ] **Step 3: Implement `load_skill_candidates`**

Insert in `mur-core/src/retrieve/skill_candidates.rs`, after the `Retrievable` impl and before the `#[cfg(test)]` block:

```rust
/// Scan `skills_dir` (typically `<mur_home>/skills/`) for skill directories
/// and return a `LoadedSkill` for each parseable `skill.yaml`.
///
/// Malformed or missing manifests are skipped with a `tracing::warn` so a
/// single bad skill never poisons the corpus. Stats are loaded via
/// `SkillStats::path(mur_home, name)`; if absent, a fresh `SkillStats` is
/// constructed so the skill still scores (with `usage_count = 0`).
pub fn load_skill_candidates(skills_dir: &Path, mur_home: &Path) -> Result<Vec<LoadedSkill>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let yaml_path = path.join("skill.yaml");
        if !yaml_path.is_file() {
            continue;
        }
        let yaml = match std::fs::read_to_string(&yaml_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %yaml_path.display(), error = %e, "read skill.yaml failed");
                continue;
            }
        };
        let manifest = match mur_common::skill::parser::parse_canonical(&yaml) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(path = %yaml_path.display(), error = %e, "parse skill.yaml failed");
                continue;
            }
        };

        let stats_path = SkillStats::path(mur_home, &manifest.name);
        let stats = match SkillStats::load(&stats_path) {
            Ok(Some(s)) => s,
            Ok(None) => SkillStats::new(&manifest.name, &manifest.version, "", Utc::now()),
            Err(e) => {
                tracing::warn!(path = %stats_path.display(), error = %e, "load skill stats failed; using fresh");
                SkillStats::new(&manifest.name, &manifest.version, "", Utc::now())
            }
        };

        out.push(LoadedSkill { manifest, stats });
    }

    Ok(out)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests`
Expected: all 11 tests (7 from Task 1 + 4 here) PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/retrieve/skill_candidates.rs
git commit -m "feat(retrieve): load_skill_candidates scans ~/.mur/skills/

Skips dirs without skill.yaml, skips malformed YAML with tracing::warn,
returns empty Vec for a missing skills dir. Stats loaded via
SkillStats::path; fresh stats fall back when absent."
```

---

## Task 3: Public generic entry `score_and_rank_generic<T: Retrievable>`

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` — add a single public generic function near the other public entry points.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `mur-core/src/retrieve/scoring.rs`:

```rust
#[test]
fn score_and_rank_generic_ranks_pattern_corpus_like_score_and_rank() {
    let p1 = make_pattern("alpha", "alpha body about deploy");
    let p2 = make_pattern("beta", "beta body about something else");
    let generic = score_and_rank_generic("alpha deploy", vec![p1.clone(), p2.clone()]);
    let legacy = score_and_rank("alpha deploy", vec![p1, p2]);
    assert_eq!(generic.len(), legacy.len());
    for (g, l) in generic.iter().zip(legacy.iter()) {
        assert_eq!(g.item.name, l.item.name);
        assert!((g.score - l.score).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::scoring::tests::score_and_rank_generic_ranks_pattern_corpus_like_score_and_rank`
Expected: COMPILE ERROR — `cannot find function 'score_and_rank_generic' in this scope`.

- [ ] **Step 3: Implement `score_and_rank_generic`**

Insert into `mur-core/src/retrieve/scoring.rs` immediately after `score_and_rank` (the existing keyword-only Pattern entry, around line 118):

```rust
/// Public generic entry point: score and rank any `Vec<T>` where
/// `T: Retrievable`. Keyword-only relevance, no scope, no project_language,
/// default scoring config. Mirrors `score_and_rank` for the generic case.
///
/// Hybrid / scope-aware generic entries are added in later plans as needed.
pub fn score_and_rank_generic<T: Retrievable>(
    query: &str,
    candidates: Vec<T>,
) -> Vec<Scored<T>> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    score_and_rank_inner(
        &query_words,
        candidates,
        None,
        None,
        None,
        |words, item: &T| keyword_relevance(words, item),
    )
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p mur-core retrieve::scoring::tests::score_and_rank_generic_ranks_pattern_corpus_like_score_and_rank`
Expected: PASS — generic and Pattern-typed entries produce identical rankings (proof of behavior preservation through the generic layer).

- [ ] **Step 5: Run the full retrieve test suite**

Run: `cargo test -p mur-core retrieve::`
Expected: all tests (existing + new from Task 1 + Task 2 + this) PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "feat(retrieve): public score_and_rank_generic<T: Retrievable>

Mirrors score_and_rank for arbitrary Retrievable types. Lets external
callers invoke the generic pipeline without touching score_and_rank_inner."
```

---

## Task 4: End-to-end integration test (loader → generic scorer → ranked skills)

**Files:**
- Modify: `mur-core/src/retrieve/skill_candidates.rs` — add an integration test that exercises the full chain.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `mur-core/src/retrieve/skill_candidates.rs`:

```rust
#[test]
fn end_to_end_ranks_loaded_skills_via_generic_scorer() {
    use crate::retrieve::scoring::score_and_rank_generic;
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    let mur_home = tmp.path();

    // alpha: matches query "deploy" in abstract and description.
    fs::create_dir_all(skills_dir.join("deploy-fly")).unwrap();
    fs::write(
        skills_dir.join("deploy-fly").join("skill.yaml"),
        "name: deploy-fly\nversion: 1.0.0\npublisher: human:test\n\
         description: deploy to Fly.io\ncategory: context\n\
         priority: high\ntags: [deploy, fly]\n\
         content:\n  abstract: how to deploy a Rust app to Fly.io\n  context: details\n",
    ).unwrap();

    // beta: unrelated keyword content.
    fs::create_dir_all(skills_dir.join("brew-update")).unwrap();
    fs::write(
        skills_dir.join("brew-update").join("skill.yaml"),
        "name: brew-update\nversion: 1.0.0\npublisher: human:test\n\
         description: keep homebrew current\ncategory: context\n\
         priority: normal\ntags: [brew, mac]\n\
         content:\n  abstract: run brew update weekly\n  context: details\n",
    ).unwrap();

    let candidates = load_skill_candidates(&skills_dir, mur_home).unwrap();
    assert_eq!(candidates.len(), 2);

    let ranked = score_and_rank_generic("deploy fly rust", candidates);
    assert!(!ranked.is_empty(), "deploy query should rank at least the deploy-fly skill");
    assert_eq!(ranked[0].item.name(), "deploy-fly");
    // If brew-update made it past the score floor, it must rank below deploy-fly.
    if ranked.len() > 1 {
        assert!(ranked[0].score > ranked[1].score);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests::end_to_end_ranks_loaded_skills_via_generic_scorer`
Expected: PASS. This is the proof that the loader + Retrievable impl + generic scorer chain together correctly.

If the test fails because *both* skills get filtered by the score floor (`SCORE_FLOOR = 0.42` in `scoring.rs`), the test's content does not match strongly enough. Re-check the abstract/description text contains the query words `deploy`, `fly`, `rust` (they do). The Pattern path scores similar content above the floor; the skill path will too, because the weighted-sum formula is identical and the only differences (no scope/lang/kind boosts) are *neutral* multiplications.

- [ ] **Step 3: Run the full retrieve test suite**

Run: `cargo test -p mur-core retrieve::`
Expected: every test PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/retrieve/skill_candidates.rs
git commit -m "test(retrieve): end-to-end loader → generic scorer ranks skills"
```

---

## Task 5: Verification gate — full workspace and lints

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass. The new module adds 12 tests total (7 in Task 1, 4 in Task 2, 1 in Task 4) + 1 in `scoring.rs` (Task 3) = 13 net new tests.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. The new `LoadedSkill` struct and `priority_to_*` helpers should not trigger any warnings. If clippy flags `needless_borrow` or `manual_match` on the helpers, fix to clean output before merging.

- [ ] **Step 3: Run `cargo fmt --check`**

Run: `cargo fmt --check`
Expected: clean. If not:

```bash
cargo fmt
git add -u
git commit --amend --no-edit
```

- [ ] **Step 4: Confirm scope**

Run: `git diff --stat origin/main..HEAD`
Expected: exactly three files appear:
- `mur-core/src/retrieve/skill_candidates.rs` (new)
- `mur-core/src/retrieve/mod.rs` (one `pub mod` line added)
- `mur-core/src/retrieve/scoring.rs` (one public function + one test added)

Anything else means scope creep — review and revert.

- [ ] **Step 5: Final commit if cleanup was needed**

If Steps 2-3 required fixes, the amend above handles it. Otherwise nothing extra to commit.

---

## Done state

After this plan:

- `LoadedSkill { manifest: SkillManifest, stats: SkillStats }` exists and implements `Retrievable` with Priority → Tier / importance mappings and stats-driven activity/effectiveness/decay inputs.
- `load_skill_candidates(skills_dir, mur_home) -> Result<Vec<LoadedSkill>>` scans the on-disk corpus, skipping malformed/missing manifests with `tracing::warn`.
- `pub fn score_and_rank_generic<T: Retrievable>(query, candidates) -> Vec<Scored<T>>` is the public generic entry point — any external caller can rank any `Retrievable` corpus.
- End-to-end integration test proves: temp skill dir → loader → generic scorer → ranked result.
- All previous retrieve tests still green (Pattern path unchanged).
- **Plan 1's `Retrievable` trait has been validated against a real second consumer** — the trait design holds (no accessor gaps surfaced; the `adjust_score` identity default is correct for non-Pattern items).

**What this unlocks (next plans):**

- **D — Pattern removal**: with Skills now retrievable, the inject path can switch from `Vec<Pattern>` to `Vec<LoadedSkill>` and Pattern can be carved out.
- **Notes N3 — `impl Retrievable for Note`**: trivial once `ContentMode::Note` exists; copies the structure here.
- **Workflow ranking surfaces** (`mur skill search` CLI, MCP `skill_search`): can call `score_and_rank_generic` directly.

**What this does NOT unlock yet (still gated on other plans):**

- Hybrid (vector) scoring for skills — needs a corpus-wide LanceDB index covering skills. Separate plan.
- Lifecycle wiring driven by retrieval events — needs the `events.jsonl` reducer (Plan B).
- Note-mode text in the embed/keyword surface — needs `ContentMode::Note` (Plan A).
