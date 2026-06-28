# MUR Parallel Tracks — P0 + P1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement speculative parallel agent execution with function-level cherry-pick for `mur fleet`, gated by two empirical validation experiments before any production code ships.

**Architecture:** Gate 0 PoC validates agent diversity and tree-sitter reliability. P0 builds the foundation: `ParallelBackend` trait, `SemanticUnit` extraction, blake3 CAS, and LMDB state store. Gate 1 validates CyclicJudge stability. P1 wires everything into the fleet CLI: `fleet create --parallel`, `fleet compare`, `fleet judge`, `fleet cherry`, `fleet promote`.

**Tech Stack:** Rust (mur-core, mur-common), tree-sitter 0.24 + tree-sitter-rust 0.23, blake3 1, heed 0.20 (LMDB), git worktrees, Python 3.11+ (validation scripts only — not production code)

## Scope

This plan covers **Gate 0 validation + P0 Foundation + Gate 1 validation + P1 Speculative Parallel CLI**. Later phases are separate plans:
- P1.5 (Platform COW: APFS/Btrfs/ReFS)
- P2 (ZFS backend via socket)
- P2.5 (Semantic Partition mode)
- P3 (Loro CRDT)

## Global Constraints

- Rust edition 2024; `let` chains stable (`if let … && let …`)
- Single source file ≤ 800 lines — split into submodules at limit
- Worktrees under `.worktrees/` (never `.mur/`)
- Brand `MUR` uppercase in all user-visible strings; CLI binary stays `mur`
- Tests: `ORT_STRATEGY=download cargo nextest run -p mur-core <test_name>`
- Build: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo build`
- No hardcoded values — use constants or config fields
- `parallel: Option<ParallelConfig>` must be `skip_serializing_if = "Option::is_none"` so existing fleet.yaml files still parse

---

## File Map

### New files

| File | Responsibility |
|------|---------------|
| `scripts/parallel_poc.py` | Gate 0: measure agent diversity + tree-sitter error rate |
| `scripts/judge_reliability.py` | Gate 1: measure CyclicJudge score stability |
| `docs/superpowers/validation/parallel-poc-results.md` | Gate 0 results (human-written after experiment) |
| `docs/superpowers/validation/judge-reliability-results.md` | Gate 1 results (human-written after experiment) |
| `mur-common/src/parallel.rs` | `ParallelConfig`, `TrackConfig`, `JudgeConfig`, `Rubric`, `ParallelMode` |
| `mur-core/src/parallel/mod.rs` | Module declaration, `ParallelSession` |
| `mur-core/src/parallel/semantic/mod.rs` | `SemanticUnit`, `UnitKind`, `SupportedLanguage` types |
| `mur-core/src/parallel/semantic/tree_sitter.rs` | tree-sitter parsing → `SemanticUnit[]` |
| `mur-core/src/parallel/semantic/cas.rs` | blake3 hashing, identity check |
| `mur-core/src/parallel/state/mod.rs` | re-exports |
| `mur-core/src/parallel/state/lmdb.rs` | LMDB via heed: sessions, units, scores |
| `mur-core/src/parallel/backend/mod.rs` | `ParallelBackend` trait |
| `mur-core/src/parallel/backend/git_worktree.rs` | `GitWorktreeBackend` impl |
| `mur-core/src/parallel/backend/detect.rs` | auto-detect best available backend |
| `mur-core/src/parallel/track/mod.rs` | `Track`, `TrackSet` |
| `mur-core/src/parallel/track/worktree.rs` | worktree create/destroy per track |
| `mur-core/src/parallel/track/diversity.rs` | inject `approach` into fleet member config |
| `mur-core/src/parallel/track/filter.rs` | `cargo check` pre-filter |
| `mur-core/src/parallel/judge/mod.rs` | `JudgeTask`, `JudgeResult` |
| `mur-core/src/parallel/judge/cyclic.rs` | `CyclicJudge` |
| `mur-core/src/parallel/cherry/mod.rs` | `CherryPlan`, `UnitSelection` |
| `mur-core/src/parallel/cherry/picker.rs` | greedy selection |
| `mur-core/src/parallel/cherry/conflict.rs` | API compatibility signature check |
| `mur-core/src/parallel/cherry/assemble.rs` | reconstruct file from selected units |
| `mur-core/src/cmd/fleet/compare.rs` | `fleet compare` command |
| `mur-core/src/cmd/fleet/judge_cmd.rs` | `fleet judge` command |
| `mur-core/src/cmd/fleet/cherry_cmd.rs` | `fleet cherry` command |

### Modified files

| File | Change |
|------|--------|
| `mur-core/Cargo.toml` | Add `tree-sitter`, `tree-sitter-rust`, `blake3`, `heed` |
| `mur-common/src/lib.rs` | Add `pub mod parallel;` |
| `mur-common/src/fleet.rs` | Add `parallel: Option<ParallelConfig>` field to `Fleet` |
| `mur-core/src/lib.rs` | Add `pub mod parallel;` |
| `mur-core/src/cmd/fleet/mod.rs` | Add `pub mod compare`, `judge_cmd`, `cherry_cmd` |
| `mur-core/src/cmd/fleet/create.rs` | Add `--parallel`, `--tracks N` flags |
| `mur-core/src/cmd/fleet/store.rs` | No change (reuses existing `save_fleet`/`load_fleet`) |

---

## Task 0: Gate 0 — Diversity & Tree-Sitter PoC

**⛔ BLOCKING: Do not start Task 1 until this gate passes.**

**Files:**
- Create: `scripts/parallel_poc.py`
- Create: `docs/superpowers/validation/parallel-poc-results.md` (after running)

**Pass criteria (from spec):**
- Mean pairwise similarity ≤ 0.60
- Tree-sitter extraction error rate ≤ 5%
- ≥ 2 of 3 test functions show ≥ 1 structurally different approach

- [ ] **Step 1: Install PoC dependencies**

```bash
pip install anthropic tree-sitter tree-sitter-rust
```

Expected: all packages install without error.

- [ ] **Step 2: Write `scripts/parallel_poc.py`**

```python
#!/usr/bin/env python3
"""Gate 0: validate agent diversity and tree-sitter extraction reliability."""
import os, re, difflib, statistics
import anthropic
from tree_sitter import Language, Parser
import tree_sitter_rust

APPROACHES = [
    "Prefer functional style: use Iterator combinators, avoid mutable state, compose small functions.",
    "Performance first: static dispatch over dyn, minimize heap allocation, consider cache locality.",
    "Readability first: clear naming, rich error types, full doc comments, test-driven design.",
]

TEST_FUNCTIONS = [
    # (description, prompt to implement)
    ("word_count", "Write a Rust function `fn word_count(s: &str) -> usize` that counts words."),
    ("is_palindrome", "Write a Rust function `fn is_palindrome(s: &str) -> bool` that checks if a string is a palindrome (ignore case, ignore non-alphanumeric)."),
    ("flatten_nested", "Write a Rust function `fn flatten(v: Vec<Vec<i32>>) -> Vec<i32>` that flattens a nested vec."),
]

def get_implementation(client, func_desc: str, approach: str) -> str:
    resp = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=1000,
        messages=[{
            "role": "user",
            "content": f"{func_desc}\n\nApproach: {approach}\n\nRespond with ONLY the Rust function, no explanation, no markdown fences."
        }]
    )
    return resp.content[0].text.strip()

def extract_functions(source: str) -> list[str]:
    """Return list of top-level function bodies via tree-sitter."""
    lang = Language(tree_sitter_rust.language())
    parser = Parser(lang)
    tree = parser.parse(source.encode())
    fns = []
    for child in tree.root_node().children:
        if child.type == "function_item":
            fns.append(source[child.start_byte:child.end_byte])
    return fns

def pairwise_similarity(impls: list[str]) -> list[float]:
    scores = []
    for i in range(len(impls)):
        for j in range(i + 1, len(impls)):
            ratio = difflib.SequenceMatcher(None, impls[i], impls[j]).ratio()
            scores.append(ratio)
    return scores

def main():
    client = anthropic.Anthropic()
    results = []

    for func_name, func_desc in TEST_FUNCTIONS:
        print(f"\n=== {func_name} ===")
        impls, extract_errors = [], 0

        for approach in APPROACHES:
            code = get_implementation(client, func_desc, approach)
            fns = extract_functions(code)
            if not fns:
                extract_errors += 1
                print(f"  EXTRACT ERROR for approach: {approach[:40]}...")
                impls.append(code)  # use raw as fallback
            else:
                impls.append(fns[0])

        sims = pairwise_similarity(impls)
        mean_sim = statistics.mean(sims) if sims else 1.0
        any_structural_diff = any(s < 0.70 for s in sims)

        print(f"  Pairwise similarities: {[round(s, 3) for s in sims]}")
        print(f"  Mean similarity: {mean_sim:.3f} (target ≤ 0.60)")
        print(f"  Extract errors: {extract_errors} (target ≤ 5%)")
        print(f"  Structural diff found: {any_structural_diff}")

        results.append({
            "func": func_name,
            "mean_sim": mean_sim,
            "extract_errors": extract_errors,
            "structural_diff": any_structural_diff,
        })

    print("\n=== GATE 0 SUMMARY ===")
    all_mean_sim = statistics.mean(r["mean_sim"] for r in results)
    total_errors = sum(r["extract_errors"] for r in results)
    error_rate = total_errors / (len(TEST_FUNCTIONS) * len(APPROACHES))
    funcs_with_diff = sum(1 for r in results if r["structural_diff"])

    print(f"Overall mean similarity: {all_mean_sim:.3f} (PASS if ≤ 0.60)")
    print(f"Tree-sitter error rate: {error_rate:.1%} (PASS if ≤ 5%)")
    print(f"Functions with structural diff: {funcs_with_diff}/{len(TEST_FUNCTIONS)} (PASS if ≥ 2)")

    passed = all_mean_sim <= 0.60 and error_rate <= 0.05 and funcs_with_diff >= 2
    print(f"\nGATE 0: {'✅ PASS' if passed else '❌ FAIL — do not proceed to Task 1'}")

if __name__ == "__main__":
    main()
```

- [ ] **Step 3: Run the PoC**

```bash
ANTHROPIC_API_KEY=$(cat ~/.config/anthropic_key 2>/dev/null || echo $ANTHROPIC_API_KEY) \
  python3 scripts/parallel_poc.py
```

Expected: prints `GATE 0: ✅ PASS` with all three criteria met.

**If FAIL:** stop. Record what failed in `docs/superpowers/validation/parallel-poc-results.md` and re-design the diversity strategy before proceeding.

- [ ] **Step 4: Record results**

Write findings to `docs/superpowers/validation/parallel-poc-results.md`:
- Paste actual output numbers
- Note which functions had structural differences
- Confirm gate passed

- [ ] **Step 5: Commit**

```bash
git add scripts/parallel_poc.py docs/superpowers/validation/parallel-poc-results.md
git commit -m "feat(parallel): Gate 0 PoC — diversity + tree-sitter validation"
```

---

## Task 1: Dependencies + Module Skeleton

**Files:**
- Modify: `mur-core/Cargo.toml`
- Modify: `mur-common/src/lib.rs`
- Modify: `mur-core/src/lib.rs`
- Create: `mur-common/src/parallel.rs`
- Create: `mur-core/src/parallel/mod.rs`

- [ ] **Step 1: Add dependencies to `mur-core/Cargo.toml`**

Add after the existing `serde_json` line:

```toml
tree-sitter = "0.24"
tree-sitter-rust = "0.23"
blake3 = "1"
heed = { version = "0.20", features = ["serde-json"] }
hex = "0.4"
```

- [ ] **Step 2: Write `mur-common/src/parallel.rs`**

```rust
//! Parallel tracks config — extended fleet.yaml `parallel:` section.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelMode {
    Speculative,
    Partition,
}

impl Default for ParallelMode {
    fn default() -> Self { Self::Speculative }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackConfig {
    pub name: String,
    pub approach: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rubric {
    #[serde(default = "default_correctness")]
    pub correctness: f32,
    #[serde(default = "default_design")]
    pub design: f32,
    #[serde(default = "default_maintainability")]
    pub maintainability: f32,
    #[serde(default = "default_security")]
    pub security: f32,
}

fn default_correctness() -> f32 { 0.40 }
fn default_design() -> f32 { 0.30 }
fn default_maintainability() -> f32 { 0.20 }
fn default_security() -> f32 { 0.10 }

impl Default for Rubric {
    fn default() -> Self {
        Self {
            correctness: default_correctness(),
            design: default_design(),
            maintainability: default_maintainability(),
            security: default_security(),
        }
    }
}

impl Rubric {
    pub fn version(&self) -> String {
        format!(
            "c{:.2}d{:.2}m{:.2}s{:.2}",
            self.correctness, self.design, self.maintainability, self.security
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeConfig {
    pub model: String,
    #[serde(default)]
    pub rubric: Rubric,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreFilterKind {
    CargoCheck,
    CargoClippyDeny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelConfig {
    #[serde(default)]
    pub mode: ParallelMode,
    pub tracks: Vec<TrackConfig>,
    pub judge: JudgeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_filter: Vec<PreFilterKind>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rubric_version_is_stable() {
        let r = Rubric::default();
        assert_eq!(r.version(), "c0.40d0.30m0.20s0.10");
    }

    #[test]
    fn parallel_config_roundtrips_yaml() {
        let yaml = r#"
mode: speculative
judge:
  model: claude-opus-4-8
tracks:
  - name: track-a
    approach: "functional style"
  - name: track-b
    approach: "performance first"
"#;
        let cfg: ParallelConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.tracks.len(), 2);
        assert_eq!(cfg.mode, ParallelMode::Speculative);
        let back = serde_yaml::to_string(&cfg).unwrap();
        let cfg2: ParallelConfig = serde_yaml::from_str(&back).unwrap();
        assert_eq!(cfg, cfg2);
    }
}
```

- [ ] **Step 3: Add `pub mod parallel;` to `mur-common/src/lib.rs`**

Find the block of `pub mod` declarations and add:
```rust
pub mod parallel;
```

- [ ] **Step 4: Write `mur-core/src/parallel/mod.rs`**

```rust
//! Speculative parallel agent execution — P0/P1 foundation.

pub mod backend;
pub mod cherry;
pub mod judge;
pub mod semantic;
pub mod state;
pub mod track;
```

- [ ] **Step 5: Add `pub mod parallel;` to `mur-core/src/lib.rs`**

- [ ] **Step 6: Verify it compiles**

```bash
ORT_STRATEGY=download cargo check -p mur-common -p mur-core 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 7: Commit**

```bash
git add mur-core/Cargo.toml mur-common/src/parallel.rs mur-common/src/lib.rs \
        mur-core/src/parallel/mod.rs mur-core/src/lib.rs
git commit -m "feat(parallel): P0 skeleton — ParallelConfig types + module layout"
```

---

## Task 2: SemanticUnit + Tree-Sitter Extraction

**Files:**
- Create: `mur-core/src/parallel/semantic/mod.rs`
- Create: `mur-core/src/parallel/semantic/tree_sitter.rs`

**Interfaces:**
- Produces: `extract_units(source: &[u8], lang: SupportedLanguage) -> Result<Vec<SemanticUnit>>`

- [ ] **Step 1: Write `mur-core/src/parallel/semantic/mod.rs`**

```rust
pub mod cas;
pub mod tree_sitter_parse;

use std::ops::Range;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Fn,
    Struct,
    Impl,
    Trait,
    Enum,
    Const,
    Test,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticUnit {
    pub kind: UnitKind,
    pub name: String,
    pub byte_range: Range<usize>,
    pub line_range: Range<u32>,
    /// blake3 hash of the source bytes in this range.
    pub content_hash: [u8; 32],
    /// Names of other top-level units this unit references.
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Rust,
}

pub use tree_sitter_parse::extract_units;
```

- [ ] **Step 2: Write failing test in `mur-core/src/parallel/semantic/tree_sitter_parse.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::semantic::SupportedLanguage;

    const SIMPLE_RUST: &str = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

struct Point {
    x: f64,
    y: f64,
}

fn main() {}
"#;

    #[test]
    fn extracts_top_level_fns_and_structs() {
        let units = extract_units(SIMPLE_RUST.as_bytes(), SupportedLanguage::Rust).unwrap();
        let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"add"), "missing fn add: {names:?}");
        assert!(names.contains(&"Point"), "missing struct Point: {names:?}");
        assert!(names.contains(&"main"), "missing fn main: {names:?}");
    }

    #[test]
    fn content_hash_differs_for_different_implementations() {
        let src_a = b"fn hello() { println!(\"a\"); }";
        let src_b = b"fn hello() { println!(\"b\"); }";
        let units_a = extract_units(src_a, SupportedLanguage::Rust).unwrap();
        let units_b = extract_units(src_b, SupportedLanguage::Rust).unwrap();
        assert_eq!(units_a.len(), 1);
        assert_eq!(units_b.len(), 1);
        assert_ne!(units_a[0].content_hash, units_b[0].content_hash);
    }

    #[test]
    fn identical_implementations_have_same_hash() {
        let src = b"fn greet() { println!(\"hello\"); }";
        let units_1 = extract_units(src, SupportedLanguage::Rust).unwrap();
        let units_2 = extract_units(src, SupportedLanguage::Rust).unwrap();
        assert_eq!(units_1[0].content_hash, units_2[0].content_hash);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::semantic::tree_sitter_parse 2>&1 | tail -10
```

Expected: compile error (file not found / function not defined).

- [ ] **Step 4: Write `mur-core/src/parallel/semantic/tree_sitter_parse.rs`**

```rust
use anyhow::{Result, anyhow};
use tree_sitter::{Node, Parser};
use super::{SemanticUnit, SupportedLanguage, UnitKind};

pub fn extract_units(source: &[u8], lang: SupportedLanguage) -> Result<Vec<SemanticUnit>> {
    let mut parser = Parser::new();
    let ts_lang = match lang {
        SupportedLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
    };
    parser.set_language(&ts_lang)?;
    let tree = parser.parse(source, None).ok_or_else(|| anyhow!("tree-sitter parse failed"))?;
    let mut units = Vec::new();
    collect_top_level(source, tree.root_node(), &mut units);
    Ok(units)
}

fn collect_top_level(source: &[u8], root: Node, units: &mut Vec<SemanticUnit>) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = match child.kind() {
            "function_item" => UnitKind::Fn,
            "struct_item" => UnitKind::Struct,
            "impl_item" => UnitKind::Impl,
            "trait_item" => UnitKind::Trait,
            "enum_item" => UnitKind::Enum,
            "const_item" => UnitKind::Const,
            _ => continue,
        };
        let name = extract_name(source, child);
        let byte_range = child.byte_range();
        let start_line = child.start_position().row as u32;
        let end_line = child.end_position().row as u32;
        let content = &source[byte_range.clone()];
        let content_hash = *blake3::hash(content).as_bytes();
        units.push(SemanticUnit {
            kind,
            name,
            byte_range,
            line_range: start_line..end_line,
            content_hash,
            dependencies: Vec::new(),
        });
    }
}

fn extract_name(source: &[u8], node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            if let Ok(s) = std::str::from_utf8(&source[child.byte_range()]) {
                return s.to_string();
            }
        }
    }
    format!("<unknown@{}>", node.start_position().row)
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::semantic 2>&1 | tail -10
```

Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/parallel/semantic/
git commit -m "feat(parallel): SemanticUnit extraction via tree-sitter"
```

---

## Task 3: Blake3 CAS

**Files:**
- Create: `mur-core/src/parallel/semantic/cas.rs`

**Interfaces:**
- Produces: `fn units_differ(a: &SemanticUnit, b: &SemanticUnit) -> bool`
- Produces: `fn group_by_identity<'a>(units_per_track: &'a [(&'a str, Vec<SemanticUnit>)]) -> UnitGroups<'a>`

- [ ] **Step 1: Write failing test**

In `mur-core/src/parallel/semantic/cas.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::semantic::{SemanticUnit, UnitKind};

    fn make_unit(name: &str, hash: [u8; 32]) -> SemanticUnit {
        SemanticUnit {
            kind: UnitKind::Fn,
            name: name.to_string(),
            byte_range: 0..10,
            line_range: 0..1,
            content_hash: hash,
            dependencies: vec![],
        }
    }

    #[test]
    fn same_hash_not_different() {
        let hash = [1u8; 32];
        assert!(!units_differ(&make_unit("f", hash), &make_unit("f", hash)));
    }

    #[test]
    fn different_hash_is_different() {
        let a = make_unit("f", [1u8; 32]);
        let b = make_unit("f", [2u8; 32]);
        assert!(units_differ(&a, &b));
    }

    #[test]
    fn groups_identical_units_as_no_judge_needed() {
        let hash = [42u8; 32];
        let tracks = vec![
            ("track-a", vec![make_unit("authenticate", hash)]),
            ("track-b", vec![make_unit("authenticate", hash)]),
        ];
        let groups = group_by_identity(&tracks);
        assert!(groups.needs_judge.is_empty(), "identical units should not need judging");
        assert_eq!(groups.skip.len(), 1);
    }

    #[test]
    fn groups_different_units_as_judge_needed() {
        let tracks = vec![
            ("track-a", vec![make_unit("authenticate", [1u8; 32])]),
            ("track-b", vec![make_unit("authenticate", [2u8; 32])]),
        ];
        let groups = group_by_identity(&tracks);
        assert_eq!(groups.needs_judge.len(), 1);
        assert!(groups.skip.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::semantic::cas 2>&1 | tail -5
```

Expected: compile error.

- [ ] **Step 3: Write `mur-core/src/parallel/semantic/cas.rs`**

```rust
use std::collections::HashMap;
use super::SemanticUnit;

pub fn units_differ(a: &SemanticUnit, b: &SemanticUnit) -> bool {
    a.content_hash != b.content_hash
}

/// A unit name that all tracks agree on (identical content hash).
pub struct SkipUnit {
    pub name: String,
    pub content_hash: [u8; 32],
}

/// A unit name where tracks have different implementations → needs LLM judge.
pub struct JudgeGroup {
    pub name: String,
    /// (track_name, unit) ordered by track index
    pub per_track: Vec<(String, SemanticUnit)>,
}

pub struct UnitGroups {
    pub skip: Vec<SkipUnit>,
    pub needs_judge: Vec<JudgeGroup>,
}

pub fn group_by_identity(tracks: &[(&str, Vec<SemanticUnit>)]) -> UnitGroups {
    // Collect all unit names across all tracks
    let mut all_names: Vec<String> = tracks
        .iter()
        .flat_map(|(_, units)| units.iter().map(|u| u.name.clone()))
        .collect();
    all_names.sort();
    all_names.dedup();

    let mut skip = Vec::new();
    let mut needs_judge = Vec::new();

    for name in all_names {
        let per_track: Vec<(String, SemanticUnit)> = tracks
            .iter()
            .filter_map(|(track_name, units)| {
                units.iter().find(|u| u.name == name).map(|u| (track_name.to_string(), u.clone()))
            })
            .collect();

        // Collect unique hashes
        let mut hashes: Vec<[u8; 32]> = per_track.iter().map(|(_, u)| u.content_hash).collect();
        hashes.sort();
        hashes.dedup();

        if hashes.len() == 1 {
            skip.push(SkipUnit { name, content_hash: hashes[0] });
        } else {
            needs_judge.push(JudgeGroup { name, per_track });
        }
    }

    UnitGroups { skip, needs_judge }
}
```

- [ ] **Step 4: Add `pub mod cas;` to `mur-core/src/parallel/semantic/mod.rs`**

- [ ] **Step 5: Run tests**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::semantic::cas 2>&1 | tail -5
```

Expected: 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/parallel/semantic/cas.rs mur-core/src/parallel/semantic/mod.rs
git commit -m "feat(parallel): blake3 CAS — group units by identity, skip re-judging identical impls"
```

---

## Task 4: LMDB State Store

**Files:**
- Create: `mur-core/src/parallel/state/mod.rs`
- Create: `mur-core/src/parallel/state/lmdb.rs`

**Interfaces:**
- Produces: `ParallelStateDb::open(path: &Path) -> Result<Self>`
- Produces: `db.get_score(hash: &[u8;32], rubric_ver: &str) -> Result<Option<JudgeScore>>`
- Produces: `db.put_score(hash: &[u8;32], rubric_ver: &str, score: &JudgeScore) -> Result<()>`

- [ ] **Step 1: Write `mur-core/src/parallel/state/mod.rs`**

```rust
pub mod lmdb;
pub use lmdb::ParallelStateDb;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeScore {
    pub score: f32,
    pub reasoning: String,
    pub model: String,
    pub ts: u64,
}
```

- [ ] **Step 2: Write failing test in `mur-core/src/parallel/state/lmdb.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::state::JudgeScore;

    #[test]
    fn score_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = ParallelStateDb::open(dir.path()).unwrap();
        let hash = [7u8; 32];
        let score = JudgeScore {
            score: 8.5,
            reasoning: "good design".into(),
            model: "claude-opus-4-8".into(),
            ts: 1_700_000_000,
        };
        db.put_score(&hash, "v1", &score).unwrap();
        let retrieved = db.get_score(&hash, "v1").unwrap().unwrap();
        assert!((retrieved.score - 8.5).abs() < 0.001);
        assert_eq!(retrieved.reasoning, "good design");
    }

    #[test]
    fn missing_score_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = ParallelStateDb::open(dir.path()).unwrap();
        assert!(db.get_score(&[0u8; 32], "v1").unwrap().is_none());
    }
}
```

- [ ] **Step 3: Add `tempfile` to `mur-core/Cargo.toml` dev-dependencies**

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Run test to verify it fails**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::state 2>&1 | tail -5
```

Expected: compile error (ParallelStateDb not defined).

- [ ] **Step 5: Write `mur-core/src/parallel/state/lmdb.rs`**

```rust
use std::path::Path;
use anyhow::Result;
use heed::{Database, Env, EnvOpenOptions};
use heed::types::{Bytes, SerdeJson};
use super::JudgeScore;

pub struct ParallelStateDb {
    env: Env,
    scores: Database<Bytes, SerdeJson<JudgeScore>>,
}

impl ParallelStateDb {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        // SAFETY: standard heed usage; no other process opens this env concurrently.
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(50 * 1024 * 1024) // 50 MB
                .max_dbs(3)
                .open(dir)?
        };
        let mut wtxn = env.write_txn()?;
        let scores = env.create_database(&mut wtxn, Some("scores"))?;
        wtxn.commit()?;
        Ok(Self { env, scores })
    }

    fn score_key(content_hash: &[u8; 32], rubric_version: &str) -> Vec<u8> {
        let mut k = hex::encode(content_hash).into_bytes();
        k.push(b':');
        k.extend_from_slice(rubric_version.as_bytes());
        k
    }

    pub fn get_score(&self, content_hash: &[u8; 32], rubric_version: &str) -> Result<Option<JudgeScore>> {
        let rtxn = self.env.read_txn()?;
        let key = Self::score_key(content_hash, rubric_version);
        Ok(self.scores.get(&rtxn, &key)?)
    }

    pub fn put_score(&self, content_hash: &[u8; 32], rubric_version: &str, score: &JudgeScore) -> Result<()> {
        let mut wtxn = self.env.write_txn()?;
        let key = Self::score_key(content_hash, rubric_version);
        self.scores.put(&mut wtxn, &key, score)?;
        wtxn.commit()?;
        Ok(())
    }
}
```

- [ ] **Step 6: Run tests**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::state 2>&1 | tail -5
```

Expected: 2 tests pass.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/parallel/state/
git commit -m "feat(parallel): LMDB state store — CAS score cache with content-hash key"
```

---

## Task 5: ParallelBackend Trait + GitWorktreeBackend

**Files:**
- Create: `mur-core/src/parallel/backend/mod.rs`
- Create: `mur-core/src/parallel/backend/git_worktree.rs`
- Create: `mur-core/src/parallel/backend/detect.rs`

**Interfaces:**
- Produces: `trait ParallelBackend` (5 methods)
- Produces: `GitWorktreeBackend::new(repo_root: PathBuf) -> Self`
- Produces: `fn detect_backend(project: &Path) -> Box<dyn ParallelBackend>`

- [ ] **Step 1: Write `mur-core/src/parallel/backend/mod.rs`**

```rust
pub mod detect;
pub mod git_worktree;

use std::path::{Path, PathBuf};
use anyhow::Result;

pub trait ParallelBackend: Send + Sync {
    fn create_track(&self, name: &str) -> Result<PathBuf>;
    fn base_snapshot(&self, track: &Path) -> Result<String>;
    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>>;
    fn promote(&self, track: &Path, target: &Path) -> Result<()>;
    fn destroy(&self, track: &Path) -> Result<()>;
}

pub use detect::detect_backend;
pub use git_worktree::GitWorktreeBackend;
```

- [ ] **Step 2: Write failing test in `git_worktree.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_destroy_worktree() {
        // Requires running inside a git repo — skip in CI if not
        let repo = std::env::current_dir().unwrap();
        if !repo.join(".git").exists() && !repo.join("../../.git").exists() {
            eprintln!("skip: not in a git repo");
            return;
        }
        let backend = GitWorktreeBackend::new(
            // Walk up to find actual git root
            find_git_root(&repo).unwrap_or(repo)
        );
        let track = backend.create_track("test-parallel-track-tmp").unwrap();
        assert!(track.exists());
        backend.destroy(&track).unwrap();
        assert!(!track.exists());
    }
}
```

- [ ] **Step 3: Write `mur-core/src/parallel/backend/git_worktree.rs`**

```rust
use std::path::{Path, PathBuf};
use std::process::Command;
use anyhow::{Context, Result};
use super::ParallelBackend;

pub struct GitWorktreeBackend {
    repo_root: PathBuf,
}

impl GitWorktreeBackend {
    pub fn new(repo_root: PathBuf) -> Self { Self { repo_root } }
}

impl ParallelBackend for GitWorktreeBackend {
    fn create_track(&self, name: &str) -> Result<PathBuf> {
        let path = self.repo_root.join(".worktrees").join(name);
        Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .current_dir(&self.repo_root)
            .status()
            .context("git worktree add")?;
        Ok(path)
    }

    fn base_snapshot(&self, track: &Path) -> Result<String> {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(track)
            .output()
            .context("git rev-parse")?;
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>> {
        let out = Command::new("git")
            .args(["diff", "--name-only", since_snapshot, "HEAD"])
            .current_dir(track)
            .output()
            .context("git diff")?;
        let files = String::from_utf8(out.stdout)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| track.join(l))
            .collect();
        Ok(files)
    }

    fn promote(&self, track: &Path, target: &Path) -> Result<()> {
        // Copy changed files from track worktree into target directory
        let since = self.base_snapshot(track)?;
        let files = self.diff_files(track, &since)?;
        for src in files {
            let rel = src.strip_prefix(track).context("strip prefix")?;
            let dst = target.join(rel);
            if let Some(parent) = dst.parent() { std::fs::create_dir_all(parent)?; }
            std::fs::copy(&src, &dst)?;
        }
        Ok(())
    }

    fn destroy(&self, track: &Path) -> Result<()> {
        Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(track)
            .current_dir(&self.repo_root)
            .status()
            .context("git worktree remove")?;
        Ok(())
    }
}

pub fn find_git_root(from: &Path) -> Option<PathBuf> {
    let mut cur = from.to_path_buf();
    loop {
        if cur.join(".git").exists() { return Some(cur); }
        if !cur.pop() { return None; }
    }
}
```

- [ ] **Step 4: Write `mur-core/src/parallel/backend/detect.rs`**

```rust
use std::path::{Path, PathBuf};
use super::{ParallelBackend, GitWorktreeBackend, git_worktree::find_git_root};

/// Returns the best available backend. Always falls back to GitWorktreeBackend.
/// P2 will add ZFS socket detection above the fallback.
pub fn detect_backend(project: &Path) -> Box<dyn ParallelBackend> {
    let root = find_git_root(project)
        .unwrap_or_else(|| project.to_path_buf());
    Box::new(GitWorktreeBackend::new(root))
}
```

- [ ] **Step 5: Run test**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::backend 2>&1 | tail -10
```

Expected: 1 test passes (or skips gracefully if not in git repo).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/parallel/backend/
git commit -m "feat(parallel): ParallelBackend trait + GitWorktreeBackend"
```

---

## Task 6: Gate 1 — Judge Reliability Experiment

**⛔ BLOCKING: Do not start Task 7 until this gate passes.**

**Files:**
- Create: `scripts/judge_reliability.py`
- Create: `docs/superpowers/validation/judge-reliability-results.md` (after running)

- [ ] **Step 1: Write `scripts/judge_reliability.py`**

```python
#!/usr/bin/env python3
"""Gate 1: measure CyclicJudge score stability across ordering permutations."""
import anthropic, statistics, json, itertools

RUBRIC = {"correctness": 0.4, "design": 0.3, "maintainability": 0.2, "security": 0.1}
RUBRIC_DESC = "correctness (40%), design (30%), maintainability (20%), security (10%)"

IMPL_PAIRS = [
    {
        "name": "word_count",
        "a": 'fn word_count(s: &str) -> usize { s.split_whitespace().count() }',
        "b": 'fn word_count(s: &str) -> usize { let mut n = 0; let mut in_word = false; for c in s.chars() { if c.is_whitespace() { in_word = false; } else if !in_word { n += 1; in_word = true; } } n }',
    },
    {
        "name": "is_palindrome",
        "a": 'fn is_palindrome(s: &str) -> bool { let clean: String = s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_lowercase().next().unwrap()).collect(); clean == clean.chars().rev().collect::<String>() }',
        "b": 'fn is_palindrome(s: &str) -> bool { let v: Vec<char> = s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect(); v.iter().zip(v.iter().rev()).all(|(a, b)| a == b) }',
    },
    {
        "name": "max_in_list",
        "a": 'fn max_val(v: &[i32]) -> Option<i32> { v.iter().copied().max() }',
        "b": 'fn max_val(v: &[i32]) -> Option<i32> { if v.is_empty() { return None; } let mut m = v[0]; for &x in &v[1..] { if x > m { m = x; } } Some(m) }',
    },
]

JUDGE_PROMPT = """\
You are a code reviewer. Score these two Rust implementations on rubric: {rubric}.

Implementation A:
```rust
{impl_a}
```

Implementation B:
```rust
{impl_b}
```

Respond with JSON only: {{"a": <0-10>, "b": <0-10>, "reasoning": "<one sentence>"}}"""

def score_pair(client, impl_a: str, impl_b: str) -> dict:
    resp = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=200,
        messages=[{"role": "user", "content": JUDGE_PROMPT.format(
            rubric=RUBRIC_DESC, impl_a=impl_a, impl_b=impl_b
        )}]
    )
    return json.loads(resp.content[0].text.strip())

def main():
    client = anthropic.Anthropic()
    all_deltas, all_flips = [], []

    for pair in IMPL_PAIRS:
        name = pair["name"]
        print(f"\n=== {name} ===")
        # Round 1: A, B
        r1 = score_pair(client, pair["a"], pair["b"])
        # Round 2: B, A (rotated — note which is "A" vs "B" in prompt)
        r2 = score_pair(client, pair["b"], pair["a"])
        # r2["a"] is actually impl_b, r2["b"] is actually impl_a
        # Normalize: impl_a score in each round
        score_a_r1 = r1["a"]
        score_a_r2 = r2["b"]  # impl_a was labeled "B" in round 2
        score_b_r1 = r1["b"]
        score_b_r2 = r2["a"]  # impl_b was labeled "A" in round 2

        delta_a = abs(score_a_r1 - score_a_r2)
        delta_b = abs(score_b_r1 - score_b_r2)
        winner_r1 = "a" if score_a_r1 > score_b_r1 else "b"
        winner_r2 = "a" if score_a_r2 > score_b_r2 else "b"
        flipped = winner_r1 != winner_r2

        print(f"  Round 1 (A,B): A={score_a_r1} B={score_b_r1} winner={winner_r1}")
        print(f"  Round 2 (B,A): A={score_a_r2} B={score_b_r2} winner={winner_r2}")
        print(f"  Delta A: {delta_a:.2f}, Delta B: {delta_b:.2f}, Flipped: {flipped}")
        all_deltas.extend([delta_a, delta_b])
        all_flips.append(flipped)

    mean_delta = statistics.mean(all_deltas)
    flip_rate = sum(all_flips) / len(all_flips)

    print("\n=== GATE 1 SUMMARY ===")
    print(f"Mean score delta across orderings: {mean_delta:.3f} (PASS if ≤ 0.15)")
    print(f"Winner flip rate: {flip_rate:.1%} (PASS if ≤ 20%)")

    passed = mean_delta <= 0.15 and flip_rate <= 0.20
    print(f"\nGATE 1: {'✅ PASS' if passed else '❌ FAIL — redesign judge before Task 7'}")

if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Run Gate 1**

```bash
python3 scripts/judge_reliability.py
```

Expected: `GATE 1: ✅ PASS`.

**If FAIL:** stop. Record results. Consider: (a) more aggressive cyclic ordering (3 rounds), (b) majority-vote with N=5, (c) different rubric decomposition.

- [ ] **Step 3: Record results**

Write to `docs/superpowers/validation/judge-reliability-results.md`.

- [ ] **Step 4: Commit**

```bash
git add scripts/judge_reliability.py docs/superpowers/validation/judge-reliability-results.md
git commit -m "feat(parallel): Gate 1 — CyclicJudge reliability validation"
```

---

## Task 7: ParallelConfig → Fleet + Track Diversity

**Files:**
- Modify: `mur-common/src/fleet.rs` (add `parallel` field)
- Create: `mur-core/src/parallel/track/mod.rs`
- Create: `mur-core/src/parallel/track/diversity.rs`
- Create: `mur-core/src/parallel/track/worktree.rs`

- [ ] **Step 1: Add `parallel` field to `Fleet` in `mur-common/src/fleet.rs`**

After the existing `loop_cfg` field, add:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub parallel: Option<mur_common::parallel::ParallelConfig>,
```

Add `use crate::parallel::ParallelConfig;` at top, then use `Option<ParallelConfig>`.

- [ ] **Step 2: Verify existing fleet tests still pass**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-common 2>&1 | tail -5
```

Expected: all existing tests pass (the new field is optional with default None).

- [ ] **Step 3: Write `mur-core/src/parallel/track/mod.rs`**

```rust
pub mod diversity;
pub mod worktree;

use std::path::PathBuf;
use mur_common::parallel::TrackConfig;

#[derive(Debug)]
pub struct Track {
    pub config: TrackConfig,
    pub worktree_path: PathBuf,
    pub base_snapshot: String,
}

pub struct TrackSet {
    pub session_id: String,
    pub tracks: Vec<Track>,
}
```

- [ ] **Step 4: Write `mur-core/src/parallel/track/diversity.rs`**

```rust
//! Inject diverse `approach` prompts into a fleet member's agent profile.
use std::path::Path;
use anyhow::Result;
use mur_common::parallel::TrackConfig;

const APPROACH_ENV_KEY: &str = "MUR_PARALLEL_APPROACH";

/// Write the approach text as an env override so the track's agent runtime picks it up.
/// The agent's system prompt should read `MUR_PARALLEL_APPROACH` when set.
pub fn inject_approach(agent_dir: &Path, track: &TrackConfig) -> Result<()> {
    let env_file = agent_dir.join("parallel_env");
    std::fs::write(&env_file, format!("{APPROACH_ENV_KEY}={}\n", track.approach))?;
    Ok(())
}

pub fn clear_approach(agent_dir: &Path) -> Result<()> {
    let env_file = agent_dir.join("parallel_env");
    if env_file.exists() { std::fs::remove_file(&env_file)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::TrackConfig;

    #[test]
    fn inject_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TrackConfig {
            name: "track-a".into(),
            approach: "functional style".into(),
            model: None,
        };
        inject_approach(dir.path(), &cfg).unwrap();
        let content = std::fs::read_to_string(dir.path().join("parallel_env")).unwrap();
        assert!(content.contains("functional style"));
        clear_approach(dir.path()).unwrap();
        assert!(!dir.path().join("parallel_env").exists());
    }
}
```

- [ ] **Step 5: Write `mur-core/src/parallel/track/worktree.rs`**

```rust
use std::path::Path;
use anyhow::Result;
use mur_common::parallel::ParallelConfig;
use crate::parallel::backend::{detect_backend, ParallelBackend};
use super::{Track, TrackSet};

pub fn create_track_set(
    session_id: &str,
    project_root: &Path,
    config: &ParallelConfig,
) -> Result<TrackSet> {
    let backend = detect_backend(project_root);
    let mut tracks = Vec::new();
    for track_cfg in &config.tracks {
        let name = format!("parallel-{session_id}-{}", track_cfg.name);
        let worktree_path = backend.create_track(&name)?;
        let base_snapshot = backend.base_snapshot(&worktree_path)?;
        tracks.push(Track {
            config: track_cfg.clone(),
            worktree_path,
            base_snapshot,
        });
    }
    Ok(TrackSet { session_id: session_id.to_string(), tracks })
}

pub fn destroy_track_set(project_root: &Path, set: &TrackSet) -> Result<()> {
    let backend = detect_backend(project_root);
    for track in &set.tracks {
        backend.destroy(&track.worktree_path)?;
    }
    Ok(())
}
```

- [ ] **Step 6: Add `pub mod track;` to `mur-core/src/parallel/mod.rs`** (already present from Task 1 skeleton — verify)

- [ ] **Step 7: Run tests**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::track 2>&1 | tail -5
```

Expected: `inject_and_clear` passes.

- [ ] **Step 8: Commit**

```bash
git add mur-common/src/fleet.rs mur-core/src/parallel/track/
git commit -m "feat(parallel): Fleet.parallel field + track diversity injection"
```

---

## Task 8: Pre-Filter (cargo check gate)

**Files:**
- Create: `mur-core/src/parallel/track/filter.rs`

**Interfaces:**
- Produces: `fn run_pre_filter(track_path: &Path, filters: &[PreFilterKind]) -> FilterResult`

- [ ] **Step 1: Write failing test in `filter.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::PreFilterKind;

    #[test]
    fn non_existent_path_fails_gracefully() {
        let result = run_pre_filter(
            std::path::Path::new("/tmp/definitely_does_not_exist_parallel_test"),
            &[PreFilterKind::CargoCheck],
        );
        assert!(matches!(result, FilterResult::Failed { .. }));
    }
}
```

- [ ] **Step 2: Write `mur-core/src/parallel/track/filter.rs`**

```rust
use std::path::Path;
use std::process::Command;
use mur_common::parallel::PreFilterKind;

#[derive(Debug)]
pub enum FilterResult {
    Passed,
    Failed { filter: PreFilterKind, stderr: String },
}

pub fn run_pre_filter(track_path: &Path, filters: &[PreFilterKind]) -> FilterResult {
    for filter in filters {
        let result = match filter {
            PreFilterKind::CargoCheck => run_cargo_check(track_path),
            PreFilterKind::CargoClippyDeny => run_clippy(track_path),
        };
        if let Err(stderr) = result {
            return FilterResult::Failed { filter: filter.clone(), stderr };
        }
    }
    FilterResult::Passed
}

fn run_cargo_check(path: &Path) -> Result<(), String> {
    let out = Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(path)
        .env("ORT_STRATEGY", "download")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

fn run_clippy(path: &Path) -> Result<(), String> {
    let out = Command::new("cargo")
        .args(["clippy", "--quiet", "--", "-D", "warnings"])
        .current_dir(path)
        .env("ORT_STRATEGY", "download")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}
```

- [ ] **Step 3: Add `Clone` to `PreFilterKind` in `mur-common/src/parallel.rs`**

`#[derive(Debug, Clone, ...)]` — verify `Clone` is already in the derive list. If not, add it.

- [ ] **Step 4: Add `pub mod filter;` to `mur-core/src/parallel/track/mod.rs`**

- [ ] **Step 5: Run tests**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::track::filter 2>&1 | tail -5
```

Expected: `non_existent_path_fails_gracefully` passes.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/parallel/track/filter.rs mur-core/src/parallel/track/mod.rs
git commit -m "feat(parallel): cargo check pre-filter — discard failed tracks before LLM judge"
```

---

## Task 9: CyclicJudge

**Files:**
- Create: `mur-core/src/parallel/judge/mod.rs`
- Create: `mur-core/src/parallel/judge/cyclic.rs`
- Create: `mur-core/src/parallel/judge/rubric.rs`

**Interfaces:**
- Produces: `CyclicJudge::score(task: &JudgeTask) -> Result<Vec<TrackScore>>`

- [ ] **Step 1: Write `mur-core/src/parallel/judge/mod.rs`**

```rust
pub mod cyclic;
pub mod rubric;

use crate::parallel::semantic::SemanticUnit;
use mur_common::parallel::TrackConfig;

pub struct JudgeTask {
    pub unit_name: String,
    pub implementations: Vec<TrackImpl>,
    pub rubric_version: String,
}

pub struct TrackImpl {
    pub track_config: TrackConfig,
    pub unit: SemanticUnit,
    pub source: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TrackScore {
    pub track_name: String,
    pub score: f32,
    pub reasoning: String,
    pub low_confidence: bool, // |round1 - round2| > 0.2
}

pub use cyclic::CyclicJudge;
```

- [ ] **Step 2: Write `mur-core/src/parallel/judge/rubric.rs`**

```rust
use mur_common::parallel::Rubric;

pub fn build_judge_prompt(
    unit_name: &str,
    implementations: &[(String, &str)],  // (track_name, source_code)
    rubric: &Rubric,
) -> String {
    let mut prompt = format!(
        "Score these Rust implementations of `{unit_name}` on:\n\
         - Correctness ({:.0}%)\n\
         - Design ({:.0}%)\n\
         - Maintainability ({:.0}%)\n\
         - Security ({:.0}%)\n\n",
        rubric.correctness * 100.0,
        rubric.design * 100.0,
        rubric.maintainability * 100.0,
        rubric.security * 100.0,
    );
    for (i, (label, code)) in implementations.iter().enumerate() {
        prompt.push_str(&format!("## Option {}\n```rust\n{code}\n```\n\n", (b'A' + i as u8) as char));
    }
    prompt.push_str(
        "Respond with JSON only:\n\
         {\"scores\": [{\"label\": \"A\", \"score\": <0-10>, \"reasoning\": \"<one sentence>\"}], \
         \"summary\": \"<one sentence>\"}"
    );
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::Rubric;

    #[test]
    fn prompt_contains_all_options() {
        let impls = vec![
            ("track-a".to_string(), "fn f() {}"),
            ("track-b".to_string(), "fn f() { todo!() }"),
        ];
        let prompt = build_judge_prompt("f", &impls, &Rubric::default());
        assert!(prompt.contains("Option A"));
        assert!(prompt.contains("Option B"));
        assert!(prompt.contains("Correctness"));
    }
}
```

- [ ] **Step 3: Write `mur-core/src/parallel/judge/cyclic.rs`**

```rust
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use mur_common::parallel::JudgeConfig;
use crate::pipeline::llm_client::completion;  // reuse existing LLM call path
use super::{JudgeTask, TrackScore, rubric::build_judge_prompt};

pub struct CyclicJudge {
    pub config: JudgeConfig,
}

#[derive(Deserialize)]
struct ScoreEntry { label: String, score: f32, reasoning: String }
#[derive(Deserialize)]
struct JudgeResponse { scores: Vec<ScoreEntry> }

impl CyclicJudge {
    pub async fn score(&self, task: &JudgeTask) -> Result<Vec<TrackScore>> {
        let n = task.implementations.len();
        if n == 0 { return Ok(vec![]); }

        // Round 1: original order
        let order1: Vec<usize> = (0..n).collect();
        let scores1 = self.one_round(task, &order1).await?;

        // Round 2: rotate right by 1 (last becomes first)
        let mut order2: Vec<usize> = (0..n).collect();
        order2.rotate_right(1);
        let scores2 = self.one_round(task, &order2).await?;

        // Average and compute low-confidence flag
        let mut results = Vec::with_capacity(n);
        for i in 0..n {
            let track_name = task.implementations[i].track_config.name.clone();
            let s1 = scores1[i];
            let s2 = scores2[i];
            let avg = (s1 + s2) / 2.0;
            let low_confidence = (s1 - s2).abs() > 2.0; // on 0-10 scale
            results.push(TrackScore {
                track_name,
                score: avg,
                reasoning: String::new(), // filled by caller from round 1
                low_confidence,
            });
        }
        Ok(results)
    }

    async fn one_round(&self, task: &JudgeTask, order: &[usize]) -> Result<Vec<f32>> {
        let ordered_impls: Vec<(String, &str)> = order
            .iter()
            .map(|&i| {
                let imp = &task.implementations[i];
                let code = std::str::from_utf8(&imp.source).unwrap_or("");
                (imp.track_config.name.clone(), code)
            })
            .collect();

        let prompt = build_judge_prompt(&task.unit_name, &ordered_impls, &self.config.rubric);
        let response = completion(&self.config.model, &prompt)
            .await
            .context("LLM judge call failed")?;

        let parsed: JudgeResponse = serde_json::from_str(response.trim())
            .context("failed to parse judge JSON")?;

        // Map label letters back to original track order
        let mut scores_by_position = vec![5.0f32; order.len()]; // default mid
        for entry in &parsed.scores {
            if entry.label.len() == 1 {
                let pos = (entry.label.as_bytes()[0] - b'A') as usize;
                if pos < order.len() {
                    scores_by_position[pos] = entry.score;
                }
            }
        }

        // Reorder back to original track indices
        let mut out = vec![5.0f32; order.len()];
        for (pos, &original_i) in order.iter().enumerate() {
            out[original_i] = scores_by_position[pos];
        }
        Ok(out)
    }
}
```

**Note:** `completion()` is the existing LLM call helper. Verify its exact import path with `grep -r "pub.*fn completion" mur-core/src/` and adjust the `use` statement accordingly.

- [ ] **Step 4: Run rubric tests**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::judge::rubric 2>&1 | tail -5
```

Expected: `prompt_contains_all_options` passes.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/judge/
git commit -m "feat(parallel): CyclicJudge — anti-position-bias scoring via two-round ordering swap"
```

---

## Task 10: Cherry-Pick Plan

**Files:**
- Create: `mur-core/src/parallel/cherry/mod.rs`
- Create: `mur-core/src/parallel/cherry/picker.rs`
- Create: `mur-core/src/parallel/cherry/conflict.rs`
- Create: `mur-core/src/parallel/cherry/assemble.rs`

**Interfaces:**
- Produces: `fn cherry_pick(scores: &[TrackScore], unit_names: &[String]) -> CherryPlan`
- Produces: `fn assemble_file(source: &[u8], selections: &CherryPlan, tracks: &[TrackSource]) -> Result<Vec<u8>>`

- [ ] **Step 1: Write `mur-core/src/parallel/cherry/mod.rs`**

```rust
pub mod assemble;
pub mod conflict;
pub mod picker;

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct UnitSelection {
    pub unit_name: String,
    pub winning_track: String,
    pub score: f32,
    pub low_confidence: bool,
}

#[derive(Debug, Clone)]
pub struct CherryPlan {
    /// unit_name → winning track name
    pub selections: HashMap<String, UnitSelection>,
}

impl CherryPlan {
    pub fn winning_track_for(&self, unit_name: &str) -> Option<&str> {
        self.selections.get(unit_name).map(|s| s.winning_track.as_str())
    }
}
```

- [ ] **Step 2: Write failing test for picker**

In `mur-core/src/parallel/cherry/picker.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::judge::TrackScore;

    fn ts(track: &str, score: f32) -> TrackScore {
        TrackScore { track_name: track.into(), score, reasoning: String::new(), low_confidence: false }
    }

    #[test]
    fn picks_highest_score_per_unit() {
        let scores_per_unit: Vec<(&str, Vec<TrackScore>)> = vec![
            ("authenticate", vec![ts("track-a", 9.0), ts("track-b", 7.5)]),
            ("logout", vec![ts("track-a", 6.0), ts("track-b", 8.5)]),
        ];
        let plan = cherry_pick(&scores_per_unit);
        assert_eq!(plan.winning_track_for("authenticate").unwrap(), "track-a");
        assert_eq!(plan.winning_track_for("logout").unwrap(), "track-b");
    }

    #[test]
    fn tie_goes_to_first_track() {
        let scores: Vec<(&str, Vec<TrackScore>)> = vec![
            ("f", vec![ts("track-a", 8.0), ts("track-b", 8.0)]),
        ];
        let plan = cherry_pick(&scores);
        assert_eq!(plan.winning_track_for("f").unwrap(), "track-a");
    }
}
```

- [ ] **Step 3: Write `mur-core/src/parallel/cherry/picker.rs`**

```rust
use std::collections::HashMap;
use crate::parallel::judge::TrackScore;
use super::{CherryPlan, UnitSelection};

pub fn cherry_pick(scores_per_unit: &[(&str, Vec<TrackScore>)]) -> CherryPlan {
    let mut selections = HashMap::new();
    for (unit_name, track_scores) in scores_per_unit {
        let Some(best) = track_scores.iter().max_by(|a, b| {
            a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)
        }) else { continue };
        selections.insert(unit_name.to_string(), UnitSelection {
            unit_name: unit_name.to_string(),
            winning_track: best.track_name.clone(),
            score: best.score,
            low_confidence: best.low_confidence,
        });
    }
    CherryPlan { selections }
}
```

- [ ] **Step 4: Write `mur-core/src/parallel/cherry/conflict.rs`**

```rust
//! API compatibility check between cherry-picked units.
//! Compares function signatures extracted by tree-sitter.
use crate::parallel::semantic::{SemanticUnit, SupportedLanguage, extract_units};
use anyhow::Result;

#[derive(Debug)]
pub struct ConflictReport {
    pub caller_unit: String,
    pub callee_unit: String,
    pub reason: String,
}

/// Check that all cross-track dependencies are API-compatible.
/// Returns a list of detected conflicts (empty = safe to assemble).
/// ponytail: signature-only check; full type inference would require rustc
pub fn check_conflicts(
    _cherry_plan: &super::CherryPlan,
    _source: &[u8],
    _lang: SupportedLanguage,
) -> Result<Vec<ConflictReport>> {
    // P1: return empty — cargo check after assembly is the real gate.
    // P2: implement signature diff via tree-sitter `parameters` node extraction.
    Ok(Vec::new())
}
```

- [ ] **Step 5: Write `mur-core/src/parallel/cherry/assemble.rs`**

```rust
use anyhow::Result;
use crate::parallel::semantic::{SemanticUnit, SupportedLanguage, extract_units};
use super::CherryPlan;

pub struct TrackSource<'a> {
    pub track_name: &'a str,
    pub source: &'a [u8],
}

/// Reconstruct a file by replacing each unit with the winning track's version.
/// Units with no selection keep the original source.
pub fn assemble_file(
    base_source: &[u8],
    plan: &CherryPlan,
    tracks: &[TrackSource<'_>],
    lang: SupportedLanguage,
) -> Result<Vec<u8>> {
    // Index track sources by name
    let track_map: std::collections::HashMap<&str, &[u8]> =
        tracks.iter().map(|t| (t.track_name, t.source)).collect();

    // Extract units from base (for ordering and byte ranges)
    let base_units = extract_units(base_source, lang)?;

    // Build the output file: replace units that have a winner, keep others
    let mut out = base_source.to_vec();
    // Process in reverse order so byte offsets stay valid
    let mut units_desc: Vec<_> = base_units.iter().collect();
    units_desc.sort_by(|a, b| b.byte_range.start.cmp(&a.byte_range.start));

    for base_unit in units_desc {
        let Some(selection) = plan.selections.get(&base_unit.name) else { continue };
        let Some(track_src) = track_map.get(selection.winning_track.as_str()) else { continue };
        let track_units = extract_units(track_src, lang)?;
        let Some(replacement) = track_units.iter().find(|u| u.name == base_unit.name) else { continue };
        let new_code = &track_src[replacement.byte_range.clone()];
        out.splice(base_unit.byte_range.clone(), new_code.iter().copied());
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::cherry::{CherryPlan, UnitSelection};
    use std::collections::HashMap;

    #[test]
    fn replaces_winning_unit() {
        let base = b"fn greet() { println!(\"hello\"); }\nfn farewell() { println!(\"bye\"); }\n";
        let track_b_src = b"fn greet() { println!(\"hi there!\"); }\nfn farewell() { println!(\"bye\"); }\n";
        let mut sels = HashMap::new();
        sels.insert("greet".into(), UnitSelection {
            unit_name: "greet".into(),
            winning_track: "track-b".into(),
            score: 9.0,
            low_confidence: false,
        });
        let plan = CherryPlan { selections: sels };
        let tracks = vec![TrackSource { track_name: "track-b", source: track_b_src }];
        let result = assemble_file(base, &plan, &tracks, SupportedLanguage::Rust).unwrap();
        let result_str = std::str::from_utf8(&result).unwrap();
        assert!(result_str.contains("hi there!"), "expected replacement: {result_str}");
        assert!(result_str.contains("farewell"), "expected farewell preserved: {result_str}");
    }
}
```

- [ ] **Step 6: Run tests**

```bash
ORT_STRATEGY=download cargo nextest run -p mur-core parallel::cherry 2>&1 | tail -10
```

Expected: `picks_highest_score_per_unit`, `tie_goes_to_first_track`, `replaces_winning_unit` all pass.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/parallel/cherry/
git commit -m "feat(parallel): cherry-pick — greedy best-score selection + file assembly"
```

---

## Task 11: CLI — `fleet create --parallel` + `fleet compare`

**Files:**
- Modify: `mur-core/src/cmd/fleet/create.rs` (add `--parallel`, `--tracks N`)
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add `compare`, `judge_cmd`, `cherry_cmd` modules)
- Create: `mur-core/src/cmd/fleet/compare.rs`

- [ ] **Step 1: Add `--parallel` flag to `cmd_fleet_create`**

In `mur-core/src/cmd/fleet/create.rs`, extend the function signature:

```rust
pub fn cmd_fleet_create(
    mur_home: &Path,
    name: &str,
    members: Vec<String>,
    router: Option<String>,
    goal: Option<String>,
    parallel: Option<mur_common::parallel::ParallelConfig>,  // NEW
) -> Result<()> {
    // ... existing code ...
    let fleet = Fleet {
        // ... existing fields ...
        parallel,   // NEW
        ..
    };
    // ... existing code ...
}
```

Update the internal test call to pass `None` for parallel.

- [ ] **Step 2: Verify the Hub and CLI callers compile**

```bash
ORT_STRATEGY=download cargo check -p mur-core 2>&1 | grep "error" | head -20
```

Fix any call sites that now have wrong arg count.

- [ ] **Step 3: Write `mur-core/src/cmd/fleet/compare.rs`**

```rust
//! `mur fleet compare <name>` — show per-unit scores across all tracks.
use std::path::Path;
use anyhow::{Context, Result};
use super::store::load_fleet;
use crate::parallel::state::ParallelStateDb;
use crate::parallel::semantic::{extract_units, SupportedLanguage};
use crate::parallel::backend::detect_backend;

pub fn cmd_fleet_compare(mur_home: &Path, fleet_name: &str, unit_filter: Option<&str>) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet.parallel.as_ref().context("fleet has no parallel config")?;
    let state_dir = mur_home.join("fleets").join(fleet_name).join("parallel_state");
    let db = ParallelStateDb::open(&state_dir)?;
    let rubric_ver = parallel.judge.rubric.version();

    // Load score for each track × unit from LMDB
    // For now print a summary; rich table follows in P1 polish
    println!("Fleet: {fleet_name}  Tracks: {}", parallel.tracks.len());
    println!("{:<25} {}", "Unit", parallel.tracks.iter().map(|t| format!("{:<12}", t.name)).collect::<Vec<_>>().join(" "));
    println!("{}", "-".repeat(70));

    // ponytail: unit list comes from scanning track worktrees; in P1 alpha we scan the first track
    // This is a stub — full implementation requires TrackSet in state DB
    println!("(Run `mur fleet judge {fleet_name}` first to populate scores)");
    Ok(())
}
```

- [ ] **Step 4: Add modules to `mur-core/src/cmd/fleet/mod.rs`**

```rust
pub mod cherry_cmd;
pub mod compare;
pub mod judge_cmd;
```

- [ ] **Step 5: Create stub files for the remaining commands** (full impl in Task 12)

Create `mur-core/src/cmd/fleet/judge_cmd.rs`:
```rust
//! `mur fleet judge <name>` — run LLM judge across all tracks.
use std::path::Path;
use anyhow::Result;
pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str) -> Result<()> {
    let _ = (mur_home, fleet_name);
    todo!("implemented in Task 12")
}
```

Create `mur-core/src/cmd/fleet/cherry_cmd.rs`:
```rust
//! `mur fleet cherry <name>` — execute cherry-pick assembly.
use std::path::Path;
use anyhow::Result;
pub fn cmd_fleet_cherry(mur_home: &Path, fleet_name: &str, auto: bool) -> Result<()> {
    let _ = (mur_home, fleet_name, auto);
    todo!("implemented in Task 12")
}
```

- [ ] **Step 6: Wire the new subcommands into the main CLI arg parser**

Find where `fleet` subcommands are matched in `mur-core/src/main.rs` (or wherever the CLI dispatch lives) and add:

```rust
("fleet", Some(fleet_args)) => match fleet_args.subcommand() {
    // ... existing arms ...
    ("compare", Some(args)) => {
        cmd_fleet_compare(mur_home, args.value_of("NAME").unwrap(), args.value_of("unit"))
    }
    ("judge", Some(args)) => {
        cmd_fleet_judge(mur_home, args.value_of("NAME").unwrap())
    }
    ("cherry", Some(args)) => {
        cmd_fleet_cherry(mur_home, args.value_of("NAME").unwrap(), args.is_present("auto"))
    }
    // ...
}
```

**Note:** First run `grep -n "fleet\|Subcommand\|subcommand" mur-core/src/main.rs | head -30` to find the exact dispatch pattern, then match it.

- [ ] **Step 7: Build and smoke-test**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build -p mur-core 2>&1 | tail -5
./target/debug/mur fleet compare --help 2>&1 | head -5
```

Expected: builds without error, `--help` shows usage.

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cmd/fleet/
git commit -m "feat(parallel): fleet compare/judge/cherry CLI scaffolding"
```

---

## Task 12: CLI — `fleet judge` + `fleet cherry` full implementation

**Files:**
- Modify: `mur-core/src/cmd/fleet/judge_cmd.rs` (replace stub)
- Modify: `mur-core/src/cmd/fleet/cherry_cmd.rs` (replace stub)

- [ ] **Step 1: Implement `cmd_fleet_judge`**

Replace the stub in `judge_cmd.rs`:

```rust
use std::path::Path;
use anyhow::{Context, Result};
use super::store::load_fleet;
use crate::parallel::{
    backend::detect_backend,
    judge::{CyclicJudge, JudgeTask, TrackImpl},
    semantic::{extract_units, SupportedLanguage},
    semantic::cas::group_by_identity,
    state::{JudgeScore, ParallelStateDb},
    track::filter::{run_pre_filter, FilterResult},
};

pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet.parallel.as_ref().context("fleet has no parallel config")?;

    let state_dir = mur_home.join("fleets").join(fleet_name).join("parallel_state");
    let db = ParallelStateDb::open(&state_dir)?;
    let rubric_ver = parallel.judge.rubric.version();
    let judge = CyclicJudge { config: parallel.judge.clone() };

    let backend = detect_backend(mur_home); // uses project cwd in practice; P2 will thread it through

    for track_cfg in &parallel.tracks {
        let worktree = mur_home
            .join("fleets").join(fleet_name)
            .join("tracks").join(&track_cfg.name);
        if !worktree.exists() {
            println!("⚠  track {} worktree not found — run `mur fleet run {fleet_name}` first", track_cfg.name);
            continue;
        }

        // Pre-filter
        let filter_result = run_pre_filter(&worktree, &parallel.pre_filter);
        if let FilterResult::Failed { filter, stderr } = filter_result {
            println!("✗  track {} failed {:?} — discarded", track_cfg.name, filter);
            eprintln!("{stderr}");
            continue;
        }
        println!("✓  track {} passed pre-filter", track_cfg.name);
    }

    // Collect changed files from all passing tracks, run CAS, then judge
    // ponytail: full implementation threads track sources through group_by_identity + CyclicJudge
    // This is the minimal working shell; scoring loop goes here in P1 polish iteration
    println!("Judge complete. Run `mur fleet compare {fleet_name}` to view scores.");
    Ok(())
}
```

- [ ] **Step 2: Implement `cmd_fleet_cherry`**

```rust
use std::path::Path;
use anyhow::{Context, Result};
use super::store::load_fleet;
use crate::parallel::{
    cherry::{assemble::assemble_file, picker::cherry_pick, CherryPlan},
    semantic::SupportedLanguage,
    state::ParallelStateDb,
};

pub fn cmd_fleet_cherry(mur_home: &Path, fleet_name: &str, auto: bool) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet.parallel.as_ref().context("fleet has no parallel config")?;
    let state_dir = mur_home.join("fleets").join(fleet_name).join("parallel_state");
    let _db = ParallelStateDb::open(&state_dir)?;

    println!("Cherry-picking best functions from {} tracks...", parallel.tracks.len());
    // ponytail: full cherry loop (load scores → cherry_pick → assemble_file → cargo check → write)
    // P1 alpha: print the plan; write output to fleets/<name>/cherry-result/
    println!("Use `mur fleet promote {fleet_name} cherry` to apply the result.");
    Ok(())
}
```

- [ ] **Step 3: Full end-to-end build check**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build -p mur-core 2>&1 | tail -5
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/fleet/judge_cmd.rs mur-core/src/cmd/fleet/cherry_cmd.rs
git commit -m "feat(parallel): fleet judge + cherry commands — P1 alpha wiring"
```

---

## Task 13: Gate 2+3 — Quality & Cost Validation Scripts

**⛔ BLOCKING for P1 final release (not for alpha).**

**Files:**
- Create: `scripts/cherry_quality.py`
- Create: `docs/superpowers/validation/cherry-pick-quality-results.md`
- Create: `docs/superpowers/validation/cas-efficiency-results.md`

- [ ] **Step 1: Write `scripts/cherry_quality.py`**

```python
#!/usr/bin/env python3
"""Gate 2+3: cherry-pick quality and CAS efficiency measurement.
Run this against a real mur-core parallel session after Task 12 is working.

Prerequisites:
  1. Create a test fleet: mur fleet create qual-test --members agent1,agent2,agent3 --parallel
  2. Run it: mur fleet run qual-test
  3. Run judge: mur fleet judge qual-test
  4. Run this script pointing at the state dir
"""
import sys, json, subprocess, statistics
from pathlib import Path

def run_cargo_check(path: Path) -> bool:
    result = subprocess.run(
        ["cargo", "check", "--quiet"],
        cwd=path, capture_output=True,
        env={**__import__("os").environ, "ORT_STRATEGY": "download"}
    )
    return result.returncode == 0

def main(fleet_dir: Path):
    state_dir = fleet_dir / "parallel_state"
    if not state_dir.exists():
        print(f"ERROR: {state_dir} not found. Run the fleet first.")
        sys.exit(1)

    cherry_dir = fleet_dir / "cherry-result"
    if not cherry_dir.exists():
        print("ERROR: no cherry result. Run `mur fleet cherry <name>` first.")
        sys.exit(1)

    # Gate 2: cargo check pass rate
    cargo_ok = run_cargo_check(cherry_dir)
    print(f"Gate 2a — cargo check on cherry result: {'✅ PASS' if cargo_ok else '❌ FAIL'}")

    # Gate 3: count CAS hits from state DB (requires LMDB introspection)
    # ponytail: read summary JSON written by mur fleet judge --stats flag (P1 polish)
    stats_file = fleet_dir / "judge_stats.json"
    if stats_file.exists():
        stats = json.loads(stats_file.read_text())
        hit_rate = stats.get("cas_hit_rate", 0)
        cost_ratio = stats.get("cost_ratio_vs_single", 99)
        print(f"Gate 3a — CAS hit rate: {hit_rate:.1%} (PASS if ≥ 30%): {'✅' if hit_rate >= 0.30 else '❌'}")
        print(f"Gate 3b — Cost ratio vs single agent: {cost_ratio:.1f}× (PASS if ≤ 2.5×): {'✅' if cost_ratio <= 2.5 else '❌'}")
    else:
        print("Gate 3: stats file not found — run with --stats flag (P1 polish iteration)")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <path-to-fleet-dir>")
        sys.exit(1)
    main(Path(sys.argv[1]))
```

- [ ] **Step 2: Commit the script**

```bash
git add scripts/cherry_quality.py
git commit -m "feat(parallel): Gate 2+3 quality + CAS efficiency scripts"
```

- [ ] **Step 3: Run against a real fleet (after Task 12 is working end-to-end)**

```bash
python3 scripts/cherry_quality.py ~/.mur/fleets/qual-test
```

Fill in `docs/superpowers/validation/cherry-pick-quality-results.md` with actual output.

**Gate 2 fail criteria:** `cargo check` fails → strengthen dependency conflict detection in `conflict.rs`.
**Gate 3 fail criteria:** CAS hit rate < 30% or cost > 4× → add embedding-similarity pre-scoring tier.

---

## Done

After Gate 2+3 pass:

1. Tag the P1 release: `git tag -a v-parallel-p1 -m "feat: parallel tracks P1 alpha"`
2. Start separate plan for **P1.5 Platform COW** (APFS `cp -c`, Btrfs reflink)
3. Open GitHub issue for **full `fleet compare` rich table** (score display is stub in Task 11)
4. Open GitHub issue for **`fleet promote cherry`** (promote wiring is stubbed in cherry_cmd)
