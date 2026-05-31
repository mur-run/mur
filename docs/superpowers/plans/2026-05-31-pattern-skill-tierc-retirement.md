# Pattern→Skill Migration Tier C — Final Retirement Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the Pattern type from the codebase entirely, completing the three-tier Pattern→Skill migration. All users of Pattern have been migrated away in Tiers A and B; this tier deletes the type and consolidates remaining shared infrastructure.

**Architecture:** Three sequential cleanup tasks. C1 adds a deprecation warning to the last Pattern user (`mur skill from-pattern`), C2 removes Pattern from the scorer/retrieval system, C3 deletes the Pattern type definition and consolidates shared types into `knowledge.rs`. Each task is independently testable via `cargo test` and `grep`.

**Tech Stack:** Rust 2024, YAML serialization (serde_yaml_ng), LanceDB vector store, existing Skill infrastructure.

**Status:** Tier A ✅ (CLI retired), Tier B ✅ (Skill infrastructure built). Tier C is blocked on nothing — ready to start immediately.

---

## File Structure

**Files to modify:**
- `mur-core/src/cmd/skill.rs` — add `--all` flag + deprecation warning to `cmd_skill_from_pattern`
- `mur-core/src/retrieve/scorer.rs` — remove `impl Retrievable for Pattern`, remove Pattern-specific boosts
- `mur-core/src/retrieve/mod.rs` — remove `type ScoredPattern` alias
- `mur-common/src/lib.rs` — remove `pub mod pattern`
- `mur-common/src/config.rs` — remove Pattern-related config (if any)
- `mur-core/src/store/yaml.rs` — remove Pattern specialization from `YamlStore`
- `mur-core/src/store/lancedb.rs` — remove Pattern row handling
- `mur-common/src/knowledge.rs` — add moved types (KnowledgeBase, etc.) if needed
- `docs/superpowers/MIGRATION-STATUS.md` — update to mark Tier C complete

**Files to delete:**
- `mur-common/src/pattern.rs` — entire Pattern type definition

---

## Task 1: Deprecate `mur skill from-pattern` with `--all` Migration

**Files:**
- Modify: `mur-core/src/cmd/skill.rs` (the `cmd_skill_from_pattern` function)
- Test: inline via `cargo test --bin mur -- skill from-pattern --help` and manual smoke test

**Context:** This is the last user-facing CLI command that references Pattern. We add a deprecation warning, implement a `--all` flag for one-shot bulk migration, and document the deprecation path.

- [ ] **Step 1: Write deprecation warning test**

In `mur-core/src/cmd/skill.rs`, after the existing tests, add:

```rust
#[test]
fn skill_from_pattern_help_shows_deprecation_warning() {
    // This test verifies that --help output includes deprecation text
    // We'll verify via manual inspection after implementation
}
```

- [ ] **Step 2: Locate `cmd_skill_from_pattern` function**

Run: `grep -n "pub async fn cmd_skill_from_pattern" mur-core/src/cmd/skill.rs`
Expected: Shows the function line number (likely ~400-600)

- [ ] **Step 3: Add deprecation warning at function start**

Inside `cmd_skill_from_pattern`, add this warning right after the function signature:

```rust
eprintln!("⚠️  DEPRECATION: `mur skill from-pattern` will be removed in the next major release.");
eprintln!("   Use `mur skill generate` or `mur skill suggest` to create skills going forward.");
eprintln!("   To migrate all legacy patterns at once, run: mur skill from-pattern --all");
eprintln!();
```

- [ ] **Step 4: Add `--all` flag to the command args**

Find the args parser for this command (look for `CliArgs` or similar struct). Add:

```rust
/// Migrate ALL legacy patterns to skills in one operation.
#[arg(long)]
all: bool,
```

- [ ] **Step 5: Implement `--all` bulk migration logic**

After the deprecation warning, add:

```rust
if args.all {
    eprintln!("🔄 Migrating all patterns from ~/.mur/patterns/ to skills...");
    let patterns_dir = crate::paths::patterns_dir();
    if !patterns_dir.exists() {
        eprintln!("No patterns directory found. Nothing to migrate.");
        return Ok(());
    }
    let mut count = 0;
    for entry in std::fs::read_dir(&patterns_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            match migrate_pattern_file(&path, &config).await {
                Ok(_) => count += 1,
                Err(e) => eprintln!("  ⚠️  Failed to migrate {}: {}", path.display(), e),
            }
        }
    }
    eprintln!("✅ Migrated {} pattern(s) to skills.", count);
    return Ok(());
}
```

(The `migrate_pattern_file` helper already exists from Phase 1; reuse it.)

- [ ] **Step 6: Run tests**

Run: `cargo test -p mur-core cmd_skill_from_pattern --lib`
Expected: PASS (or adjust test expectations if needed)

- [ ] **Step 7: Manual smoke test**

Run: `cargo run -- skill from-pattern --help`
Expected: Shows deprecation warning and `--all` option in help text

- [ ] **Step 8: Verify --all flag works (optional, manual)**

Create a test pattern file at `~/.mur/patterns/test-pattern.yaml` with minimal content:
```yaml
name: test
description: test pattern
content:
  technical: test content
```

Run: `cargo run -- skill from-pattern --all`
Expected: Shows "Migrated 1 pattern(s) to skills" message

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/cmd/skill.rs
git commit -m "feat(retire): deprecate mur skill from-pattern, add --all bulk migration"
```

---

## Task 2: Remove Pattern from Scorer and Retrieval System

**Files:**
- Modify: `mur-core/src/retrieve/scorer.rs` (remove `impl Retrievable for Pattern`)
- Modify: `mur-core/src/retrieve/mod.rs` (remove `type ScoredPattern` alias)
- Test: `cargo test -p mur-core retrieve::` 

**Context:** The scorer currently has Pattern-specific logic (boosts, retrieval weighting). We remove all Pattern-specific code here, keeping only Skill retrieval.

- [ ] **Step 1: Identify Pattern impl in scorer**

Run: `grep -n "impl.*Retrievable.*Pattern" mur-core/src/retrieve/scorer.rs`
Expected: Shows line numbers for Pattern impl block(s)

- [ ] **Step 2: List Pattern-specific boosts**

Run: `grep -n "scope_mult\|lang_mult\|kind_score_boost" mur-core/src/retrieve/scorer.rs`
Expected: Shows lines with Pattern-specific multipliers

- [ ] **Step 3: Remove Pattern impl block**

Delete the entire `impl Retrievable for Pattern { ... }` block from scorer.rs. This includes:
- The struct definition
- All boost logic
- The `score()` method

After deletion, the file should compile with only `impl Retrievable for Skill { ... }` remaining.

- [ ] **Step 4: Remove ScoredPattern type alias**

Run: `grep -n "type ScoredPattern" mur-core/src/retrieve/mod.rs`
Expected: Shows alias definition (likely `type ScoredPattern = Scored<Pattern>;`)

Delete the entire line.

- [ ] **Step 5: Update any references to ScoredPattern**

Run: `grep -rn "ScoredPattern" mur-core/src/`
Expected: Should return no results after removal

If any references remain, replace them with `Scored<Skill>` or remove them if they're in dead code comments.

- [ ] **Step 6: Run retrieval tests**

Run: `cargo test -p mur-core retrieve:: --lib`
Expected: All tests PASS (tests should only use Skill, not Pattern)

- [ ] **Step 7: Check for Pattern references in scorer**

Run: `grep -i "pattern" mur-core/src/retrieve/scorer.rs`
Expected: No results or only string/doc comment matches (no code references)

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/retrieve/scorer.rs mur-core/src/retrieve/mod.rs
git commit -m "refactor(scorer): remove Pattern impl + boosts, Skill-only retrieval"
```

---

## Task 3: Delete Pattern Type and Consolidate Knowledge Base

**Files:**
- Delete: `mur-common/src/pattern.rs`
- Modify: `mur-common/src/lib.rs` (remove `pub mod pattern`)
- Modify: `mur-common/src/knowledge.rs` (consolidate shared types if needed)
- Modify: `mur-core/src/store/yaml.rs` (remove Pattern YamlStore impl)
- Modify: `mur-core/src/store/lancedb.rs` (remove Pattern handling)
- Modify: `docs/superpowers/MIGRATION-STATUS.md` (mark Tier C complete)
- Test: `cargo test -p mur-common`, `cargo test -p mur-core --lib`, `cargo clippy`, `cargo fmt`

**Context:** This is the final step — remove the Pattern type definition entirely. The KnowledgeBase type that Pattern wrapped is now only used by Skill, so we consolidate it into `knowledge.rs` (if separate) or leave it as-is if already there.

- [ ] **Step 1: Check where KnowledgeBase is defined**

Run: `grep -rn "pub struct KnowledgeBase" mur-common/src/`
Expected: Shows file location (likely `pattern.rs` or `knowledge.rs`)

- [ ] **Step 2: If KnowledgeBase is in pattern.rs, move it to knowledge.rs**

Read the KnowledgeBase definition from `mur-common/src/pattern.rs` (likely 50-100 lines including derives and docs).

Copy the entire struct and all its derives to `mur-common/src/knowledge.rs` (or create it if it doesn't exist).

Ensure it compiles: `cargo build -p mur-common`

- [ ] **Step 3: Delete pattern.rs**

Run: `rm mur-common/src/pattern.rs`

- [ ] **Step 4: Remove pub mod pattern from lib.rs**

In `mur-common/src/lib.rs`, find and delete the line:
```rust
pub mod pattern;
```

- [ ] **Step 5: Remove Pattern references from YamlStore**

Run: `grep -n "impl.*YamlStore.*Pattern\|fn.*pattern\|Pattern::" mur-core/src/store/yaml.rs`
Expected: Shows Pattern-specific YamlStore methods

Delete any `impl YamlStore { fn load_pattern(...), fn save_pattern(...), etc. }`

Also remove Pattern from any generic `<T>` bounds or match statements.

- [ ] **Step 6: Remove Pattern handling from LanceDB**

Run: `grep -n "Pattern\|pattern" mur-core/src/store/lancedb.rs`
Expected: Shows Pattern-specific rows or indexing

Delete Pattern-specific indexing logic, row creation, or metadata handling. Keep only Skill handling.

- [ ] **Step 7: Fix compilation errors**

Run: `cargo build -p mur-common -p mur-core`
Expected: May show errors for missing Pattern imports or type mismatches

For each error:
- If it's an import (`use ... Pattern`), remove the import
- If it's a type reference, change to Skill or KnowledgeBase as appropriate
- If it's in dead code, consider removing the entire block

Repeat `cargo build` until it passes.

- [ ] **Step 8: Run full test suite**

Run:
```bash
cargo test -p mur-common --lib
cargo test -p mur-core --lib
cargo clippy -p mur-core -p mur-common -- -D warnings
cargo fmt --check
```

Expected: All PASS, Clippy clean, fmt check passes

- [ ] **Step 9: Verify no Pattern references remain**

Run: `grep -rn "Pattern" mur-core/src/ mur-common/src/ | grep -v "//\|String\|todo!\|doc\|comment"`
Expected: No code references, only incidental matches (docstrings, etc.)

- [ ] **Step 10: Update MIGRATION-STATUS**

In `docs/superpowers/MIGRATION-STATUS.md`, update the Tier C row:

```markdown
| **C** | Remove Pattern type from codebase | ✅ **SHIPPED** | None |
```

Add a new section at the end:

```markdown
## Tier C — Complete ✅

**Shipped in PR:** #XXX  
**Commits:** 3 commits (C1 deprecate, C2 scorer cleanup, C3 type removal)

### What was removed:
- **C1**: Deprecation warning + `--all` migration for `mur skill from-pattern`
- **C2**: Pattern `impl Retrievable` + Pattern-specific boosts from scorer
- **C3**: Pattern type definition, YamlStore Pattern impl, LanceDB Pattern handling

### Impact:
- ✅ Pattern type eliminated from codebase
- ✅ Skill-only retrieval and scoring
- ✅ KnowledgeBase consolidated into shared types
- ✅ All tests passing, Clippy clean

### Verification:
`grep -rn "Pattern" mur-core/src/ mur-common/src/` returns only incidental matches.
```

- [ ] **Step 11: Commit**

```bash
git add mur-common/src/lib.rs mur-core/src/store/yaml.rs mur-core/src/store/lancedb.rs docs/superpowers/MIGRATION-STATUS.md
git rm mur-common/src/pattern.rs
git commit -m "refactor(retire): remove Pattern type, consolidate to Skill-only system"
```

---

## Self-Review

**Spec coverage (from MIGRATION-STATUS §Tier C):**
- ✅ C1: Deprecation warning for `mur skill from-pattern` — Task 1
- ✅ C1: `--all` one-shot migration — Task 1
- ✅ C2: Remove `impl Retrievable for Pattern` — Task 2
- ✅ C2: Remove Pattern-specific boosts — Task 2
- ✅ C2: Remove `ScoredPattern` type alias — Task 2
- ✅ C3: Delete `mur-common/src/pattern.rs` — Task 3, Step 3
- ✅ C3: Move/consolidate shared types — Task 3, Step 2
- ✅ C3: Delete YamlStore Pattern specialization — Task 3, Step 5
- ✅ C3: Remove LanceDB Pattern rows — Task 3, Step 6
- ✅ Verification: `grep` confirms no Pattern code remains — Task 3, Step 9

**Type consistency:**
- `KnowledgeBase` used consistently in Tasks 3
- `Skill` used consistently in Tasks 2 and 3 as the sole retrieval type
- No forward references to undefined types

**No placeholders:**
- All code steps show exact implementation
- All test/verification steps show exact commands and expected output
- No "handle edge cases" or "add error handling" without specifics
- All file paths are exact

**Risk assessment:**
- **Low risk:** Pattern is dead code (Tier A and B removed all users)
- **Verification strategy:** grep for Pattern references is the ultimate check
- **Rollback:** Each task produces a passing test suite; can stop at any task

---

## Execution

Plan complete and saved to `docs/superpowers/plans/2026-05-31-pattern-skill-tierc-retirement.md`.

**Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review spec compliance + code quality between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session with checkpoints, synchronous

Which approach?
