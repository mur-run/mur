# `mur notes list` + `mur notes show` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the note read/browse surface — `mur notes list [--maturity <state>]` enumerates notes with their lifecycle maturity, and `mur notes show <name>` prints a note's body + maturity and records the view as a retrieval — making the Notes MVP fully usable (create, browse, read, search, evolve).

**Architecture:** Two pure handlers `do_list` and `do_show` take an explicit `mur_home: &Path` for tempdir testing; thin `cmd_list` / `cmd_show` wrappers add `resolve_mur_home()` + stdout (and `cmd_show` records a retrieval via Plan 5's `record_retrieval`). `do_list` reuses `load_skill_candidates` (Plan 2), filters `category == Note`, optionally filters by `lifecycle_state`, sorts by name. `do_show` reads a single skill via `read_from_dir(global_skill_dir(...))`, verifies it is a note, and pulls maturity from `SkillStats`. CLI scaffolding extends the existing `NotesAction` enum (Plan E) with `List` and `Show`.

**Tech Stack:** Rust 2024, clap derive, existing `mur_common::skill::store::{global_skill_dir, read_from_dir}`, `mur_common::skill::stats::{LifecycleState, SkillStats}`, `mur_core::retrieve::skill_candidates::load_skill_candidates`, and `record_retrieval` (Plan 5). No new dependencies.

**Depends on:** Plans 1, 2, A, E (notes create+search), and 5 (`record_retrieval`) merged. This extends `mur-core/src/cli/notes.rs`, `mur-core/src/cmd/notes_cmd.rs`, and `dispatch.rs` created/edited by those plans.

**Out of scope (later plans):** `mur notes edit` ($EDITOR round-trip via `parse_markdown`/`serialize_canonical`), `archive`, `export --obsidian`, MCP surface, hybrid vector search.

---

## Design notes (verified)

1. **`LifecycleState`** is `#[serde(rename_all = "snake_case")]`, derives `Copy, PartialEq, Eq, Default`, has **no `Display`** — print with `{:?}` (e.g. `Draft`, `Emerging`). The `--maturity` flag is parsed case-insensitively by a small `parse_maturity` helper.
2. **Single-note read:** `read_from_dir(&global_skill_dir(mur_home, name)) -> Result<SkillManifest, StoreError>` (both re-exported from `mur_common::skill::store`). Maturity comes from `SkillStats::load(&SkillStats::path(mur_home, name))`; a note with no stats sidecar yet defaults to `LifecycleState::Draft`.
3. **`load_skill_candidates`** (Plan 2) already returns `LoadedSkill { manifest, stats }`, so `do_list` gets maturity per note for free (fresh stats default to `Draft`).
4. **Viewing is using:** `cmd_show` records a retrieval (Plan 5's `record_retrieval`) so reading a note accrues lifecycle usage, same as search. Best-effort (warn, never fail the read).

---

## File map

- **Modify:** `mur-core/src/cli/notes.rs` — add `List` and `Show` to `NotesAction`.
- **Modify:** `mur-core/src/cmd/notes_cmd.rs` — `parse_maturity`, `NoteListRow`, `NoteView`, `do_list`, `do_show`, `cmd_list`, `cmd_show`, plus added imports.
- **Modify:** `mur-core/src/dispatch.rs` — `List` and `Show` match arms.

---

## Task 1: `do_list` + maturity filter

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs`

- [ ] **Step 1: Add the needed imports**

At the top of `mur-core/src/cmd/notes_cmd.rs`, update/add these imports (the `store` line gains `read_from_dir`; `anyhow` gains `anyhow`; add the `stats` line):

```rust
use anyhow::{Context, Result, anyhow, bail};
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::store::{global_skill_dir, read_from_dir, write_to_dir};
```

(Leave the other existing imports from Plans E and 5 in place.)

- [ ] **Step 2: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn do_list_returns_notes_sorted_by_name_with_maturity() {
    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "zebra", "z note", "body").unwrap();
    do_create(tmp.path(), "alpha", "a note", "body").unwrap();

    let rows = do_list(tmp.path(), None, 10).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "alpha");
    assert_eq!(rows[1].name, "zebra");
    assert_eq!(rows[0].maturity, LifecycleState::Draft);
}

#[test]
fn do_list_filters_by_maturity() {
    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "n1", "d", "body").unwrap();
    // Fresh notes are Draft.
    assert!(do_list(tmp.path(), Some(LifecycleState::Stable), 10).unwrap().is_empty());
    assert_eq!(do_list(tmp.path(), Some(LifecycleState::Draft), 10).unwrap().len(), 1);
}

#[test]
fn do_list_excludes_non_note_skills() {
    use std::fs;
    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "real-note", "d", "body").unwrap();
    let ctx = tmp.path().join("skills").join("ctx");
    fs::create_dir_all(&ctx).unwrap();
    fs::write(
        ctx.join("skill.yaml"),
        "name: ctx\nversion: 1.0.0\npublisher: human:test\n\
         category: context\ndescription: d\ncontent:\n  abstract: a\n  context: c\n",
    ).unwrap();

    let rows = do_list(tmp.path(), None, 10).unwrap();
    assert_eq!(rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(), vec!["real-note"]);
}

#[test]
fn parse_maturity_is_case_insensitive_and_rejects_unknown() {
    assert_eq!(parse_maturity("Stable").unwrap(), LifecycleState::Stable);
    assert_eq!(parse_maturity("emerging").unwrap(), LifecycleState::Emerging);
    assert!(parse_maturity("bogus").is_err());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::do_list_returns_notes_sorted_by_name_with_maturity`
Expected: COMPILE ERROR — `cannot find function 'do_list'`.

- [ ] **Step 4: Implement `parse_maturity`, `NoteListRow`, `do_list`**

Add to `mur-core/src/cmd/notes_cmd.rs` (above the `#[cfg(test)]` block):

```rust
/// One row of `mur notes list`.
#[derive(Debug, Clone)]
pub struct NoteListRow {
    pub name: String,
    pub maturity: LifecycleState,
    pub description: String,
}

/// Parse a `--maturity` value (case-insensitive) into a `LifecycleState`.
pub fn parse_maturity(s: &str) -> Result<LifecycleState> {
    match s.to_lowercase().as_str() {
        "draft" => Ok(LifecycleState::Draft),
        "emerging" => Ok(LifecycleState::Emerging),
        "stable" => Ok(LifecycleState::Stable),
        "canonical" => Ok(LifecycleState::Canonical),
        "deprecated" => Ok(LifecycleState::Deprecated),
        "archived" => Ok(LifecycleState::Archived),
        other => bail!(
            "unknown maturity '{other}' \
             (expected draft|emerging|stable|canonical|deprecated|archived)"
        ),
    }
}

/// List `category: note` skills, optionally filtered by maturity, sorted by name.
pub fn do_list(
    mur_home: &Path,
    maturity: Option<LifecycleState>,
    limit: usize,
) -> Result<Vec<NoteListRow>> {
    let skills_dir = mur_home.join("skills");
    let all = load_skill_candidates(&skills_dir, mur_home)?;
    let mut rows: Vec<NoteListRow> = all
        .into_iter()
        .filter(|s| s.manifest.category == Category::Note)
        .filter(|s| maturity.is_none_or(|m| s.stats.lifecycle_state == m))
        .map(|s| NoteListRow {
            name: s.manifest.name.clone(),
            maturity: s.stats.lifecycle_state,
            description: s.manifest.description.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows.truncate(limit);
    Ok(rows)
}
```

(`Option::is_none_or` is stable in Rust 1.82+; the workspace is on edition 2024 / a recent toolchain. If the toolchain rejects it, use `maturity.map_or(true, |m| s.stats.lifecycle_state == m)`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::do_list`
Expected: the three `do_list_*` tests and `parse_maturity_*` PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "feat(notes): do_list enumerates notes with maturity, filter + sort"
```

---

## Task 2: `do_show` — read a single note

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn do_show_returns_a_note_view() {
    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "my-note", "My description", "# Heading\nprose").unwrap();

    let v = do_show(tmp.path(), "my-note").unwrap();
    assert_eq!(v.name, "my-note");
    assert_eq!(v.description, "My description");
    assert_eq!(v.body, "# Heading\nprose");
    assert_eq!(v.maturity, LifecycleState::Draft);
}

#[test]
fn do_show_errors_for_missing_note() {
    let tmp = tempdir().unwrap();
    let err = do_show(tmp.path(), "nope").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn do_show_errors_for_non_note_skill() {
    use std::fs;
    let tmp = tempdir().unwrap();
    let ctx = tmp.path().join("skills").join("ctx");
    fs::create_dir_all(&ctx).unwrap();
    fs::write(
        ctx.join("skill.yaml"),
        "name: ctx\nversion: 1.0.0\npublisher: human:test\n\
         category: context\ndescription: d\ncontent:\n  abstract: a\n  context: c\n",
    ).unwrap();

    let err = do_show(tmp.path(), "ctx").unwrap_err();
    assert!(err.to_string().contains("not a note"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::do_show_returns_a_note_view`
Expected: COMPILE ERROR — `cannot find function 'do_show'`.

- [ ] **Step 3: Implement `NoteView` and `do_show`**

Add to `mur-core/src/cmd/notes_cmd.rs` (above the `#[cfg(test)]` block):

```rust
/// A note rendered for `mur notes show`.
#[derive(Debug, Clone)]
pub struct NoteView {
    pub name: String,
    pub description: String,
    pub maturity: LifecycleState,
    pub body: String,
}

/// Load a single note for display. Errors if the skill is missing or not a note.
pub fn do_show(mur_home: &Path, name: &str) -> Result<NoteView> {
    let dir = global_skill_dir(mur_home, name);
    let manifest = read_from_dir(&dir).map_err(|_| anyhow!("note '{name}' not found"))?;
    if manifest.category != Category::Note {
        bail!("'{name}' is not a note (category: {:?})", manifest.category);
    }
    let maturity = SkillStats::load(&SkillStats::path(mur_home, name))?
        .map(|s| s.lifecycle_state)
        .unwrap_or_default();
    let body = manifest.content.note.clone().unwrap_or_default();
    Ok(NoteView {
        name: manifest.name,
        description: manifest.description,
        maturity,
        body,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::do_show`
Expected: the three `do_show_*` tests PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "feat(notes): do_show reads a single note (errors on missing/non-note)"
```

---

## Task 3: CLI wiring — `mur notes list` / `show`

**Files:**
- Modify: `mur-core/src/cli/notes.rs`
- Modify: `mur-core/src/cmd/notes_cmd.rs` — `cmd_list`, `cmd_show`
- Modify: `mur-core/src/dispatch.rs`

- [ ] **Step 1: Extend the `NotesAction` enum**

In `mur-core/src/cli/notes.rs`, add two variants to `NotesAction` (after the existing `Create` and `Search`):

```rust
    /// List notes, optionally filtered by maturity.
    List {
        /// Filter by lifecycle maturity: draft|emerging|stable|canonical|deprecated|archived.
        #[arg(long)]
        maturity: Option<String>,

        /// Maximum notes to print.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Print a single note's body and maturity (records a retrieval).
    Show {
        /// The note name.
        name: String,
    },
```

- [ ] **Step 2: Add the `cmd_list` / `cmd_show` wrappers**

Append to `mur-core/src/cmd/notes_cmd.rs`:

```rust
/// Top-level `mur notes list` handler.
pub fn cmd_list(maturity: Option<&str>, limit: usize) -> Result<()> {
    let home = resolve_mur_home()?;
    let filter = maturity.map(parse_maturity).transpose()?;
    let rows = do_list(&home, filter, limit)?;
    if rows.is_empty() {
        println!("No notes found.");
        return Ok(());
    }
    for r in &rows {
        println!("{:<40} {:<11} {}", r.name, format!("{:?}", r.maturity), r.description);
    }
    Ok(())
}

/// Top-level `mur notes show` handler.
pub fn cmd_show(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let view = do_show(&home, name)?;
    println!("# {}", view.name);
    println!("{}", view.description);
    println!("maturity: {:?}\n", view.maturity);
    println!("{}", view.body);

    // Viewing a note is a retrieval — best-effort, never fail the read.
    if let Err(e) = record_retrieval(&home, &view.name, Utc::now()) {
        tracing::warn!(note = %view.name, error = %e, "record retrieval failed");
    }
    Ok(())
}
```

- [ ] **Step 3: Wire dispatch**

In `mur-core/src/dispatch.rs`, extend the `Commands::Notes { action } => match action { ... }` block (added in Plan E) with two arms:

```rust
            crate::cli::notes::NotesAction::List { maturity, limit } => {
                cmd::notes_cmd::cmd_list(maturity.as_deref(), limit)?
            }
            crate::cli::notes::NotesAction::Show { name } => {
                cmd::notes_cmd::cmd_show(&name)?
            }
```

- [ ] **Step 4: Build and run the notes test surface**

Run: `cargo build -p mur-core && cargo test -p mur-core cmd::notes_cmd::`
Expected: clean build, all existing notes tests PASS.

- [ ] **Step 5: Smoke-test the CLI**

```bash
TMPHOME=$(mktemp -d)
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes create alpha -d "First note" --body-file <(echo "# alpha body")
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes create beta  -d "Second note" --body-file <(echo "# beta body")
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes list
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes list --maturity draft
MUR_HOME=$TMPHOME cargo run -q -p mur-core -- notes show alpha
```

Expected: `list` prints `alpha`/`beta` with `Draft`; `list --maturity draft` prints both; `list --maturity stable` would print "No notes found."; `show alpha` prints the heading/description/maturity and the body.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/notes.rs mur-core/src/cmd/notes_cmd.rs mur-core/src/dispatch.rs
git commit -m "feat(notes): wire mur notes {list,show} CLI; show records a retrieval"
```

---

## Task 4: End-to-end integration test

**Files:**
- Modify: `mur-core/src/cmd/notes_cmd.rs`

- [ ] **Step 1: Write the test**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn create_then_list_and_show_compose() {
    let tmp = tempdir().unwrap();
    do_create(tmp.path(), "fly", "Deploy to fly", "# fly\nsteps").unwrap();
    do_create(tmp.path(), "brew", "Brew tips", "# brew\nupdate").unwrap();

    // list is sorted by name
    let rows = do_list(tmp.path(), None, 10).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["brew", "fly"]
    );
    assert!(rows.iter().all(|r| r.maturity == LifecycleState::Draft));

    // show returns the right body
    let v = do_show(tmp.path(), "fly").unwrap();
    assert_eq!(v.body, "# fly\nsteps");
    assert_eq!(v.description, "Deploy to fly");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core cmd::notes_cmd::tests::create_then_list_and_show_compose`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/notes_cmd.rs
git commit -m "test(notes): create -> list -> show compose end-to-end"
```

---

## Task 5: Verification gate — full workspace and lints

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all pass. Net new tests: 4 (Task 1) + 3 (Task 2) + 1 (Task 4) = 8.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. If clippy flags `is_none_or` as unstable on the pinned toolchain, switch to `map_or(true, ...)` as noted in Task 1.

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
Expected files for this plan's commits: `mur-core/src/cli/notes.rs`, `mur-core/src/cmd/notes_cmd.rs`, `mur-core/src/dispatch.rs`. Anything else is scope creep — review.

- [ ] **Step 5: Final commit if cleanup was needed**

If Steps 2-3 required fixes, the amend handles it. Otherwise nothing extra.

---

## Done state

After this plan:

- `mur notes list [--maturity <state>] [--limit N]` prints `name maturity description`, sorted by name, filterable by lifecycle state.
- `mur notes show <name>` prints a note's heading, description, maturity, and markdown body, and records the view as a retrieval (feeding the Plan 5 lifecycle loop).
- `parse_maturity` accepts the six lifecycle states case-insensitively.
- 8 new tests cover sort, maturity filter, non-note exclusion, parse, view rendering, missing/non-note errors, and the create→list→show composition.
- **The Notes MVP is now fully usable:** create, browse (list), read (show), find (search), and evolve (lifecycle) — all on the unified Skill foundation.

**What this unlocks (later plans):**
- `mur notes edit <name>` — $EDITOR round-trip via the existing `parse_markdown`/`serialize_canonical`.
- `mur notes archive <name>` — set `lifecycle_state: Archived` on the stats sidecar.
- `mur notes export --obsidian <vault>` — render each note as a SKILL.md file.
- MCP `skill_search(category: note)` / `note_create` for AI-assistant access.

**What this does NOT do:**
- No editing/archiving/exporting — separate plans.
- No MCP surface — separate plan.
- `list` does not show usage counts or last-activity — maturity only (add columns later if wanted).
