# Skill Provenance + Curation Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make LLM-extracted skills structurally unable to auto-promote past `Emerging` until a human has curated them, implementing amendment **A1** of `2026-05-28-mur-workflow-engine-design-v2.md`.

**Architecture:** A new `Provenance { Human, Llm, Hybrid }` field on `SkillManifest` records a skill's origin. A `mur.skill.curated` trace event (emitted by `mur skill curate`) is reduced into a `curated_at` timestamp on `SkillStats`. A pure `cap_for_provenance()` function caps the lifecycle state proposed by `next_state()` at `Emerging` when a skill is `Llm`-authored and not yet curated; the lifecycle sweep applies this cap before persisting any transition. The gate is config-toggled (`require_human_curation_before_stable`, default `true`).

**Tech Stack:** Rust (edition 2024), `serde`/`serde_json`, `chrono`, `anyhow`, `tokio` (async reindex), `tempfile` (tests). Crates: `mur-common` (types/lifecycle/stats/config), `mur-core` (reducer/sweep/CLI).

**Why this design:** `next_state(stats, now)` is a PURE function over stats only (see `lifecycle.rs:72`). Provenance lives on the manifest, not stats — so the cap is a *separate* pure function applied by the sweep (which already loads both), keeping `next_state` untouched and independently testable. SkillsBench (arXiv 2604.01687) measured LLM-authored skills at +0.0pp vs +16.2pp for human-curated; this gate turns that finding into a structural invariant.

---

## File Structure

**Modify:**
- `mur-common/src/skill/types.rs` — add `Provenance` enum (sibling of `Category`).
- `mur-common/src/skill/manifest.rs:36` — add `provenance` field to `SkillManifest`.
- `mur-common/src/skill/stats.rs` — add `curated_at` field + init in `new()`.
- `mur-common/src/skill/lifecycle.rs` — add pure `cap_for_provenance()`.
- `mur-common/src/config.rs:1240` — add `require_human_curation_before_stable` to `SkillsConfig`.
- `mur-common/src/telemetry.rs` — add `METHOD_SKILL_CURATED` const.
- `mur-core/src/skill_stats/reindex.rs` — reduce curated events → `curated_at`.
- `mur-core/src/skill_lifecycle/sweep.rs` — load provenance, apply cap; new `SweepOptions` field.
- `mur-core/src/cmd/skill_sweep.rs` — wire config → `SweepOptions`.
- `mur-core/src/cli/skill.rs` — add `Curate { name }` action.
- `mur-core/src/dispatch.rs:442` — dispatch the new action.

**Create:**
- `mur-core/src/cmd/skill_curate.rs` — `cmd_curate` + `record_curation` (emits the trace event).

---

## Task 1: Add `Provenance` enum + manifest field

**Files:**
- Modify: `mur-common/src/skill/types.rs` (after the `Category` enum, ~line 41)
- Modify: `mur-common/src/skill/manifest.rs:36-77` (add field to `SkillManifest`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `mur-common/src/skill/types.rs`:

```rust
    #[test]
    fn provenance_defaults_to_human_and_roundtrips() {
        // Default is Human (a skill is human-authored unless stated otherwise).
        assert_eq!(Provenance::default(), Provenance::Human);
        // Serializes lowercase, like Category.
        let yaml = serde_yaml_ng::to_string(&Provenance::Llm).unwrap();
        assert_eq!(yaml.trim(), "llm");
        let parsed: Provenance = serde_yaml_ng::from_str("hybrid").unwrap();
        assert_eq!(parsed, Provenance::Hybrid);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common provenance_defaults_to_human_and_roundtrips`
Expected: FAIL — `cannot find type Provenance in this scope`.

- [ ] **Step 3: Add the enum**

Insert in `mur-common/src/skill/types.rs` immediately after the closing `}` of the `Category` enum (after line 41):

```rust
/// Where a skill came from. Drives the curation gate: `Llm`-authored skills
/// cannot auto-promote past `Emerging` until a human curates them
/// (amendment A1, `2026-05-28-mur-workflow-engine-design-v2.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Hand-authored by a person. Default — no gate.
    #[default]
    Human,
    /// Produced by the LLM extraction judge. Gated until curated.
    Llm,
    /// LLM-extracted, then human-reviewed/edited. No gate.
    Hybrid,
}
```

- [ ] **Step 4: Add the manifest field**

In `mur-common/src/skill/manifest.rs`, extend the `use super::types::...` import (line 5) to include `Provenance`:

```rust
use super::types::{Category, ContentMode, HostId, Priority, Provenance, TriggerKind, TrustLevel};
```

Then add this field inside `pub struct SkillManifest { … }` (after `pub category: Category,` at line 41):

```rust
    /// Origin of this skill. Defaults to `Human` so every existing manifest
    /// (which has no `provenance:` key) parses as human-authored.
    #[serde(default)]
    pub provenance: Provenance,
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p mur-common provenance_defaults_to_human_and_roundtrips`
Expected: PASS.

- [ ] **Step 6: Verify existing manifests still parse**

Run: `cargo test -p mur-common skill::`
Expected: PASS (all existing manifest/serde tests green — `#[serde(default)]` means old YAML without `provenance:` is unaffected).

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/skill/types.rs mur-common/src/skill/manifest.rs
git commit -m "feat(skill): add Provenance enum + manifest field (A1)"
```

---

## Task 2: Add `curated_at` to `SkillStats`

**Files:**
- Modify: `mur-common/src/skill/stats.rs:44-112`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `mur-common/src/skill/stats.rs`:

```rust
    #[test]
    fn curated_at_defaults_to_none_and_is_backward_compatible() {
        // A SkillStats JSON written before this field existed must still parse.
        let legacy = r#"{
            "schema_version": 1, "skill_name": "x", "skill_version": "1",
            "manifest_digest": "d", "lifecycle_state": "draft",
            "lifecycle_changed_at": "2026-01-01T00:00:00Z", "pinned": false,
            "usage_count": 0, "success_count": 0, "failure_count": 0,
            "anchor_confidence": 1.0
        }"#;
        let s: SkillStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.curated_at, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common curated_at_defaults_to_none_and_is_backward_compatible`
Expected: FAIL — `no field curated_at on type SkillStats`.

- [ ] **Step 3: Add the field**

In `mur-common/src/skill/stats.rs`, add inside `pub struct SkillStats { … }` immediately after the `resolution_misses` field (line 83):

```rust
    /// Timestamp of the most recent human curation event
    /// (`mur.skill.curated`). `None` until a human has reviewed an
    /// LLM-extracted skill. Opens the provenance gate (see
    /// `lifecycle::cap_for_provenance`). `#[serde(default)]` keeps older
    /// stats files parsing.
    #[serde(default)]
    pub curated_at: Option<DateTime<Utc>>,
```

- [ ] **Step 4: Initialise it in `new()`**

In the same file, add to the struct literal returned by `SkillStats::new` (after `resolution_misses: 0,` at line 110):

```rust
            curated_at: None,
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p mur-common curated_at_defaults_to_none_and_is_backward_compatible`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/skill/stats.rs
git commit -m "feat(skill): add curated_at to SkillStats (A1)"
```

---

## Task 3: Pure `cap_for_provenance()` in lifecycle

**Files:**
- Modify: `mur-common/src/skill/lifecycle.rs` (import + new fn + tests)

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `mur-common/src/skill/lifecycle.rs`:

```rust
    #[test]
    fn cap_blocks_llm_uncurated_above_emerging() {
        // Stable proposed, LLM, not curated, gate on → capped to Emerging.
        assert_eq!(
            cap_for_provenance(LifecycleState::Stable, Provenance::Llm, false, true),
            LifecycleState::Emerging
        );
        // Canonical likewise capped.
        assert_eq!(
            cap_for_provenance(LifecycleState::Canonical, Provenance::Llm, false, true),
            LifecycleState::Emerging
        );
    }

    #[test]
    fn cap_is_noop_for_human_curated_or_disabled() {
        // Human authorship → never gated.
        assert_eq!(
            cap_for_provenance(LifecycleState::Stable, Provenance::Human, false, true),
            LifecycleState::Stable
        );
        // LLM but curated → gate open.
        assert_eq!(
            cap_for_provenance(LifecycleState::Stable, Provenance::Llm, true, true),
            LifecycleState::Stable
        );
        // Gate disabled by config → no cap.
        assert_eq!(
            cap_for_provenance(LifecycleState::Canonical, Provenance::Llm, false, false),
            LifecycleState::Canonical
        );
        // At or below Emerging → unchanged even when gated.
        assert_eq!(
            cap_for_provenance(LifecycleState::Draft, Provenance::Llm, false, true),
            LifecycleState::Draft
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-common cap_`
Expected: FAIL — `cannot find function cap_for_provenance` and `cannot find type Provenance`.

- [ ] **Step 3: Add the import**

In `mur-common/src/skill/lifecycle.rs`, after the existing `use crate::skill::stats::{LifecycleState, SkillStats};` (line 7) add:

```rust
use crate::skill::types::Provenance;
```

- [ ] **Step 4: Implement the function**

Add after the `next_state` function (after its closing `}` at line 133) in `mur-common/src/skill/lifecycle.rs`:

```rust
/// Cap a proposed lifecycle state for LLM-authored, uncurated skills.
///
/// PURE. The promotion ladder (`next_state`) is provenance-blind; this
/// applies the A1 curation gate on top: an `Llm` skill that no human has
/// curated cannot rise above `Emerging`, no matter how good its run stats
/// look. `Human`/`Hybrid` skills, curated skills, and a disabled gate all
/// pass `proposed` through unchanged. States at or below `Emerging` are
/// never raised.
pub fn cap_for_provenance(
    proposed: LifecycleState,
    provenance: Provenance,
    curated: bool,
    gate_enabled: bool,
) -> LifecycleState {
    let gated = gate_enabled && provenance == Provenance::Llm && !curated;
    if gated && rank(proposed) > rank(LifecycleState::Emerging) {
        LifecycleState::Emerging
    } else {
        proposed
    }
}
```

(`rank` is the private fn already defined in this module at line 160 — reuse it, do not redefine.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-common cap_`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/skill/lifecycle.rs
git commit -m "feat(skill): pure cap_for_provenance gate (A1)"
```

---

## Task 4: Config flag `require_human_curation_before_stable`

**Files:**
- Modify: `mur-common/src/config.rs:1240-1256`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `mur-common/src/config.rs`:

```rust
    #[test]
    fn skills_config_curation_gate_defaults_on() {
        let c = SkillsConfig::default();
        assert!(c.require_human_curation_before_stable);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common skills_config_curation_gate_defaults_on`
Expected: FAIL — `no field require_human_curation_before_stable on type SkillsConfig`.

- [ ] **Step 3: Add the field**

In `mur-common/src/config.rs`, add to `pub struct SkillsConfig { … }` (after `pub adaptive: Option<AdaptiveSkillsConfig>,` at line 1244):

```rust
    /// When true (default), LLM-authored skills cannot auto-promote past
    /// `Emerging` until a human curates them (amendment A1). Set false to
    /// let LLM-extracted skills promote on run stats alone.
    #[serde(default = "default_require_human_curation")]
    pub require_human_curation_before_stable: bool,
```

- [ ] **Step 4: Add the default fn + Default-impl line**

In `mur-common/src/config.rs`, add this free function immediately above `impl Default for SkillsConfig` (before line 1247):

```rust
fn default_require_human_curation() -> bool {
    true
}
```

Then add to the struct literal in `impl Default for SkillsConfig` (after `adaptive: Some(AdaptiveSkillsConfig::default()),` at line 1253):

```rust
            require_human_curation_before_stable: default_require_human_curation(),
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p mur-common skills_config_curation_gate_defaults_on`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): require_human_curation_before_stable flag (A1)"
```

---

## Task 5: Telemetry constant for the curation event

**Files:**
- Modify: `mur-common/src/telemetry.rs:65`

- [ ] **Step 1: Add the constant**

In `mur-common/src/telemetry.rs`, immediately after `pub const METHOD_NOTE_RETRIEVED: &str = "mur.note.retrieved";` (line 65) add:

```rust
/// Emitted by `mur skill curate` — a human reviewed an LLM-extracted skill.
/// Reduced into `SkillStats::curated_at`; opens the A1 provenance gate.
pub const METHOD_SKILL_CURATED: &str = "mur.skill.curated";
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p mur-common`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
git add mur-common/src/telemetry.rs
git commit -m "feat(telemetry): METHOD_SKILL_CURATED constant (A1)"
```

---

## Task 6: Reducer maps curated events → `curated_at`

**Files:**
- Modify: `mur-core/src/skill_stats/reindex.rs:96-171`

The reducer currently processes only lines containing `mur.skill.executed` or `mur.note.retrieved` and treats each as a usage. A curated event is **not** a usage — it must set `curated_at` and not touch `usage_count`/`success_count`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `mur-core/src/skill_stats/reindex.rs` (mirror the structure of the existing `reindex_counts_note_retrieval_events_as_usage_and_success` test at line 259):

```rust
    #[tokio::test]
    async fn reindex_sets_curated_at_without_counting_usage() {
        use mur_common::skill::stats::SkillStats;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Minimal installed skill dir so reindex enumerates it.
        std::fs::create_dir_all(home.join("skills").join("my-skill")).unwrap();
        std::fs::write(
            home.join("skills").join("my-skill").join("skill.yaml"),
            "name: my-skill\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: note\nprovenance: llm\ncontent:\n  abstract: a\n  note: \"b\"\n",
        )
        .unwrap();

        // One curated event in today's trace log.
        let today = chrono::Utc::now();
        let traces = home.join("traces");
        std::fs::create_dir_all(&traces).unwrap();
        let line = format!(
            "{{\"ts\":\"{}\",\"method\":\"mur.skill.curated\",\"mur.skill.name\":\"my-skill\"}}",
            today.to_rfc3339()
        );
        std::fs::write(
            traces.join(today.format("%Y-%m-%d").to_string()).with_extension("jsonl"),
            format!("{line}\n"),
        )
        .unwrap();

        reindex_stats(
            home,
            ReindexOptions { skill_filter: Some("my-skill".into()), since: None, days_back: 1 },
        )
        .await
        .unwrap();

        let stats = SkillStats::load(&SkillStats::path(home, "my-skill")).unwrap().unwrap();
        assert!(stats.curated_at.is_some(), "curated event should set curated_at");
        assert_eq!(stats.usage_count, 0, "curation is not a usage");
        assert_eq!(stats.success_count, 0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core reindex_sets_curated_at_without_counting_usage`
Expected: FAIL — `curated_at` is `None` (line is skipped because it contains neither `mur.skill.executed` nor `mur.note.retrieved`).

- [ ] **Step 3: Allow curated lines past the filter**

In `mur-core/src/skill_stats/reindex.rs`, change the early-skip filter (lines 103-107) from:

```rust
                if !trimmed.contains("mur.skill.executed")
                    && !trimmed.contains("mur.note.retrieved")
                {
                    continue;
                }
```

to:

```rust
                if !trimmed.contains("mur.skill.executed")
                    && !trimmed.contains("mur.note.retrieved")
                    && !trimmed.contains("mur.skill.curated")
                {
                    continue;
                }
```

- [ ] **Step 4: Branch on curated before counting usage**

In the same file, immediately after the skill-name match guard (after `lines_consumed += 1;` at line 118) and **before** the `let outcome = …` block (line 120), insert:

```rust
                // A curation event records human review, not a usage. Set the
                // watermark and skip the usage/outcome accounting below.
                if trimmed.contains("mur.skill.curated") {
                    if let Some(ts) = val.get("ts").and_then(|v| v.as_str())
                        && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts)
                    {
                        let utc = parsed.with_timezone(&Utc);
                        fresh.curated_at = Some(match fresh.curated_at {
                            Some(e) => e.max(utc),
                            None => utc,
                        });
                    }
                    continue;
                }
```

- [ ] **Step 5: Persist `curated_at` in the merge**

In the same file, inside the `SkillStats::merge_in_place(…, |existing| { … })` closure, add after `existing.first_successful_use_at = fresh.first_successful_use_at;` block (after line 168):

```rust
            if fresh.curated_at.is_some() {
                existing.curated_at = fresh.curated_at;
            }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p mur-core reindex_sets_curated_at_without_counting_usage`
Expected: PASS.

- [ ] **Step 7: Verify the existing reducer test still passes**

Run: `cargo test -p mur-core reindex_counts_note_retrieval_events_as_usage_and_success`
Expected: PASS (curated branch does not affect retrieval/execution counting).

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/skill_stats/reindex.rs
git commit -m "feat(stats): reduce mur.skill.curated into curated_at (A1)"
```

---

## Task 7: Sweep applies the provenance cap

**Files:**
- Modify: `mur-core/src/skill_lifecycle/sweep.rs:33-47` (`SweepOptions`), `88-117` (`run_sweep`)

- [ ] **Step 1: Write the failing test**

Add to a `#[cfg(test)] mod tests` block at the bottom of `mur-core/src/skill_lifecycle/sweep.rs` (create the block if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mur_common::skill::stats::SkillStats;

    fn write_llm_skill(home: &std::path::Path, name: &str) {
        std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
        std::fs::write(
            home.join("skills").join(name).join("skill.yaml"),
            format!("name: {name}\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: workflow\nprovenance: llm\ncontent:\n  abstract: a\n  command: \"echo hi\"\n"),
        )
        .unwrap();
    }

    // Stats that next_state() would promote to Stable: 12 successes, perfect
    // rate, aged 40 days.
    fn stable_grade_stats(home: &std::path::Path, name: &str, now: chrono::DateTime<Utc>) {
        let mut s = SkillStats::new(name, "1", "digest", now - Duration::days(40));
        s.lifecycle_state = LifecycleState::Emerging;
        s.usage_count = 12;
        s.success_count = 12;
        s.last_used_at = Some(now);
        s.last_success_at = Some(now);
        s.first_successful_use_at = Some(now - Duration::days(40));
        s.anchor_confidence = 1.0;
        s.lifecycle_changed_at = now - Duration::days(40);
        let path = SkillStats::path(home, name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();
    }

    #[test]
    fn llm_uncurated_skill_is_capped_at_emerging() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let now = Utc::now();
        write_llm_skill(home, "deploy");
        stable_grade_stats(home, "deploy", now);

        run_sweep(
            home,
            SweepOptions {
                filter: Some("deploy".into()),
                dry_run: false,
                now,
                require_human_curation_before_stable: true,
            },
        )
        .unwrap();

        let after = SkillStats::load(&SkillStats::path(home, "deploy")).unwrap().unwrap();
        assert_eq!(
            after.lifecycle_state,
            LifecycleState::Emerging,
            "LLM uncurated skill must not pass Emerging despite Stable-grade stats"
        );
    }

    #[test]
    fn llm_curated_skill_promotes_to_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let now = Utc::now();
        write_llm_skill(home, "deploy");
        stable_grade_stats(home, "deploy", now);
        // Mark curated.
        let path = SkillStats::path(home, "deploy");
        let mut s = SkillStats::load(&path).unwrap().unwrap();
        s.curated_at = Some(now - Duration::days(1));
        std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();

        run_sweep(
            home,
            SweepOptions {
                filter: Some("deploy".into()),
                dry_run: false,
                now,
                require_human_curation_before_stable: true,
            },
        )
        .unwrap();

        let after = SkillStats::load(&path).unwrap().unwrap();
        assert_eq!(after.lifecycle_state, LifecycleState::Stable);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core llm_uncurated_skill_is_capped_at_emerging llm_curated_skill_promotes_to_stable`
Expected: FAIL — `SweepOptions` has no field `require_human_curation_before_stable`.

- [ ] **Step 3: Add the `SweepOptions` field + default**

In `mur-core/src/skill_lifecycle/sweep.rs`, add to `pub struct SweepOptions { … }` (after `pub now: DateTime<Utc>,` at line 36):

```rust
    /// A1 curation gate. When true, LLM-authored uncurated skills are capped
    /// at `Emerging`. Set by the CLI from `config.skills`.
    pub require_human_curation_before_stable: bool,
```

And in `impl Default for SweepOptions` (the struct literal at lines 41-45) add after `now: Utc::now(),`:

```rust
            require_human_curation_before_stable: true,
```

- [ ] **Step 4: Import the cap function + manifest loader**

In `mur-core/src/skill_lifecycle/sweep.rs`, extend the lifecycle import (lines 7-9) to include `cap_for_provenance`:

```rust
use mur_common::skill::lifecycle::{
    calculate_decay, cap_for_provenance, half_life_days, next_state, on_promotion,
    transition_allowed,
};
```

- [ ] **Step 5: Apply the cap in `run_sweep`**

In `mur-core/src/skill_lifecycle/sweep.rs`, replace the single line that computes `proposed` (line 105):

```rust
        let proposed = next_state(&current, opts.now);
```

with:

```rust
        // Provenance gate (A1): an LLM-authored, uncurated skill cannot rise
        // above Emerging. Load the manifest for its provenance; a missing
        // manifest defaults to Human (no cap), matching `#[serde(default)]`.
        let provenance = mur_common::skill::local::load_installed(home, &name)
            .map(|m| m.provenance)
            .unwrap_or_default();
        let proposed = cap_for_provenance(
            next_state(&current, opts.now),
            provenance,
            current.curated_at.is_some(),
            opts.require_human_curation_before_stable,
        );
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p mur-core llm_uncurated_skill_is_capped_at_emerging llm_curated_skill_promotes_to_stable`
Expected: PASS (both).

- [ ] **Step 7: Fix the existing sweep callers (compile break)**

Adding a field to `SweepOptions` breaks any literal that doesn't set it. The notes test at `mur-core/src/cmd/notes_cmd.rs:632` constructs `SweepOptions { filter, dry_run, now }`. Add the new field there:

```rust
            SweepOptions {
                filter: Some("rust-errors".into()),
                dry_run: false,
                now: now + Duration::days(2),
                require_human_curation_before_stable: true,
            },
```

(That note has `provenance: Human` by default, so the gate is a no-op and the test's Draft→Emerging expectation is unchanged.)

- [ ] **Step 8: Run the full crate test + clippy**

Run: `cargo test -p mur-core skill_lifecycle:: && cargo test -p mur-core three_retrievals_promote_a_note_from_draft_to_emerging`
Expected: PASS (no other `SweepOptions` literal remains — `cmd_sweep` is updated in Task 9).

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/skill_lifecycle/sweep.rs mur-core/src/cmd/notes_cmd.rs
git commit -m "feat(sweep): apply provenance curation cap before transition (A1)"
```

---

## Task 8: `mur skill curate` emits the curation event

**Files:**
- Create: `mur-core/src/cmd/skill_curate.rs`
- Modify: `mur-core/src/cmd/mod.rs` (register the module)

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/skill_curate.rs` with this content (test first, real impl in Step 3):

```rust
//! `mur skill curate <name>` — record a human curation event so an
//! LLM-extracted skill can promote past Emerging (amendment A1).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_common::telemetry::METHOD_SKILL_CURATED;
use std::path::Path;

use super::agent::resolve_mur_home;

/// Append a `mur.skill.curated` event to today's trace log. The stats
/// reducer (`reindex_stats`) turns this into `SkillStats::curated_at`.
pub fn record_curation(mur_home: &Path, skill_name: &str, now: DateTime<Utc>) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    let line = serde_json::json!({
        "ts": now.to_rfc3339(),
        "method": METHOD_SKILL_CURATED,
        "mur.skill.name": skill_name,
    });

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

/// CLI handler for `mur skill curate <name>`.
pub fn cmd_curate(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    record_curation(&home, name, Utc::now())?;
    println!(
        "Curated '{name}'. Run `mur skill reindex-stats {name}` then `mur skill sweep` \
         to let it promote past Emerging."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_curation_appends_a_curated_event() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        record_curation(tmp.path(), "deploy", now).unwrap();

        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("mur.skill.curated"));
        assert!(content.contains("\"mur.skill.name\":\"deploy\""));
    }

    #[test]
    fn record_curation_appends_not_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        record_curation(tmp.path(), "a", now).unwrap();
        record_curation(tmp.path(), "b", now).unwrap();
        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 2);
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/mod.rs`, add (in alphabetical position among the `pub mod skill_*;` declarations):

```rust
pub mod skill_curate;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p mur-core skill_curate::`
Expected: PASS (both tests). The impl is already complete in Step 1 — this is a self-contained, deterministic module.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/skill_curate.rs mur-core/src/cmd/mod.rs
git commit -m "feat(skill): mur skill curate records curation event (A1)"
```

---

## Task 9: CLI wiring — `Curate` action + config-driven sweep

**Files:**
- Modify: `mur-core/src/cli/skill.rs:170-171` (add `Curate` variant)
- Modify: `mur-core/src/dispatch.rs:442` (dispatch arm)
- Modify: `mur-core/src/cmd/skill_sweep.rs:7-16` (read config → `SweepOptions`)

- [ ] **Step 1: Add the `Curate` CLI variant**

In `mur-core/src/cli/skill.rs`, add a variant to the `SkillAction` enum near the `Sweep` variant (after the `Sweep { … }` block ending around line 171's struct). Match the existing doc-comment + struct style:

```rust
    /// Record a human curation event for an LLM-extracted skill, so it can
    /// promote past Emerging (amendment A1).
    Curate {
        /// Skill name to curate.
        name: String,
    },
```

- [ ] **Step 2: Add the dispatch arm**

In `mur-core/src/dispatch.rs`, next to the existing `SkillAction::Sweep { name, dry_run } => { … }` arm (line 442), add:

```rust
            crate::cli::SkillAction::Curate { name } => cmd::skill_curate::cmd_curate(&name)?,
```

- [ ] **Step 3: Wire config into the sweep**

In `mur-core/src/cmd/skill_sweep.rs`, replace the `cmd_sweep` body's `SweepOptions` construction (lines 9-16) so it reads the flag from config:

```rust
pub fn cmd_sweep(filter: Option<&str>, dry_run: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let report = crate::skill_lifecycle::sweep::run_sweep(
        &home,
        crate::skill_lifecycle::sweep::SweepOptions {
            filter: filter.map(str::to_string),
            dry_run,
            now: chrono::Utc::now(),
            require_human_curation_before_stable: cfg.skills.require_human_curation_before_stable,
        },
    )?;
```

(Leave the rest of `cmd_sweep` — the printing block from line 18 onward — unchanged.)

- [ ] **Step 4: Build the whole workspace**

Run: `cargo build --workspace`
Expected: builds with no errors (all `SweepOptions` literals now set the new field; the new CLI variant is matched).

- [ ] **Step 5: Run the full test + lint gate**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: all PASS / clean.

- [ ] **Step 6: Manual smoke test**

Run:
```bash
cargo run -- skill curate nonexistent-skill
```
Expected: prints `Curated 'nonexistent-skill'. Run \`mur skill reindex-stats …\``  and exits 0 (the command records intent even if stats don't exist yet — the reducer will pick it up when the skill exists). Confirm a line was appended:
```bash
cat ~/.mur/traces/$(date +%F).jsonl | grep mur.skill.curated
```
Expected: one JSON line with `"method":"mur.skill.curated"` and `"mur.skill.name":"nonexistent-skill"`.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cli/skill.rs mur-core/src/dispatch.rs mur-core/src/cmd/skill_sweep.rs
git commit -m "feat(cli): mur skill curate + config-driven curation gate in sweep (A1)"
```

---

## Self-Review

**Spec coverage (amendment A1):**
- *`Provenance { Human, Llm, Hybrid }` on the manifest* → Task 1. ✅
- *Extracted skills enter as `Llm`* → Task 1 enables the value; the extraction judge (separate, deferred plan P5a) sets it. Default `Human` keeps all current skills ungated. ✅
- *Curation gate: `provenance == Llm` cannot promote past `Emerging` until a human curation event* → Tasks 3 (pure cap), 6 (curated_at from event), 7 (sweep applies cap). ✅
- *Config `require_human_curation_before_stable` (default true), not hardcoded (Mandatory Rule #1)* → Task 4 + wired in Task 9. ✅
- *`{"kind":"curate"}` appended to the per-skill event log* → realized as a `mur.skill.curated` trace event (Task 5 const, Task 8 writer) consistent with the existing trace-log-is-source-of-truth reducer (`record_retrieval` pattern). ✅

**Deferred (out of scope, noted for the reader):** the *flip Llm→Hybrid on first curation* phrasing in the amendment is realized here as `curated_at` (a stats-side review flag) rather than a manifest rewrite — this avoids re-hashing/re-signing the manifest on every curation and keeps `provenance` as an immutable origin record. The gate keys on `curated_at`, which is the behavior the amendment requires. The extraction judge that *produces* `provenance: llm` skills is a separate plan (v2 P5a) and is not built here.

**Placeholder scan:** none — every code step shows complete code; every run step shows the exact command + expected result.

**Type consistency:** `Provenance` (Task 1) is imported and used identically in `lifecycle.rs` (Task 3), `reindex.rs`/`sweep.rs` read `manifest.provenance` (Tasks 6-7). `curated_at: Option<DateTime<Utc>>` is defined once (Task 2), written by the reducer (Task 6), and read by the sweep as `current.curated_at.is_some()` (Task 7). `require_human_curation_before_stable` is the same name on `SkillsConfig` (Task 4) and `SweepOptions` (Task 7), wired in Task 9. `cap_for_provenance(proposed, provenance, curated, gate_enabled)` signature (Task 3) matches its call site (Task 7). ✅
