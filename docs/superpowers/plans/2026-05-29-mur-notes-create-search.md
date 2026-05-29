# `mur notes create` + `mur notes search` MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the minimum-viable note CLI — `mur notes create` writes a `category: note` skill to `~/.mur/skills/<name>/skill.yaml`, and `mur notes search <query>` ranks notes through the existing generic scorer — together delivering a complete write→find cycle and exercising the entire foundation (Plans 1+2+A) end-to-end.

**Architecture:** Two pure handlers `do_create` and `do_search` take an explicit `mur_home: &Path` so they are fully testable against a tempdir. Thin `cmd_create` / `cmd_search` wrappers call `resolve_mur_home()` and handle stdout/stdin (so unit tests bypass the I/O layer entirely). CLI scaffolding adds `Commands::Notes { action: NotesAction }` mirroring the existing `Commands::Skill { action: SkillAction }` pattern. `do_create` builds a `category: note` `SkillManifest` with `content.note` populated, validates, and writes via the existing atomic `mur_common::skill::store::write_to_dir`. `do_search` calls `load_skill_candidates` (Plan 2), filters `category == Note`, scores via `score_and_rank_generic` (Plan 1), and returns the ranked `Vec<Scored<LoadedSkill>>`.

**Tech Stack:** Rust 2024, clap derive, existing helpers — `mur_common::skill::store::{global_skill_dir, write_to_dir}`, `mur_common::skill::validate`, `mur_common::skill::parser::parse_canonical`, `mur_common::skill::types::{Category, Priority}`, `mur_core::retrieve::skill_candidates::{LoadedSkill, load_skill_candidates}`, `mur_core::retrieve::scoring::{score_and_rank_generic, Scored}`. No new dependencies (`tempfile` already in `mur-core` dev-deps).

**Depends on:** Plans 1, 2, and A merged. This plan uses `LoadedSkill`, `load_skill_candidates`, `score_and_rank_generic`, `Category::Note`, and `Content.note` — all introduced by those plans.

**Out of scope (later plans):** `mur notes list`, `show`, `edit` (with `$EDITOR`), `archive`, `export --obsidian`, `ingest`, MCP `note_create`, hybrid (vector) search. Each is a small standalone follow-on once this MVP ships.

---

## Design notes (verified)

1. **Sync handlers** match existing convention (`cmd::skill_cmd::cmd_*` are all sync `Result<()>`).
2. **`pub(crate) fn resolve_mur_home() -> Result<PathBuf>` lives at `mur-core/src/cmd/agent/mod.rs:89`.** Importable as `use super::agent::resolve_mur_home;` from any `cmd/*.rs` sibling.
3. **Atomic write helper exists** — `mur_common::skill::store::write_to_dir(dir, &SkillManifest) -> Result<PathBuf, StoreError>` writes `.skill.yaml.tmp` then renames. No new atomic-write code needed.
4. **Body source = `--body-file` or stdin.** `$EDITOR` integration is deferred to a later `mur notes edit` plan.
5. **Publisher default** — Mandatory Rule #1 forbids inline magic strings. Defined as a single `const DEFAULT_PUBLISHER: &str = "human:local";` at the top of `cmd/notes_cmd.rs`, with a comment that future plans may make it config-driven.

---

## File map

- **Create:** `mur-core/src/cli/notes.rs` — `NotesAction` enum (`Create`, `Search`).
- **Modify:** `mur-core/src/cli/mod.rs` — `pub mod notes;`, add `Notes { action: NotesAction }` to `Commands`.
- **Create:** `mur-core/src/cmd/notes_cmd.rs` — `do_create`, `do_search`, `cmd_create`, `cmd_search`, plus tests.
- **Modify:** `mur-core/src/cmd/mod.rs` — `pub mod notes_cmd;` (in alphabetical position after `mod.rs:24 pub mod doctor;`).
- **Modify:** `mur-core/src/dispatch.rs` — add `Commands::Notes { action } => match action { ... }` arm.

---

## Task 1: `do_create` — pure function that writes a note skill

**Files:**
- Create: `mur-core/src/cmd/notes_cmd.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/notes_cmd.rs` with this initial content:

```rust
//! `mur notes` CLI handlers — MVP create + search.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::skill::manifest::{Content, SkillManifest};
use mur_common::skill::store::{global_skill_dir, write_to_dir};
use mur_common::skill::types::{Category, Priority};
use mur_common::skill::validate;

/// Author identity stamped onto notes created via the local CLI.
/// Plan-marker: later plans may swap this for a config-driven value.
const DEFAULT_PUBLISHER: &str = "human:local";

/// Build a `category: note` skill at `<mur_home>/skills/<name>/skill.yaml`.
/// Returns the path written.
///
/// Errors:
/// - if the target skill directory already contains a `skill.yaml` (duplicate name)
/// - if the resulting manifest fails `mur_common::skill::validate::validate`
pub fn do_create(
    mur_home: &Path,
    name: &str,
    description: &str,
    body: &str,
) -> Result<PathBuf> {
    let dir = global_skill_dir(mur_home, name);
    if dir.join("skill.yaml").exists() {
        bail!("note '{name}' already exists at {}", dir.display());
    }

    let manifest = SkillManifest {
        name: name.to_string(),
        version: "1.0.0".into(),
        publisher: DEFAULT_PUBLISHER.into(),
        description: description.to_string(),
        category: Category::Note,
        hosts: vec![],
        content: Content {
            r#abstract: description.to_string(),
            context: None,
            procedure: None,
            command: None,
            note: Some(body.to_string()),
        },
        requires: vec![],
        tags: vec![],
        triggers: vec![],
        priority: Priority::Normal,
        evolution_log: vec![],
        transfer_chain: vec![],
        mcp_requirements: vec![],
    };

    validate(&manifest).with_context(|| format!("validate note '{name}'"))?;
    let written = write_to_dir(&dir, &manifest)
        .with_context(|| format!("write skill.yaml for '{name}'"))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::parser::parse_canonical;
    use tempfile::tempdir;

    #[test]
    fn do_create_writes_a_well_formed_note_skill() {
        let tmp = tempdir().unwrap();
        let path = do_create(
            tmp.path(),
            "rust-error-handling",
            "Rust error handling reference",
            "# Rust Error Handling\n\nUse anyhow for app errors.",
        )
        .unwrap();

        assert!(path.ends_with("skills/rust-error-handling/skill.yaml"));
        let yaml = std::fs::read_to_string(&path).unwrap();
        let m = parse_canonical(&yaml).unwrap();

        assert_eq!(m.name, "rust-error-handling");
        assert_eq!(m.category, Category::Note);
        assert_eq!(m.content.r#abstract, "Rust error handling reference");
        assert_eq!(
            m.content.note.as_deref(),
            Some("# Rust Error Handling\n\nUse anyhow for app errors.")
        );
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn do_create_rejects_duplicate_name() {
        let tmp = tempdir().unwrap();
        do_create(tmp.path(), "dup", "d", "body").unwrap();
        let err = do_create(tmp.path(), "dup", "d", "body").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn do_create_rejects_invalid_name() {
        let tmp = tempdir().unwrap();
        // Uppercase letters violate validate_name (ascii_lowercase only).
        let err = do_create(tmp.path(), "BadName", "d", "body").unwrap_err();
        assert!(err.to_string().contains("validate") || err.to_string().contains("name"));
    }
}
```

- [ ] **Step 2: Register the module so the test runs**

In `mur-core/src/cmd/mod.rs`, add `pub mod notes_cmd;` in alphabetical position (between `pub mod doctor;` at line 24 and the next entry). The compiler's "unused import" warnings inside `notes_cmd.rs` will go away once the tests reference each import.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p mur-core cmd::notes_cmd::tests`
Expected: 3 tests PASS.

If `do_create_rejects_invalid_name` fails because validate's error message no longer contains "name", widen the assertion to also check for `InvalidName` debug formatting — but the existing `ValidationError::InvalidName(...)` `Display` impl returns "invalid name ..." so the current assertion holds.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs mur-core/src/cmd/mod.rs
git commit -m "feat(notes): do_create writes category:note skill atomically

Validates manifest before write_to_dir. Rejects duplicates and invalid names.
Body lives in content.note (1a storage)."
```

---

## Task 2: `do_search` — pure function that ranks notes

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs` — add the function and its tests.

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/cmd/notes_cmd.rs` (above the existing `#[cfg(test)] mod tests`):

```rust
use crate::retrieve::scoring::{Scored, score_and_rank_generic};
use crate::retrieve::skill_candidates::{LoadedSkill, load_skill_candidates};

/// Search `~/.mur/skills/` for `category: note` skills matching `query`.
/// Returns up to `limit` ranked results (Scored<LoadedSkill>).
pub fn do_search(
    mur_home: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<Scored<LoadedSkill>>> {
    let skills_dir = mur_home.join("skills");
    let all = load_skill_candidates(&skills_dir, mur_home)?;
    let notes: Vec<LoadedSkill> = all
        .into_iter()
        .filter(|s| s.manifest.category == Category::Note)
        .collect();
    let mut ranked = score_and_rank_generic(query, notes);
    ranked.truncate(limit);
    Ok(ranked)
}
```

Then append these tests inside the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn do_search_filters_out_non_note_skills() {
    use std::fs;
    let tmp = tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");

    // A genuine note (created via do_create).
    do_create(tmp.path(), "deploy-fly", "Deploy to Fly.io", "# fly deploy steps").unwrap();

    // A non-note (category: context) hand-written to the same skills dir.
    let ctx_dir = skills_dir.join("context-thing");
    fs::create_dir_all(&ctx_dir).unwrap();
    fs::write(
        ctx_dir.join("skill.yaml"),
        "name: context-thing\nversion: 1.0.0\npublisher: human:test\n\
         category: context\ndescription: deploy context\n\
         content:\n  abstract: deploy fly\n  context: details\n",
    ).unwrap();

    let ranked = do_search(tmp.path(), "deploy fly", 10).unwrap();
    let names: Vec<_> = ranked.iter().map(|s| s.item.manifest.name.clone()).collect();
    assert!(names.contains(&"deploy-fly".to_string()));
    assert!(!names.contains(&"context-thing".to_string()));
}

#[test]
fn do_search_respects_limit_and_orders_by_score() {
    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "rust-anyhow",
              "Anyhow for rust apps",
              "# anyhow\nuse anyhow for application errors").unwrap();
    do_create(tmp.path(), "rust-thiserror",
              "thiserror for libraries",
              "# thiserror\nuse thiserror for library errors").unwrap();
    do_create(tmp.path(), "unrelated-brew",
              "homebrew update",
              "# brew\nrun brew update weekly").unwrap();

    let ranked = do_search(tmp.path(), "rust anyhow application errors", 2).unwrap();
    assert!(ranked.len() <= 2);
    assert_eq!(ranked[0].item.manifest.name, "rust-anyhow",
               "rust-anyhow should rank above rust-thiserror for this query");
    if ranked.len() == 2 {
        assert!(ranked[0].score >= ranked[1].score);
    }
}

#[test]
fn do_search_returns_empty_when_no_notes_exist() {
    let tmp = tempdir().unwrap();
    let ranked = do_search(tmp.path(), "anything", 10).unwrap();
    assert!(ranked.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail / then pass**

Run: `cargo test -p mur-core cmd::notes_cmd::tests`
Expected: tests compile (the impl is already in Step 1's append) and pass. The function and tests went in together, so this task is unusually structured — but TDD here is "write the contract and the tests in the same step" because the impl is a thin orchestration of three existing functions (load + filter + score). Splitting into a separate failing-then-passing cycle adds no design value.

If `do_search_respects_limit_and_orders_by_score` fails because both `rust-anyhow` and `rust-thiserror` are filtered by the score floor, lower the expected match by widening the query or assert non-strict ordering. The intended outcome: `rust-anyhow` (name + abstract match all four query words `rust anyhow application errors`) outscores `rust-thiserror` (matches only `rust`).

- [ ] **Step 3: Run the full retrieve + cmd test surface to catch any regressions**

Run: `cargo test -p mur-core retrieve:: cmd::notes_cmd::`
Expected: every test PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "feat(notes): do_search ranks category:note skills via the generic scorer"
```

---

## Task 3: CLI scaffolding — `Commands::Notes { action }`

**Files:**
- Create: `mur-core/src/cli/notes.rs`
- Modify: `mur-core/src/cli/mod.rs`
- Modify: `mur-core/src/cmd/notes_cmd.rs` — add `cmd_create` / `cmd_search` wrappers.
- Modify: `mur-core/src/dispatch.rs`

- [ ] **Step 1: Create the `NotesAction` enum**

Create `mur-core/src/cli/notes.rs`:

```rust
//! `mur notes` CLI subcommands — MVP: create + search.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum NotesAction {
    /// Create a new note. Body comes from --body-file or stdin.
    Create {
        /// Unique kebab-case name (lowercase letters, digits, hyphens; ≤ 64 chars).
        name: String,

        /// One-line description (also stored as content.abstract).
        #[arg(long, short = 'd')]
        description: String,

        /// Read the markdown body from this file. If omitted, body is read from stdin.
        #[arg(long)]
        body_file: Option<PathBuf>,
    },

    /// Rank existing notes by a keyword query.
    Search {
        /// The search query.
        query: String,

        /// Maximum results to print.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}
```

- [ ] **Step 2: Wire the enum into `Commands`**

In `mur-core/src/cli/mod.rs`:

a. Near the top, add the module import (alphabetical with the existing `pub mod skill;` etc.):

```rust
pub mod notes;
```

b. In the `pub enum Commands` definition, add the `Notes` variant immediately after the existing `Skill { ... }` variant:

```rust
    /// Manage notes — category:note skills with markdown bodies.
    Notes {
        #[command(subcommand)]
        action: notes::NotesAction,
    },
```

- [ ] **Step 3: Add the `cmd_create` and `cmd_search` wrappers**

In `mur-core/src/cmd/notes_cmd.rs`, append the top-level handlers:

```rust
use std::io::Read;

use super::agent::resolve_mur_home;

/// Top-level `mur notes create` handler.
pub fn cmd_create(name: &str, description: &str, body_file: Option<&Path>) -> Result<()> {
    let body = match body_file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("read body file {}", p.display()))?,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read body from stdin")?;
            s
        }
    };
    let home = resolve_mur_home()?;
    let path = do_create(&home, name, description, &body)?;
    println!("Created note '{}' at {}", name, path.display());
    Ok(())
}

/// Top-level `mur notes search` handler.
pub fn cmd_search(query: &str, limit: usize) -> Result<()> {
    let home = resolve_mur_home()?;
    let ranked = do_search(&home, query, limit)?;
    if ranked.is_empty() {
        println!("No notes match '{query}'.");
        return Ok(());
    }
    for (i, sp) in ranked.iter().enumerate() {
        println!(
            "{:>2}. {:<40} score={:.3}  {}",
            i + 1,
            sp.item.manifest.name,
            sp.score,
            sp.item.manifest.description
        );
    }
    Ok(())
}
```

- [ ] **Step 4: Wire dispatch**

In `mur-core/src/dispatch.rs`, add this match arm to the `run` function (place it adjacent to `Commands::Skill { action } => match action { ... }`):

```rust
        Commands::Notes { action } => match action {
            crate::cli::notes::NotesAction::Create {
                name,
                description,
                body_file,
            } => cmd::notes_cmd::cmd_create(&name, &description, body_file.as_deref())?,
            crate::cli::notes::NotesAction::Search { query, limit } => {
                cmd::notes_cmd::cmd_search(&query, limit)?
            }
        },
```

- [ ] **Step 5: Build and run the workspace tests**

Run: `cargo build --workspace`
Expected: clean.

Run: `cargo test -p mur-core cmd::notes_cmd::`
Expected: all 6 tests still PASS (3 from Task 1 + 3 from Task 2). The new `cmd_create`/`cmd_search` wrappers are thin and are exercised in Task 4.

- [ ] **Step 6: Smoke-test the CLI binary**

```bash
TMPHOME=$(mktemp -d)
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes create demo-note \
  -d "demo description" --body-file <(echo "# demo body")
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes search demo
```

Expected: first command prints `Created note 'demo-note' at .../skills/demo-note/skill.yaml`. Second prints a ranked list with `demo-note` at the top.

If `resolve_mur_home` does not honor `MUR_HOME`, look at `mur-core/src/cmd/agent/mod.rs:89` to see what env var or default path it uses, and adjust the smoke test accordingly. The exact env-var contract is internal to `resolve_mur_home`; the test is informational only — automated coverage lives in Task 4.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cli/notes.rs mur-core/src/cli/mod.rs \
        mur-core/src/cmd/notes_cmd.rs mur-core/src/dispatch.rs
git commit -m "feat(notes): wire mur notes {create,search} CLI

NotesAction enum + Commands::Notes variant + dispatch arm. Body source is
--body-file or stdin. Search prints ranked results with scores."
```

---

## Task 4: End-to-end integration test

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs` — add an integration test that exercises the full create→search cycle.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn end_to_end_create_two_notes_then_search_returns_them_ranked() {
    let tmp = tempdir().unwrap();

    do_create(
        tmp.path(),
        "fly-deploy",
        "Deploy a Rust app to Fly.io",
        "# Deploy Steps\n1. cargo build --release\n2. fly deploy",
    ).unwrap();

    do_create(
        tmp.path(),
        "brew-tips",
        "Homebrew maintenance",
        "# Brew\nRun brew update weekly to keep formulae fresh.",
    ).unwrap();

    let ranked = do_search(tmp.path(), "deploy rust fly", 10).unwrap();
    assert!(!ranked.is_empty(), "search should find at least the deploy note");
    assert_eq!(ranked[0].item.manifest.name, "fly-deploy");
    // brew-tips may or may not pass the score floor; if it did, it must rank below.
    if ranked.len() > 1 {
        assert!(ranked[0].score > ranked[1].score);
    }

    // Re-running create with the same name fails — proves duplicate detection survives a real flow.
    let err = do_create(tmp.path(), "fly-deploy", "x", "y").unwrap_err();
    assert!(err.to_string().contains("already exists"));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::end_to_end_create_two_notes_then_search_returns_them_ranked`
Expected: PASS — this is the proof that create + search compose into a working note system on a real (temp) filesystem.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "test(notes): end-to-end create -> search cycle on a tempdir"
```

---

## Task 5: Verification gate — full workspace and lints

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: every test passes. Net new tests added by this plan: 3 (Task 1) + 3 (Task 2) + 1 (Task 4) = 7.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. The new CLI scaffolding may trigger `clippy::needless_pass_by_value` on `&Path` / `&str` arguments — only fix if the warning is genuine; the chosen signatures match the rest of the codebase.

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
Expected files (in this plan's commits only):
- `mur-core/src/cli/notes.rs` (new)
- `mur-core/src/cli/mod.rs` (one module import + one Commands variant)
- `mur-core/src/cmd/notes_cmd.rs` (new)
- `mur-core/src/cmd/mod.rs` (one `pub mod notes_cmd;` line)
- `mur-core/src/dispatch.rs` (one match arm)

Anything else means scope creep — review and revert.

- [ ] **Step 5: Final commit if cleanup was needed**

If Steps 2-3 required fixes, the amend above handles it. Otherwise nothing extra.

---

## Done state

After this plan:

- `mur notes create <name> --description <D> [--body-file <PATH>]` writes a validated `category: note` skill at `~/.mur/skills/<name>/skill.yaml`, with the markdown body in `content.note` (1a storage). Body comes from `--body-file` or stdin.
- `mur notes search <query> [--limit N]` ranks notes through `score_and_rank_generic` and prints `rank. name score description`.
- 7 new tests prove: well-formed create, duplicate rejection, invalid name rejection, category filter, score-floor + ordering, empty corpus, and full create→search round-trip.
- **The foundation (Plans 1+2+A) is exercised end-to-end** — keyword-only hybrid scoring over `LoadedSkill` is now actually used by a real CLI feature, not just a building block.

**What this unlocks (small follow-on plans, each ~3-5 tasks):**
- `mur notes list [--tag <T>]` — scan + filter without scoring.
- `mur notes show <name>` — print frontmatter + rendered body (or raw markdown).
- `mur notes edit <name>` — `parse_canonical` → SKILL.md view → `$EDITOR` → `parse_markdown` → `serialize_canonical` round-trip (the existing `parse_markdown` does most of the work).
- `mur notes archive <name>` — sets `lifecycle_state: Archived` on the stats sidecar.
- `mur notes export --obsidian <vault>` — renders each note as a SKILL.md file in the vault.

**What this does NOT do:**
- No `$EDITOR` integration (deferred — needs a temp-file round-trip through `parse_markdown`/`serialize_canonical`).
- No MCP surface — separate plan.
- No vector embedding of note bodies — needs a corpus LanceDB index, separate plan.
- No Pattern removal — independent cleanup, separate plan.
