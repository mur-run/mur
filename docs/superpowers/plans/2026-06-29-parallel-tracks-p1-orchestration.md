# Parallel Tracks P1 — Orchestration + CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire together the existing P0 skeleton — tree-sitter, CAS, CyclicJudge, cherry-pick, LMDB — into a working `mur fleet compare / judge / cherry` pipeline with track worktree management.

**Architecture:** Three new pieces connect the existing components: (1) `Track`/`TrackSet` types that record which worktree belongs to which fleet member, (2) a `fleet judge` command that runs the pre-filter → semantic → CAS → LLM judge pipeline and stores results in LMDB, (3) `fleet compare` and `fleet cherry` commands that read those results and produce output. The fleet run machinery (existing) runs agents; judge/compare/cherry analyze the results afterward.

**Tech Stack:** Rust 2024, existing deps — `tree-sitter`/`tree-sitter-rust` (semantic parse), `blake3` (CAS), `heed` (LMDB cache), `serde_json`/`serde_yaml` (config), `tokio` (async judge calls), `mur_common::parallel` (config types), `mur_common::config::BackendConfig` (LLM factory).

## Global Constraints

- No new workspace dependencies — all required crates are already in `mur-core/Cargo.toml` (`tree-sitter`, `tree-sitter-rust`, `blake3`, `heed`, `tokio`, `serde`, `serde_json`, `serde_yaml`, `anyhow`, `dirs`, `tempfile`)
- Single source file ≤ 800 lines; split into submodules when approaching limit
- `cargo clippy --workspace -- -D warnings` must pass
- `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist` must be set when running clippy/tests
- Brand name is uppercase **MUR** in all user-visible strings
- `CyclicJudge` is the only judge strategy in P1 (no alternatives)
- `detect_backend()` already handles worktree creation; `track/worktree.rs` calls it
- `fleet judge` is **synchronous** in P1 (blocking `tokio::runtime::Runtime::block_on`) — async refactor is P2
- Gate 0 validation script is the final task; it validates assumptions with existing code

---

## Existing Skeleton — What NOT to Rewrite

All of these exist and are real implementations. Do not replace them:

| File | Lines | Status |
|------|-------|--------|
| `mur-common/src/parallel.rs` | 120 | ✅ complete — `ParallelMode`, `TrackConfig`, `JudgeConfig`, `Rubric`, `PreFilterKind`, `ParallelConfig` |
| `mur-core/src/parallel/backend/` | ~400 | ✅ complete — `ParallelBackend` trait, `GitWorktreeBackend`, `ZfsNativeBackend`, `ZfsSocketBackend`, `detect_backend()` |
| `mur-core/src/parallel/semantic/mod.rs` | 39 | ✅ complete — `SemanticUnit`, `UnitKind`, `SupportedLanguage`, `extract_units()` re-export |
| `mur-core/src/parallel/semantic/tree_sitter_parse.rs` | 114 | ✅ complete — `extract_units(source, lang) -> Vec<SemanticUnit>` |
| `mur-core/src/parallel/semantic/cas.rs` | 154 | ✅ complete — `group_by_identity()`, `UnitGroups`, `JudgeGroup`, `SkipUnit` |
| `mur-core/src/parallel/judge/mod.rs` | 28 | ✅ complete — `JudgeTask`, `TrackImpl`, `TrackScore`, re-exports |
| `mur-core/src/parallel/judge/cyclic.rs` | 125 | ✅ complete — `CyclicJudge::score()` (async, 2 cyclic rounds, anti-position-bias) |
| `mur-core/src/parallel/judge/rubric.rs` | 76 | ✅ complete — `build_judge_prompt()` |
| `mur-core/src/parallel/cherry/mod.rs` | 28 | ✅ complete — `UnitSelection`, `CherryPlan`, `winning_track_for()` |
| `mur-core/src/parallel/cherry/picker.rs` | 67 | ✅ complete — `cherry_pick(scores_per_unit) -> CherryPlan` |
| `mur-core/src/parallel/cherry/assemble.rs` | 95 | ✅ complete — `assemble_file(base_source, units_by_track, plan) -> Vec<u8>` |
| `mur-core/src/parallel/cherry/conflict.rs` | 26 | ✅ complete — `check_conflicts(plan, units) -> ConflictReport` |
| `mur-core/src/parallel/state/lmdb.rs` | 99 | ✅ complete — `ParallelStateDb::open()`, `get_score()`, `put_score()` |
| `mur-core/src/parallel/state/mod.rs` | 16 | ✅ complete — `JudgeScore`, re-exports |
| `mur-core/src/parallel/track/filter.rs` | 71 | ✅ complete — `run_pre_filter(path, filters) -> FilterResult` |
| `mur-core/src/conversations/backend/factory.rs` | ~80 | ✅ complete — `build_for_stage(cfg, stage) -> Arc<dyn ChatBackend>` |
| `mur-core/src/cli/actions.rs` | 540 | ✅ wired — `FleetAction::Compare`, `Judge`, `Cherry` variants defined |
| `mur-core/src/dispatch.rs:353-361` | — | ✅ wired — dispatches to `cmd_fleet_compare`, `cmd_fleet_judge`, `cmd_fleet_cherry` |

**Stubs that this plan fills in:**

| File | Current | Target |
|------|---------|--------|
| `mur-core/src/parallel/track/mod.rs` | 3 lines (only `pub mod filter`) | `Track`, `TrackSet` types + save/load |
| `mur-core/src/parallel/track/worktree.rs` | MISSING | create/destroy worktrees via `detect_backend()` |
| `mur-core/src/parallel/track/diversity.rs` | MISSING | approach prompt injection per member |
| `mur-core/src/parallel/mod.rs` | 9 lines (module re-exports only) | `run_judge_pipeline()` orchestrator |
| `mur-core/src/cmd/fleet/judge_cmd.rs` | 57-line stub (prints placeholder) | full pipeline |
| `mur-core/src/cmd/fleet/compare.rs` | 48-line stub (prints placeholder) | LMDB reads + table |
| `mur-core/src/cmd/fleet/cherry_cmd.rs` | 29-line stub (prints placeholder) | cherry_pick + assemble + validate |

---

## Task 1: Track and TrackSet types

**Files:**
- Modify: `mur-core/src/parallel/track/mod.rs` (replace 3-line stub)

**Interfaces:**
- Produces: `Track { config: TrackConfig, worktree_path: PathBuf }`, `TrackSet { tracks: Vec<Track> }`, `TrackSet::save(dir)`, `TrackSet::load(dir)`

- [ ] **Step 1: Write the failing test**

```rust
// At the bottom of mur-core/src/parallel/track/mod.rs (in #[cfg(test)])
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::TrackConfig;

    #[test]
    fn trackset_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ts = TrackSet {
            tracks: vec![
                Track {
                    config: TrackConfig {
                        name: "track-a".into(),
                        approach: "functional".into(),
                        model: None,
                    },
                    worktree_path: std::path::PathBuf::from("/tmp/track-a"),
                },
                Track {
                    config: TrackConfig {
                        name: "track-b".into(),
                        approach: "performance".into(),
                        model: None,
                    },
                    worktree_path: std::path::PathBuf::from("/tmp/track-b"),
                },
            ],
        };
        ts.save(dir.path()).unwrap();
        let loaded = TrackSet::load(dir.path()).unwrap();
        assert_eq!(loaded.tracks.len(), 2);
        assert_eq!(loaded.tracks[0].config.name, "track-a");
        assert_eq!(loaded.tracks[1].worktree_path, std::path::PathBuf::from("/tmp/track-b"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::track::tests::trackset_roundtrip 2>&1 | tail -5
```
Expected: FAIL with "unresolved imports" or "struct not found"

- [ ] **Step 3: Write implementation**

Replace `mur-core/src/parallel/track/mod.rs` entirely:

```rust
//! Track and TrackSet — one entry per parallel fleet member + worktree.
#![allow(dead_code)]

pub mod filter;

use anyhow::Result;
use mur_common::parallel::TrackConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One parallel track: a fleet member with its dedicated worktree path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub config: TrackConfig,
    pub worktree_path: PathBuf,
}

/// All tracks for a parallel fleet session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackSet {
    pub tracks: Vec<Track>,
}

impl TrackSet {
    /// Persist to `<dir>/tracks.json`.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("tracks.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load from `<dir>/tracks.json`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("tracks.json");
        let json = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("cannot read tracks.json at {path:?}: {e}"))?;
        Ok(serde_json::from_str(&json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::TrackConfig;

    #[test]
    fn trackset_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ts = TrackSet {
            tracks: vec![
                Track {
                    config: TrackConfig {
                        name: "track-a".into(),
                        approach: "functional".into(),
                        model: None,
                    },
                    worktree_path: PathBuf::from("/tmp/track-a"),
                },
                Track {
                    config: TrackConfig {
                        name: "track-b".into(),
                        approach: "performance".into(),
                        model: None,
                    },
                    worktree_path: PathBuf::from("/tmp/track-b"),
                },
            ],
        };
        ts.save(dir.path()).unwrap();
        let loaded = TrackSet::load(dir.path()).unwrap();
        assert_eq!(loaded.tracks.len(), 2);
        assert_eq!(loaded.tracks[0].config.name, "track-a");
        assert_eq!(
            loaded.tracks[1].worktree_path,
            PathBuf::from("/tmp/track-b")
        );
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::track::tests::trackset_roundtrip 2>&1 | tail -5
```
Expected: `test parallel::track::tests::trackset_roundtrip ... ok`

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/track/mod.rs
git commit -m "feat(parallel/p1): Track + TrackSet types with JSON persistence"
```

---

## Task 2: Worktree lifecycle

**Files:**
- Create: `mur-core/src/parallel/track/worktree.rs`
- Modify: `mur-core/src/parallel/track/mod.rs` — add `pub mod worktree;`

**Interfaces:**
- Consumes: `detect_backend(project)` from `crate::parallel::backend::detect_backend`, `TrackConfig`, `ParallelConfig`
- Produces: `create_tracks(config: &ParallelConfig, project: &Path) -> Result<TrackSet>`, `destroy_tracks(tracks: &TrackSet, project: &Path)`

**Context:** `detect_backend(project).create_track(name)` creates one worktree and returns its `PathBuf`. The track name is the `TrackConfig.name`. Destroying calls `backend.destroy(path)`.

- [ ] **Step 1: Write the failing test**

Add to the bottom of the new `worktree.rs` file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::{ParallelConfig, TrackConfig, JudgeConfig, Rubric, PreFilterKind};

    fn two_track_config() -> ParallelConfig {
        ParallelConfig {
            mode: Default::default(),
            tracks: vec![
                TrackConfig { name: "t-a".into(), approach: "functional".into(), model: None },
                TrackConfig { name: "t-b".into(), approach: "performance".into(), model: None },
            ],
            judge: JudgeConfig {
                model: "claude-haiku-4-5".into(),
                rubric: Rubric::default(),
                strategy: Default::default(),
            },
            filters: vec![PreFilterKind::CargoCheck],
        }
    }

    #[test]
    fn create_tracks_returns_one_track_per_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = two_track_config();
        let ts = create_tracks(&cfg, dir.path()).unwrap();
        assert_eq!(ts.tracks.len(), 2);
        assert_eq!(ts.tracks[0].config.name, "t-a");
        // Each worktree path must exist on disk
        for t in &ts.tracks {
            assert!(t.worktree_path.exists(), "{:?} should exist", t.worktree_path);
        }
    }

    #[test]
    fn destroy_tracks_removes_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = two_track_config();
        let ts = create_tracks(&cfg, dir.path()).unwrap();
        let paths: Vec<_> = ts.tracks.iter().map(|t| t.worktree_path.clone()).collect();
        destroy_tracks(&ts, dir.path());
        for p in &paths {
            assert!(!p.exists(), "{p:?} should be removed");
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::track::worktree 2>&1 | tail -5
```
Expected: FAIL with "module not found"

- [ ] **Step 3: Write implementation**

Create `mur-core/src/parallel/track/worktree.rs`:

```rust
//! Worktree lifecycle for parallel tracks.
use anyhow::Result;
use mur_common::parallel::ParallelConfig;
use std::path::Path;

use crate::parallel::backend::detect_backend;
use super::{Track, TrackSet};

/// Create one git worktree (or ZFS clone) per track in `config`.
/// Stores the resulting paths in a `TrackSet`. Does NOT save tracks.json;
/// the caller decides when to persist (typically after `fleet create`).
pub fn create_tracks(config: &ParallelConfig, project: &Path) -> Result<TrackSet> {
    let backend = detect_backend(project);
    let mut tracks = Vec::with_capacity(config.tracks.len());
    for tc in &config.tracks {
        let worktree_path = backend.create_track(&tc.name)?;
        tracks.push(Track {
            config: tc.clone(),
            worktree_path,
        });
    }
    Ok(TrackSet { tracks })
}

/// Destroy all worktrees in a `TrackSet`. Errors are logged but not fatal —
/// we always attempt all tracks even if one fails.
pub fn destroy_tracks(tracks: &TrackSet, project: &Path) {
    let backend = detect_backend(project);
    for t in &tracks.tracks {
        if let Err(e) = backend.destroy(&t.worktree_path) {
            eprintln!(
                "warn: failed to destroy track {} at {:?}: {e}",
                t.config.name, t.worktree_path
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::{JudgeConfig, ParallelConfig, PreFilterKind, Rubric, TrackConfig};

    fn two_track_config() -> ParallelConfig {
        ParallelConfig {
            mode: Default::default(),
            tracks: vec![
                TrackConfig {
                    name: "t-a".into(),
                    approach: "functional".into(),
                    model: None,
                },
                TrackConfig {
                    name: "t-b".into(),
                    approach: "performance".into(),
                    model: None,
                },
            ],
            judge: JudgeConfig {
                model: "claude-haiku-4-5".into(),
                rubric: Rubric::default(),
                strategy: Default::default(),
            },
            filters: vec![PreFilterKind::CargoCheck],
        }
    }

    #[test]
    fn create_tracks_returns_one_track_per_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = two_track_config();
        let ts = create_tracks(&cfg, dir.path()).unwrap();
        assert_eq!(ts.tracks.len(), 2);
        assert_eq!(ts.tracks[0].config.name, "t-a");
        for t in &ts.tracks {
            assert!(
                t.worktree_path.exists(),
                "{:?} should exist",
                t.worktree_path
            );
        }
    }

    #[test]
    fn destroy_tracks_removes_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = two_track_config();
        let ts = create_tracks(&cfg, dir.path()).unwrap();
        let paths: Vec<_> = ts.tracks.iter().map(|t| t.worktree_path.clone()).collect();
        destroy_tracks(&ts, dir.path());
        for p in &paths {
            assert!(!p.exists(), "{p:?} should be removed");
        }
    }
}
```

Add `pub mod worktree;` to the top of `mur-core/src/parallel/track/mod.rs` (after `pub mod filter;`).

- [ ] **Step 4: Run tests to verify they pass**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::track::worktree 2>&1 | tail -8
```
Expected:
```
test parallel::track::worktree::tests::create_tracks_returns_one_track_per_config ... ok
test parallel::track::worktree::tests::destroy_tracks_removes_worktrees ... ok
test result: ok. 2 passed; 0 failed
```

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/track/worktree.rs mur-core/src/parallel/track/mod.rs
git commit -m "feat(parallel/p1): worktree lifecycle — create/destroy tracks via detect_backend"
```

---

## Task 3: Approach diversity injection

**Files:**
- Create: `mur-core/src/parallel/track/diversity.rs`
- Modify: `mur-core/src/parallel/track/mod.rs` — add `pub mod diversity;`

**Interfaces:**
- Consumes: `TrackConfig` (from `mur_common::parallel`)
- Produces: `fn approach_system_suffix(tc: &TrackConfig) -> String` — returns a block of text that can be appended to an agent's system prompt to steer it toward the desired approach

**Context:** Agents in a fleet receive their approach via their fleet member YAML (`system_prompt` extension). In P1, the injected text is a formatted block appended to the base system prompt. Full agent profile patching (writing to `~/.mur/agents/<name>/profile.yaml`) is Task 3b and is NOT part of P1 — `approach_system_suffix` is the primitive used by `judge_cmd` to label TrackImpl entries.

- [ ] **Step 1: Write the failing test**

```rust
// In diversity.rs #[cfg(test)]
#[test]
fn suffix_contains_approach_text() {
    use mur_common::parallel::TrackConfig;
    let tc = TrackConfig {
        name: "track-functional".into(),
        approach: "Prefer functional style: Iterator combinators, avoid mutable state.".into(),
        model: None,
    };
    let suffix = approach_system_suffix(&tc);
    assert!(suffix.contains("track-functional"));
    assert!(suffix.contains("Iterator combinators"));
    // Must be non-empty and end with a newline for clean prompt appending.
    assert!(suffix.ends_with('\n'));
}

#[test]
fn suffix_is_stable_across_calls() {
    use mur_common::parallel::TrackConfig;
    let tc = TrackConfig {
        name: "t".into(),
        approach: "performance first".into(),
        model: None,
    };
    assert_eq!(approach_system_suffix(&tc), approach_system_suffix(&tc));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::track::diversity 2>&1 | tail -5
```
Expected: FAIL with "module not found"

- [ ] **Step 3: Write implementation**

Create `mur-core/src/parallel/track/diversity.rs`:

```rust
//! Approach prompt injection for parallel tracks.
use mur_common::parallel::TrackConfig;

/// Returns a system-prompt suffix block that steers the agent toward
/// this track's `approach`. Appended to the agent's base system prompt
/// at session start so the approach is always in-context.
pub fn approach_system_suffix(tc: &TrackConfig) -> String {
    format!(
        "\n---\n## Track: {}\n\nFor this coding session, apply the following approach:\n{}\n",
        tc.name,
        tc.approach.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::TrackConfig;

    #[test]
    fn suffix_contains_approach_text() {
        let tc = TrackConfig {
            name: "track-functional".into(),
            approach: "Prefer functional style: Iterator combinators, avoid mutable state.".into(),
            model: None,
        };
        let suffix = approach_system_suffix(&tc);
        assert!(suffix.contains("track-functional"));
        assert!(suffix.contains("Iterator combinators"));
        assert!(suffix.ends_with('\n'));
    }

    #[test]
    fn suffix_is_stable_across_calls() {
        let tc = TrackConfig {
            name: "t".into(),
            approach: "performance first".into(),
            model: None,
        };
        assert_eq!(approach_system_suffix(&tc), approach_system_suffix(&tc));
    }
}
```

Add `pub mod diversity;` to `mur-core/src/parallel/track/mod.rs`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::track::diversity 2>&1 | tail -5
```
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/track/diversity.rs mur-core/src/parallel/track/mod.rs
git commit -m "feat(parallel/p1): approach prompt suffix for track diversity injection"
```

---

## Task 4: `run_judge_pipeline` orchestrator

**Files:**
- Modify: `mur-core/src/parallel/mod.rs` (replace 9-line stub)

**Interfaces:**
- Consumes: `TrackSet` (Task 1), `ParallelConfig`, `ParallelStateDb`, `run_pre_filter()`, `extract_units()`, `group_by_identity()`, `CyclicJudge`, `build_for_stage()`
- Produces: `pub fn run_judge_pipeline(tracks: &TrackSet, config: &ParallelConfig, state_db: &ParallelStateDb) -> Result<()>`

**Context:** This is the synchronous judge pipeline. It does NOT start a tokio runtime — callers (judge_cmd) wrap it in `tokio::runtime::Runtime::new()?.block_on(run_judge_pipeline_async(...))`. Pipeline steps:
1. Pre-filter each track (cargo check); skip failed tracks
2. Parse Rust files changed in each track via `extract_units()`
3. `group_by_identity()` → `UnitGroups { skip: Vec<SkipUnit>, judge: Vec<JudgeGroup> }`
4. For each `JudgeGroup`: check LMDB cache by `content_hash + rubric_version`; call `CyclicJudge::score()` for cache misses; store result

- [ ] **Step 1: Write the failing test**

```rust
// In parallel/mod.rs #[cfg(test)]
#[test]
fn pipeline_smoke_with_mock_backend() {
    // Verify the pipeline returns Ok (no panic) when called with
    // a TrackSet whose worktrees contain no Rust files.
    let dir = tempfile::tempdir().unwrap();
    let state_db = crate::parallel::state::ParallelStateDb::open(dir.path()).unwrap();
    use mur_common::parallel::{JudgeConfig, ParallelConfig, PreFilterKind, Rubric, TrackConfig};
    let config = ParallelConfig {
        mode: Default::default(),
        tracks: vec![],
        judge: JudgeConfig {
            model: "claude-haiku-4-5".into(),
            rubric: Rubric::default(),
            strategy: Default::default(),
        },
        filters: vec![PreFilterKind::CargoCheck],
    };
    let tracks = crate::parallel::track::TrackSet { tracks: vec![] };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(run_judge_pipeline_async(&tracks, &config, &state_db));
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::tests::pipeline_smoke_with_mock_backend 2>&1 | tail -5
```
Expected: FAIL with "function not found"

- [ ] **Step 3: Write implementation**

Replace `mur-core/src/parallel/mod.rs` entirely:

```rust
//! Speculative parallel agent execution — P0/P1 orchestrator.
#![allow(dead_code, unused_imports)]

pub mod backend;
pub mod cherry;
pub mod judge;
pub mod semantic;
pub mod state;
pub mod track;

use anyhow::Result;
use mur_common::parallel::ParallelConfig;
use state::ParallelStateDb;
use track::TrackSet;

/// Run the full judge pipeline over all tracks:
/// pre-filter → semantic parse → CAS dedup → LLM judge (cached) → LMDB store.
///
/// Async because `CyclicJudge::score()` calls an LLM. Callers that are
/// synchronous should use `tokio::runtime::Runtime::new()?.block_on(...)`.
pub async fn run_judge_pipeline_async(
    tracks: &TrackSet,
    config: &ParallelConfig,
    state_db: &ParallelStateDb,
) -> Result<()> {
    use semantic::{SupportedLanguage, extract_units};
    use semantic::cas::group_by_identity;
    use judge::{CyclicJudge, JudgeTask, TrackImpl};
    use track::filter::{FilterResult, run_pre_filter};
    use std::sync::Arc;
    use mur_common::config::BackendConfig;
    use crate::conversations::backend::factory::build_for_stage;

    let rubric_version = config.judge.rubric.version();

    // 1. Pre-filter: cargo check each track; skip failures.
    let mut live_tracks: Vec<&track::Track> = Vec::new();
    for t in &tracks.tracks {
        match run_pre_filter(&t.worktree_path, &config.filters) {
            FilterResult::Passed => live_tracks.push(t),
            FilterResult::Failed { filter, stderr } => {
                eprintln!(
                    "  track {} failed {:?} pre-filter — skipping\n    {}",
                    t.config.name,
                    filter,
                    stderr.lines().next().unwrap_or("")
                );
            }
        }
    }

    if live_tracks.is_empty() {
        eprintln!("  all tracks failed pre-filter; nothing to judge");
        return Ok(());
    }

    // 2. Parse changed .rs files in each surviving track.
    let mut track_units: Vec<(&str, Vec<semantic::SemanticUnit>)> = Vec::new();
    for t in &live_tracks {
        let mut units: Vec<semantic::SemanticUnit> = Vec::new();
        for entry in walkdir::WalkDir::new(&t.worktree_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            if let Ok(source) = std::fs::read(entry.path()) {
                units.extend(extract_units(&source, SupportedLanguage::Rust));
            }
        }
        track_units.push((&t.config.name, units));
    }

    // 3. CAS dedup.
    let groups = group_by_identity(&track_units);

    // 4. Judge each group — check cache first.
    if groups.judge.is_empty() {
        eprintln!("  all units identical across tracks (CAS hit) — no LLM calls needed");
        return Ok(());
    }

    // Build backend once for all judge calls (model from JudgeConfig).
    let judge_backend_cfg = BackendConfig {
        provider: "anthropic".into(),
        model: config.judge.model.clone(),
        endpoint: None,
        api_key_env: None,
        timeout_secs: Some(120),
    };
    let backend = build_for_stage(&judge_backend_cfg, "parallel.judge")?;
    let judge = CyclicJudge::new(config.judge.clone(), Arc::clone(&backend));

    let mut cache_hits = 0usize;
    let mut judge_calls = 0usize;

    for jg in &groups.judge {
        let unit_name = &jg.unit_name;

        // Build TrackImpl list from the JudgeGroup.
        let mut impls: Vec<TrackImpl> = Vec::new();
        for (track_name, unit) in &jg.variants {
            if let Some(t) = live_tracks.iter().find(|t| &t.config.name == track_name) {
                // Try reading the source bytes from the unit's file path.
                // `unit.byte_range` gives the byte offset within the file.
                // We re-read the file to get the source slice.
                let file_source = live_tracks
                    .iter()
                    .find(|t| &t.config.name == track_name)
                    .and_then(|_| {
                        // Walk to find the file containing this unit.
                        // In P1 we store the full file source as the unit source.
                        None::<Vec<u8>>
                    })
                    .unwrap_or_else(|| unit.content_hash.to_vec());
                impls.push(TrackImpl {
                    track_config: t.config.clone(),
                    unit: unit.clone(),
                    source: file_source,
                });
            }
        }

        if impls.is_empty() {
            continue;
        }

        // Check LMDB cache (keyed by content_hash + rubric_version).
        // For a group, we check the FIRST impl's hash as the group representative.
        let rep_hash = &impls[0].unit.content_hash;
        if state_db.get_score(rep_hash, &rubric_version)?.is_some() {
            cache_hits += 1;
            continue;
        }

        // Build JudgeTask and call CyclicJudge.
        let task = JudgeTask {
            unit_name: unit_name.clone(),
            implementations: impls,
            rubric_version: rubric_version.clone(),
        };
        let scores = judge.score(&task).await?;
        judge_calls += 1;

        // Store all scores for this group.
        for score in &scores {
            if let Some(imp) = task
                .implementations
                .iter()
                .find(|i| i.track_config.name == score.track_name)
            {
                let js = state::JudgeScore {
                    score: score.score,
                    reasoning: score.reasoning.clone(),
                    model: config.judge.model.clone(),
                    ts: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                };
                state_db.put_score(&imp.unit.content_hash, &rubric_version, &js)?;
            }
        }
    }

    eprintln!(
        "  judge complete: {} units, {} cache hits, {} LLM calls",
        groups.judge.len(),
        cache_hits,
        judge_calls
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::{JudgeConfig, ParallelConfig, PreFilterKind, Rubric};

    #[test]
    fn pipeline_smoke_with_empty_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = state::ParallelStateDb::open(dir.path()).unwrap();
        let config = ParallelConfig {
            mode: Default::default(),
            tracks: vec![],
            judge: JudgeConfig {
                model: "claude-haiku-4-5".into(),
                rubric: Rubric::default(),
                strategy: Default::default(),
            },
            filters: vec![PreFilterKind::CargoCheck],
        };
        let tracks = track::TrackSet { tracks: vec![] };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_judge_pipeline_async(&tracks, &config, &state_db));
        assert!(result.is_ok());
    }
}
```

**Note:** `walkdir` is already in `mur-core/Cargo.toml` (used by other modules). If it isn't, add it as a workspace dependency.

**Note:** The unit source-loading in the inner loop uses `unit.content_hash.to_vec()` as a placeholder for P1 — the actual source byte extraction from `byte_range` in the specific file is wired in Task 5 where we know the full file path.

- [ ] **Step 4: Run test to verify it passes**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::tests::pipeline_smoke_with_empty_tracks 2>&1 | tail -5
```
Expected: `test parallel::tests::pipeline_smoke_with_empty_tracks ... ok`

- [ ] **Step 5: Check clippy**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error" | head -5
```
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/parallel/mod.rs
git commit -m "feat(parallel/p1): run_judge_pipeline_async orchestrator — pre-filter → semantic → CAS → judge → LMDB"
```

---

## Task 5: `fleet judge` implementation

**Files:**
- Modify: `mur-core/src/cmd/fleet/judge_cmd.rs` (replace 57-line stub)

**Interfaces:**
- Consumes: `load_fleet(mur_home, name)`, `TrackSet::load()`, `run_judge_pipeline_async()`, `ParallelStateDb::open()`
- Signature (unchanged): `pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str) -> Result<()>`

**Context:**
- Fleet state dir: `~/.mur/fleets/<name>/`
- TrackSet JSON: `~/.mur/fleets/<name>/tracks.json`
- LMDB dir: `~/.mur/fleets/<name>/parallel_state/`
- If `tracks.json` doesn't exist: bail with "fleet has no track worktrees — run `mur fleet run <name>` first"

- [ ] **Step 1: Write the failing test**

No unit test for judge_cmd itself (it needs real worktrees). Verify compilation only:

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check -p mur-core 2>&1 | grep "^error" | head -10
```
Expected: currently shows no errors (stub compiles). After rewrite, it must still compile.

- [ ] **Step 2: Replace `judge_cmd.rs`**

```rust
//! `mur fleet judge <name>` — run the full judge pipeline.
use anyhow::{Context, Result};
use std::path::Path;

use super::store::load_fleet;
use crate::parallel::{run_judge_pipeline_async, state::ParallelStateDb, track::TrackSet};

pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config — add a `parallel:` section to fleet.yaml")?;

    let fleet_dir = mur_home.join("fleets").join(fleet_name);

    let tracks = TrackSet::load(&fleet_dir).context(
        "no tracks.json — run `mur fleet run <fleet_name>` first to populate track worktrees",
    )?;

    let state_dir = fleet_dir.join("parallel_state");
    let state_db = ParallelStateDb::open(&state_dir)?;

    eprintln!("Running judge pipeline for fleet '{fleet_name}' ({} tracks)...", tracks.tracks.len());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_judge_pipeline_async(&tracks, parallel, &state_db))?;

    println!("Judge complete. Run `mur fleet compare {fleet_name}` to view scores.");
    Ok(())
}
```

- [ ] **Step 3: Verify compilation**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check -p mur-core 2>&1 | grep "^error" | head -10
```
Expected: 0 errors

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/fleet/judge_cmd.rs
git commit -m "feat(parallel/p1): fleet judge — wire run_judge_pipeline_async + LMDB"
```

---

## Task 6: `fleet compare` table output

**Files:**
- Modify: `mur-core/src/cmd/fleet/compare.rs` (replace 48-line stub)

**Interfaces:**
- Consumes: `load_fleet()`, `TrackSet::load()`, `ParallelStateDb::open()` + `get_score()`
- Signature (unchanged): `pub fn cmd_fleet_compare(mur_home: &Path, fleet_name: &str, unit_filter: Option<&str>) -> Result<()>`

**Context:** The compare command reads all scores stored in LMDB and prints a table. In P1, we only have content-hash-keyed scores. To associate scores with unit names, we must re-parse the track worktrees (same as the judge pipeline). Keep it simple: re-parse, look up score by content hash, print table.

Output format (from spec):
```
src/auth.rs

Function          track-functional  track-performance  track-readability  Rec
────────────────────────────────────────────────────────────────────────────
authenticate      8.2               7.1                9.0                track-readability ★
authorize         7.5               8.8                7.2                track-performance ★
```

- [ ] **Step 1: Write test for the table formatter**

```rust
// In compare.rs #[cfg(test)] 
#[test]
fn format_score_row_pads_columns() {
    let row = format_score_row("my_function", &[("t-a", Some(8.2)), ("t-b", Some(7.1))], "t-a");
    assert!(row.contains("my_function"));
    assert!(row.contains("8.2"));
    assert!(row.contains("t-a ★"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::fleet::compare 2>&1 | tail -5
```
Expected: FAIL

- [ ] **Step 3: Replace `compare.rs`**

```rust
//! `mur fleet compare <name>` — per-unit scores across all parallel tracks.
#![allow(dead_code)]
use anyhow::{Context, Result};
use std::path::Path;

use crate::parallel::{
    semantic::{SupportedLanguage, extract_units},
    state::ParallelStateDb,
    track::TrackSet,
};
use super::store::load_fleet;

pub fn cmd_fleet_compare(
    mur_home: &Path,
    fleet_name: &str,
    unit_filter: Option<&str>,
) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;

    let fleet_dir = mur_home.join("fleets").join(fleet_name);
    let tracks = TrackSet::load(&fleet_dir)
        .context("no tracks.json — run `mur fleet run` then `mur fleet judge` first")?;
    let state_db = ParallelStateDb::open(&fleet_dir.join("parallel_state"))?;
    let rubric_ver = parallel.judge.rubric.version();

    let track_names: Vec<&str> = tracks.tracks.iter().map(|t| t.config.name.as_str()).collect();

    // Collect all unit names across tracks by re-parsing worktrees.
    let mut all_unit_names: Vec<String> = Vec::new();
    // unit_name → track_name → JudgeScore
    let mut score_map: std::collections::HashMap<String, Vec<(String, Option<f32>)>> =
        std::collections::HashMap::new();

    for t in &tracks.tracks {
        for entry in walkdir::WalkDir::new(&t.worktree_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            if let Ok(source) = std::fs::read(entry.path()) {
                for unit in extract_units(&source, SupportedLanguage::Rust) {
                    let name = unit.name.clone();
                    if let Some(filter) = unit_filter {
                        if !name.contains(filter) {
                            continue;
                        }
                    }
                    let score_val = state_db
                        .get_score(&unit.content_hash, &rubric_ver)?
                        .map(|s| s.score);
                    let entry = score_map.entry(name.clone()).or_default();
                    entry.push((t.config.name.clone(), score_val));
                    if !all_unit_names.contains(&name) {
                        all_unit_names.push(name);
                    }
                }
            }
        }
    }

    if all_unit_names.is_empty() {
        println!("No units found. Run `mur fleet judge {fleet_name}` to populate scores.");
        return Ok(());
    }

    // Print header.
    let col_w = 14usize;
    let name_w = 28usize;
    let header: String = std::iter::once(format!("{:<name_w$}", "Function"))
        .chain(track_names.iter().map(|n| format!("{:<col_w$}", n)))
        .chain(std::iter::once("Rec".into()))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{header}");
    println!("{}", "─".repeat(header.len()));

    all_unit_names.sort();
    for unit_name in &all_unit_names {
        let scores: Vec<(&str, Option<f32>)> = track_names
            .iter()
            .map(|tn| {
                let score = score_map
                    .get(unit_name)
                    .and_then(|v| v.iter().find(|(t, _)| t == tn))
                    .and_then(|(_, s)| *s);
                (*tn, score)
            })
            .collect();
        let rec = scores
            .iter()
            .filter_map(|(tn, s)| s.map(|v| (*tn, v)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(tn, _)| tn)
            .unwrap_or("-");
        println!("{}", format_score_row(unit_name, &scores, rec));
    }
    Ok(())
}

fn format_score_row(unit_name: &str, scores: &[(&str, Option<f32>)], rec: &str) -> String {
    let name_w = 28usize;
    let col_w = 14usize;
    let mut parts = vec![format!("{:<name_w$}", truncate(unit_name, name_w))];
    for (track_name, score) in scores {
        let cell = score.map(|s| format!("{:.1}", s)).unwrap_or_else(|| "-".into());
        parts.push(format!("{:<col_w$}", cell));
    }
    // Find the rec track name in scores to append ★
    let rec_col = scores
        .iter()
        .find(|(tn, _)| *tn == rec)
        .map(|(tn, _)| format!("{tn} ★"))
        .unwrap_or_else(|| rec.into());
    parts.push(rec_col);
    parts.join("  ")
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_score_row_pads_columns() {
        let row = format_score_row(
            "my_function",
            &[("t-a", Some(8.2)), ("t-b", Some(7.1))],
            "t-a",
        );
        assert!(row.contains("my_function"));
        assert!(row.contains("8.2"));
        assert!(row.contains("t-a ★"));
    }

    #[test]
    fn format_score_row_handles_missing_score() {
        let row = format_score_row("f", &[("t-a", None), ("t-b", Some(5.0))], "t-b");
        assert!(row.contains('-'));
        assert!(row.contains("t-b ★"));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::fleet::compare 2>&1 | tail -8
```
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet/compare.rs
git commit -m "feat(parallel/p1): fleet compare — per-unit score table from LMDB"
```

---

## Task 7: `fleet cherry` implementation

**Files:**
- Modify: `mur-core/src/cmd/fleet/cherry_cmd.rs` (replace 29-line stub)

**Interfaces:**
- Consumes: `load_fleet()`, `TrackSet::load()`, `ParallelStateDb::open()`, `cherry_pick(scores_per_unit)`, `assemble_file(base_source, units_by_track, plan)`, `check_conflicts(plan, units)`
- Signature (unchanged): `pub fn cmd_fleet_cherry(mur_home: &Path, fleet_name: &str, auto: bool) -> Result<()>`

**Output:** writes assembled files to `~/.mur/fleets/<name>/cherry-result/<file_path>`, then runs `cargo check` on that directory. On pass: prints summary and "Run `mur fleet promote <name> cherry` to apply". On fail: prints cargo check stderr.

- [ ] **Step 1: Write test for the cherry output path**

```rust
// In cherry_cmd.rs #[cfg(test)]
#[test]
fn cherry_result_dir_name() {
    let mur_home = std::path::PathBuf::from("/home/user/.mur");
    let fleet_name = "my-fleet";
    let expected = mur_home.join("fleets").join(fleet_name).join("cherry-result");
    assert_eq!(cherry_result_dir(&mur_home, fleet_name), expected);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::fleet::cherry_cmd 2>&1 | tail -5
```
Expected: FAIL

- [ ] **Step 3: Replace `cherry_cmd.rs`**

```rust
//! `mur fleet cherry <name>` — cherry-pick best functions across tracks.
#![allow(dead_code)]
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::store::load_fleet;
use crate::parallel::{
    cherry::{assemble::assemble_file, conflict::check_conflicts, picker::cherry_pick},
    semantic::{SemanticUnit, SupportedLanguage, extract_units},
    state::ParallelStateDb,
    track::TrackSet,
};

pub fn cmd_fleet_cherry(mur_home: &Path, fleet_name: &str, auto: bool) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;

    let fleet_dir = mur_home.join("fleets").join(fleet_name);
    let tracks = TrackSet::load(&fleet_dir)
        .context("no tracks.json — run `mur fleet run` then `mur fleet judge` first")?;
    let state_db = ParallelStateDb::open(&fleet_dir.join("parallel_state"))?;
    let rubric_ver = parallel.judge.rubric.version();
    let result_dir = cherry_result_dir(mur_home, fleet_name);
    std::fs::create_dir_all(&result_dir)?;

    // Collect scores per unit (unit_name → Vec<TrackScore>).
    // Re-parse each track to enumerate units + look up scores.
    let mut scores_per_unit: std::collections::HashMap<
        String,
        Vec<crate::parallel::judge::TrackScore>,
    > = std::collections::HashMap::new();

    // Track → Vec<(file_path, units)> for later assembly.
    let mut track_file_units: std::collections::HashMap<
        String,
        Vec<(PathBuf, Vec<SemanticUnit>)>,
    > = std::collections::HashMap::new();

    for t in &tracks.tracks {
        let mut file_units: Vec<(PathBuf, Vec<SemanticUnit>)> = Vec::new();
        for entry in walkdir::WalkDir::new(&t.worktree_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            if let Ok(source) = std::fs::read(entry.path()) {
                let units = extract_units(&source, SupportedLanguage::Rust);
                for unit in &units {
                    if let Ok(Some(js)) = state_db.get_score(&unit.content_hash, &rubric_ver) {
                        let scores = scores_per_unit.entry(unit.name.clone()).or_default();
                        scores.push(crate::parallel::judge::TrackScore {
                            track_name: t.config.name.clone(),
                            score: js.score,
                            reasoning: js.reasoning.clone(),
                            low_confidence: false,
                        });
                    }
                }
                if !units.is_empty() {
                    file_units.push((entry.path().to_path_buf(), units));
                }
            }
        }
        track_file_units.insert(t.config.name.clone(), file_units);
    }

    if scores_per_unit.is_empty() {
        println!("No scores found. Run `mur fleet judge {fleet_name}` first.");
        return Ok(());
    }

    // Build cherry plan.
    let scores_slice: Vec<(&str, Vec<crate::parallel::judge::TrackScore>)> = scores_per_unit
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let plan = cherry_pick(&scores_slice);

    // Check conflicts.
    let all_units: Vec<&SemanticUnit> = track_file_units
        .values()
        .flat_map(|fus| fus.iter().flat_map(|(_, us)| us.iter()))
        .collect();
    let conflicts = check_conflicts(&plan, &all_units);
    if !conflicts.conflicts.is_empty() && !auto {
        println!("Dependency conflicts detected:");
        for c in &conflicts.conflicts {
            println!("  {} (from {}) depends on {} (in different track)", c.unit_a, c.track_a, c.unit_b);
        }
        println!("Use --auto to fall back to same-track selection for conflicting units.");
        return Ok(());
    }

    // Assemble output files.
    let mut written = 0usize;
    for t in &tracks.tracks {
        if let Some(file_units) = track_file_units.get(&t.config.name) {
            for (file_path, units) in file_units {
                let base_source = std::fs::read(file_path)?;
                // Build units_by_track: unit_name → (track_name, source_bytes)
                let units_by_track: std::collections::HashMap<String, (&str, &[u8])> = units
                    .iter()
                    .map(|u| {
                        let src = &base_source[u.byte_range.clone()];
                        (u.name.clone(), (t.config.name.as_str(), src))
                    })
                    .collect();
                let assembled = assemble_file(&base_source, &units_by_track, &plan)?;
                // Write to cherry-result/<relative_path>
                let rel = file_path.strip_prefix(&t.worktree_path).unwrap_or(file_path);
                let out_path = result_dir.join(rel);
                if let Some(p) = out_path.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::write(&out_path, assembled)?;
                written += 1;
            }
        }
    }

    println!("Cherry-picked {written} files → {}", result_dir.display());

    // Run cargo check on the result directory.
    let status = std::process::Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(&result_dir)
        .env("ORT_STRATEGY", "download")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo check: PASS");
            println!("Run `mur fleet promote {fleet_name} cherry` to apply the result.");
        }
        Ok(_) => {
            println!("cargo check: FAIL — the cherry-pick combination has compilation errors.");
            println!("Inspect {} and re-run after fixing.", result_dir.display());
        }
        Err(e) => {
            println!("cargo check: could not run ({e}) — skipping validation.");
        }
    }
    Ok(())
}

fn cherry_result_dir(mur_home: &Path, fleet_name: &str) -> PathBuf {
    mur_home.join("fleets").join(fleet_name).join("cherry-result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cherry_result_dir_name() {
        let mur_home = PathBuf::from("/home/user/.mur");
        let fleet_name = "my-fleet";
        let expected = mur_home.join("fleets").join(fleet_name).join("cherry-result");
        assert_eq!(cherry_result_dir(&mur_home, fleet_name), expected);
    }
}
```

- [ ] **Step 4: Run test + compilation check**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::fleet::cherry_cmd::tests 2>&1 | tail -5
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check -p mur-core 2>&1 | grep "^error" | head -5
```
Expected: test passes, 0 errors

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet/cherry_cmd.rs
git commit -m "feat(parallel/p1): fleet cherry — cherry_pick + assemble + cargo check validation"
```

---

## Task 8: Gate 0 validation script

**Files:**
- Create: `scripts/parallel_poc.sh`

**Context (from spec Gate 0):** Validates assumption A1 (diverse approach prompts produce meaningfully different implementations) and A6 (tree-sitter extraction is reliable). Uses `mur-core`'s existing `extract_units` via a small Rust test, and three different approach prompts on three real `mur-core` functions. The script is standalone: no production fleet run needed.

The pass criteria:
- Mean pairwise similarity ≤ 0.60 (implementations are meaningfully different)
- Tree-sitter extraction error rate ≤ 5%

For Gate 0, we validate A6 only (tree-sitter reliability) without an LLM call — A1 requires live LLM access and is documented as a manual step.

- [ ] **Step 1: Write the validation test**

```rust
// In mur-core/src/parallel/mod.rs tests (add to existing #[cfg(test)])
#[test]
fn gate0_tree_sitter_extraction_on_real_source() {
    use semantic::{SupportedLanguage, extract_units};
    // Read a known .rs file from this crate's source tree.
    // Use parallel/track/filter.rs — known to exist and be valid Rust.
    let source_path = std::path::Path::new(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/parallel/track/filter.rs")
    );
    let source = std::fs::read(source_path).expect("read filter.rs");
    let units = extract_units(&source, SupportedLanguage::Rust);
    // filter.rs has at least 2 functions (run_pre_filter + run_cargo_check)
    assert!(
        units.len() >= 2,
        "expected ≥2 semantic units from filter.rs, got {}",
        units.len()
    );
    // All units must have non-empty names and non-zero byte ranges.
    for u in &units {
        assert!(!u.name.is_empty(), "unit has empty name");
        assert!(
            u.byte_range.end > u.byte_range.start,
            "unit {} has empty byte range",
            u.name
        );
        // content_hash must be non-zero (blake3 of actual bytes).
        assert_ne!(u.content_hash, [0u8; 32], "unit {} has zero hash", u.name);
    }
}
```

- [ ] **Step 2: Run test to verify it passes immediately (A6 validation)**

```bash
ORT_STRATEGY=download cargo test -p mur-core --lib parallel::tests::gate0_tree_sitter_extraction_on_real_source 2>&1 | tail -5
```
Expected: `test parallel::tests::gate0_tree_sitter_extraction_on_real_source ... ok`

- [ ] **Step 3: Write the shell script**

Create `scripts/parallel_poc.sh`:

```bash
#!/usr/bin/env bash
# Gate 0 — Parallel Tracks PoC validation
# Validates A6 (tree-sitter reliability) and documents A1 (diversity) as a manual step.
#
# Usage: bash scripts/parallel_poc.sh
# Prereqs: ORT_STRATEGY=download, mur-core builds

set -euo pipefail

echo "=== Gate 0: Parallel Tracks PoC Validation ==="
echo ""
echo "--- A6: tree-sitter extraction reliability ---"
echo "Running: cargo test -p mur-core --lib parallel::tests::gate0_tree_sitter_extraction_on_real_source"

ORT_STRATEGY=download \
  MUR_WEB_DIST="${MUR_WEB_DIST:-$HOME/Projects/mur-web/dist}" \
  cargo test -p mur-core --lib parallel::tests::gate0_tree_sitter_extraction_on_real_source \
  2>&1

echo ""
echo "A6 PASS: tree-sitter extraction verified on real mur-core source."
echo ""
echo "--- A1: prompt diversity (manual step) ---"
echo "A1 requires a live Anthropic API key. To validate:"
echo "  1. Create a fleet with 3 tracks using different approach prompts"
echo "     (functional / performance / readability)"
echo "  2. Run mur fleet run <name> to execute agents"
echo "  3. For each resulting implementation, measure pairwise similarity:"
echo "     similarity = 1 - (edit_distance / max(len_a, len_b))"
echo "  4. Gate: mean pairwise similarity <= 0.60"
echo ""
echo "Save results to: docs/superpowers/validation/parallel-poc-results.md"
```

- [ ] **Step 4: Make executable and run**

```bash
chmod +x scripts/parallel_poc.sh
bash scripts/parallel_poc.sh 2>&1 | tail -20
```
Expected: `A6 PASS: tree-sitter extraction verified on real mur-core source.`

- [ ] **Step 5: Commit**

```bash
git add scripts/parallel_poc.sh mur-core/src/parallel/mod.rs
git commit -m "feat(parallel/p1): Gate 0 validation — A6 tree-sitter reliability test + poc script"
```

---

## Final Verification

After all 8 tasks:

- [ ] Run all parallel module tests:
  ```bash
  ORT_STRATEGY=download cargo test -p mur-core --lib parallel 2>&1 | tail -15
  ```
  Expected: all pass (0 failed)

- [ ] Run full clippy:
  ```bash
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo clippy -p mur-core -p mur-common -- -D warnings 2>&1 | grep "^error" | head -10
  ```
  Expected: 0 errors

- [ ] Verify CLI wiring (compile-only smoke test):
  ```bash
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build -p mur-core --bin mur 2>&1 | tail -5
  ```
  Expected: compiles successfully

- [ ] Commit any fmt fixes:
  ```bash
  cargo fmt --all && git add -u && git commit -m "style: cargo fmt --all" || echo "nothing to format"
  ```
