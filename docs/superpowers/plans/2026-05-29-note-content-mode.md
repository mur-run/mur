# Note Content Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce the `note` content mode end-to-end at the type level — `Category::Note`, `ContentMode::Note`, a `Content.note` markdown body field, validation, and surfacing the note body in `LoadedSkill::text()` — so note-mode skills are well-formed, validated, and searchable through the existing generic scorer.

**Architecture:** Add a `Note` variant to the `Category` and `ContentMode` enums (lowercase serde, matching siblings). Add `note: Option<String>` to `Content` (the 1a storage decision: a note's markdown body lives inside the canonical `skill.yaml` under `content.note`, not a sibling file). Extend `Content::mode()` from a 3-tuple to a 4-tuple, update `validate`'s mode disambiguation and `mode_matches_category` (`Category::Note` ↔ `ContentMode::Note`), and extend `LoadedSkill::text()` (from the skill-retrievable plan) to include the note body so notes rank on their content.

**Tech Stack:** Rust 2024 edition, cargo workspace, `mur-common::skill` types (`Category`, `ContentMode`, `Content`, `validate`, `parse_canonical`), `mur-core::retrieve::skill_candidates::LoadedSkill`. No new dependencies.

**Depends on:** `2026-05-29-retrievable-trait-extraction.md` (Plan 1) and `2026-05-29-skill-retrievable-impl.md` (Plan 2) must be merged — Task 4 edits `LoadedSkill::text()` and Task 2 fixes a `Content` literal in `skill_candidates.rs`.

**Out of scope (later plans):** `mur notes` CLI facade, `note_create`/`note_link` MCP tools, Obsidian export, local-LLM ingest, Pattern removal. This plan only makes the *type* exist, validate, and rank.

---

## Design notes (verified against the codebase)

1. **Adding the enum variants is safe.** No exhaustive `match` on `Category` or `ContentMode` exists that lacks a wildcard:
   - `Content::mode()` is the only tuple match — this plan edits it.
   - `validate::mode_matches_category` uses `matches!(...)` (returns false for unlisted pairs — safe).
   - `skill_doctor.rs:710` uses `!= Some(ContentMode::Workflow)` (a comparison, not a match — safe).
   - Other `Category::*` sites are construction or comparison, not exhaustive matches.
2. **Adding `Content.note` breaks every `Content { ... }` struct literal** (skill `Content` does not derive `Default`). The complete set to fix with `note: None,` is:
   - `mur-common/src/skill/manifest.rs` — two test literals (the `mode()` tests).
   - `mur-common/src/skill/gene.rs:250`
   - `mur-core/src/cmd/skill_from_pattern.rs:90`
   - `mur-core/src/skill_index/text.rs:60`
   - `mur-core/src/retrieve/skill_candidates.rs` — the `fake_loaded` test helper from Plan 2.

   `cargo build` after the field addition will list any literal missed; add `note: None,` to each.

---

## File map

- **Modify:** `mur-common/src/skill/types.rs` — `Note` on `Category` and `ContentMode`.
- **Modify:** `mur-common/src/skill/manifest.rs` — `Content.note` field, 4-tuple `mode()`, two test-literal fixes.
- **Modify:** `mur-common/src/skill/validate.rs` — 4-element disambiguation, `mode_matches_category` Note arm, two error-message strings.
- **Modify:** `mur-common/src/skill/gene.rs`, `mur-core/src/cmd/skill_from_pattern.rs`, `mur-core/src/skill_index/text.rs` — one `note: None,` each.
- **Modify:** `mur-core/src/retrieve/skill_candidates.rs` — `note: None,` in `fake_loaded`; extend `LoadedSkill::text()` to include the note body.

---

## Task 1: Add `Note` to `Category` and `ContentMode`

**Files:**
- Modify: `mur-common/src/skill/types.rs` (enums at lines ~36 and ~46; test module at ~71)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `mur-common/src/skill/types.rs`:

```rust
#[test]
fn note_category_serialises_lowercase_and_roundtrips() {
    let yaml = serde_yaml_ng::to_string(&Category::Note).unwrap();
    assert_eq!(yaml.trim(), "note");
    let parsed: Category = serde_yaml_ng::from_str("note").unwrap();
    assert_eq!(parsed, Category::Note);
}

#[test]
fn note_content_mode_serialises_lowercase_and_roundtrips() {
    let yaml = serde_yaml_ng::to_string(&ContentMode::Note).unwrap();
    assert_eq!(yaml.trim(), "note");
    let parsed: ContentMode = serde_yaml_ng::from_str("note").unwrap();
    assert_eq!(parsed, ContentMode::Note);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-common skill::types::tests::note_category_serialises_lowercase_and_roundtrips`
Expected: COMPILE ERROR — `no variant named 'Note' found for enum 'Category'`.

- [ ] **Step 3: Add the `Note` variants**

In `mur-common/src/skill/types.rs`, the `Category` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Context,
    Workflow,
    Command,
    Meta,
    Note,
}
```

And the `ContentMode` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentMode {
    Context,
    Workflow,
    Command,
    Note,
}
```

(Keep the existing derives and `#[serde(rename_all = "lowercase")]` exactly — only the new variant is added.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-common skill::types::tests`
Expected: all type tests PASS (new + existing).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/types.rs
git commit -m "feat(skill): add Note variant to Category and ContentMode"
```

---

## Task 2: Add `Content.note` field and extend `mode()` to a 4-tuple

**Files:**
- Modify: `mur-common/src/skill/manifest.rs` — `Content` struct (~line 80), `mode()` (~line 96), two test literals (~222, ~233)
- Modify: `mur-common/src/skill/gene.rs:250`, `mur-core/src/cmd/skill_from_pattern.rs:90`, `mur-core/src/skill_index/text.rs:60`, `mur-core/src/retrieve/skill_candidates.rs` (fake_loaded)

- [ ] **Step 1: Write the failing test**

Add to the test module in `mur-common/src/skill/manifest.rs`:

```rust
#[test]
fn mode_returns_note_when_only_note_populated() {
    let c = Content {
        r#abstract: "a".into(),
        context: None,
        procedure: None,
        command: None,
        note: Some("# body".into()),
    };
    assert_eq!(c.mode(), Some(ContentMode::Note));
}

#[test]
fn mode_returns_none_when_note_and_context_both_populated() {
    let c = Content {
        r#abstract: "a".into(),
        context: Some("ctx".into()),
        procedure: None,
        command: None,
        note: Some("# body".into()),
    };
    assert_eq!(c.mode(), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common skill::manifest::tests::mode_returns_note_when_only_note_populated`
Expected: COMPILE ERROR — `struct 'Content' has no field named 'note'`.

- [ ] **Step 3: Add the `note` field to `Content`**

In `mur-common/src/skill/manifest.rs`, the `Content` struct becomes:

```rust
pub struct Content {
    /// Layer 2 — injected into the system prompt at session start.
    pub r#abstract: String,

    /// Exactly one of the following is `Some`: context / procedure / command / note.
    /// Enforced by schema validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procedure: Option<Procedure>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Note mode (category: note): free markdown body, stored inline in the
    /// canonical skill.yaml per the 1a storage decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
```

- [ ] **Step 4: Extend `Content::mode()` to a 4-tuple**

Replace the `mode()` body:

```rust
pub fn mode(&self) -> Option<ContentMode> {
    match (
        self.context.is_some(),
        self.procedure.is_some(),
        self.command.is_some(),
        self.note.is_some(),
    ) {
        (true, false, false, false) => Some(ContentMode::Context),
        (false, true, false, false) => Some(ContentMode::Workflow),
        (false, false, true, false) => Some(ContentMode::Command),
        (false, false, false, true) => Some(ContentMode::Note),
        _ => None,
    }
}
```

- [ ] **Step 5: Fix every `Content { ... }` literal so the workspace compiles**

Add `note: None,` to each of these struct literals:

- `mur-common/src/skill/manifest.rs` — the two existing test literals (the pre-existing `mode()` tests near lines 222 and 233). Add `note: None,` as the last field in each.
- `mur-common/src/skill/gene.rs:250` — inside `content: Content { ... }`, add `note: None,`.
- `mur-core/src/cmd/skill_from_pattern.rs:90` — inside `content: Content { ... }`, add `note: None,`.
- `mur-core/src/skill_index/text.rs:60` — inside `content: Content { ... }`, add `note: None,`.
- `mur-core/src/retrieve/skill_candidates.rs` — in the `fake_loaded` test helper's `Content { ... }`, add `note: None,`.

Then build to catch any literal this list missed:

Run: `cargo build --workspace`
Expected: clean. If the compiler reports `missing field 'note' in initializer of 'Content'` anywhere else, add `note: None,` there too.

- [ ] **Step 6: Run the manifest tests**

Run: `cargo test -p mur-common skill::manifest::tests`
Expected: all PASS, including the two new `mode()` tests and the pre-existing ones.

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/skill/manifest.rs mur-common/src/skill/gene.rs \
  mur-core/src/cmd/skill_from_pattern.rs mur-core/src/skill_index/text.rs \
  mur-core/src/retrieve/skill_candidates.rs
git commit -m "feat(skill): add Content.note field; mode() recognizes note mode

note body stored inline in skill.yaml (1a). All Content literals updated."
```

---

## Task 3: Validate the note content mode

**Files:**
- Modify: `mur-common/src/skill/validate.rs` — disambiguation array (~line 70), `mode_matches_category` (~line 168), error-message strings (~line 37 and ~41)

- [ ] **Step 1: Write the failing tests**

Add to the test module in `mur-common/src/skill/validate.rs`:

```rust
#[test]
fn valid_note_manifest_passes() {
    let yaml = "name: rust-notes\nversion: 1.0.0\npublisher: human:test\n\
                category: note\ndescription: d\n\
                content:\n  abstract: a\n  note: |\n    # body\n";
    let m = parse_canonical(yaml).unwrap();
    assert!(validate(&m).is_ok());
}

#[test]
fn note_category_with_context_mode_is_mismatch() {
    let yaml = "name: rust-notes\nversion: 1.0.0\npublisher: human:test\n\
                category: note\ndescription: d\n\
                content:\n  abstract: a\n  context: c\n";
    let m = parse_canonical(yaml).unwrap();
    assert!(matches!(
        validate(&m),
        Err(ValidationError::ContentModeMismatch { .. })
    ));
}

#[test]
fn note_plus_command_is_multiple_modes() {
    let yaml = "name: rust-notes\nversion: 1.0.0\npublisher: human:test\n\
                category: note\ndescription: d\n\
                content:\n  abstract: a\n  note: x\n  command: y\n";
    let m = parse_canonical(yaml).unwrap();
    assert!(matches!(validate(&m), Err(ValidationError::MultipleContentModes)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-common skill::validate::tests::valid_note_manifest_passes`
Expected: FAIL — `mode_matches_category` returns false for `(Note, Note)`, so a valid note is rejected as `ContentModeMismatch`.

- [ ] **Step 3: Add the `(Note, Note)` arm to `mode_matches_category`**

In `mur-common/src/skill/validate.rs`:

```rust
fn mode_matches_category(cat: Category, mode: ContentMode) -> bool {
    matches!(
        (cat, mode),
        (Category::Workflow, ContentMode::Workflow)
            | (Category::Command, ContentMode::Command)
            | (Category::Context, ContentMode::Context)
            | (Category::Meta, ContentMode::Context)
            | (Category::Note, ContentMode::Note)
    )
}
```

- [ ] **Step 4: Add `note` to the multiple-modes disambiguation**

In the `validate` function, the `populated` count array must include note:

```rust
    let mode = m.content.mode().ok_or_else(|| {
        let populated = [
            m.content.context.is_some(),
            m.content.procedure.is_some(),
            m.content.command.is_some(),
            m.content.note.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if populated > 1 {
            ValidationError::MultipleContentModes
        } else {
            ValidationError::NoContentMode
        }
    })?;
```

- [ ] **Step 5: Update the two error-message strings to mention `note`**

In the `Display` impl for `ValidationError`:

```rust
            NoContentMode => write!(
                f,
                "content must populate exactly one of: context / procedure / command / note"
            ),
            MultipleContentModes => write!(
                f,
                "content must populate only one of: context / procedure / command / note"
            ),
```

- [ ] **Step 6: Run the validate tests**

Run: `cargo test -p mur-common skill::validate::tests`
Expected: all PASS (3 new + existing).

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/skill/validate.rs
git commit -m "feat(skill): validate note content mode (Category::Note <-> ContentMode::Note)"
```

---

## Task 4: Surface the note body in `LoadedSkill::text()`

**Files:**
- Modify: `mur-core/src/retrieve/skill_candidates.rs` — `text()` in `impl Retrievable for LoadedSkill`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `mur-core/src/retrieve/skill_candidates.rs`:

```rust
#[test]
fn text_includes_note_body_when_present() {
    use mur_common::skill::manifest::Content;
    use mur_common::skill::types::Category;

    let mut s = fake_loaded("note-skill", Priority::Normal);
    s.manifest.category = Category::Note;
    s.manifest.content = Content {
        r#abstract: "abstract line".into(),
        context: None,
        procedure: None,
        command: None,
        note: Some("the note body about rust errors".into()),
    };
    let text = s.text();
    assert!(text.contains("abstract line"));
    assert!(text.contains("the note body about rust errors"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests::text_includes_note_body_when_present`
Expected: FAIL — the current `text()` only concatenates `abstract` + `description`, so the note body is absent.

- [ ] **Step 3: Extend `text()` to append the note body**

In `mur-core/src/retrieve/skill_candidates.rs`, replace the `text` method in `impl Retrievable for LoadedSkill`:

```rust
    fn text(&self) -> std::borrow::Cow<'_, str> {
        // abstract + description is the base keyword/embed surface; note-mode
        // skills append their markdown body so they rank on their content.
        let mut s = format!(
            "{}\n{}",
            self.manifest.content.r#abstract, self.manifest.description
        );
        if let Some(note) = &self.manifest.content.note {
            s.push('\n');
            s.push_str(note);
        }
        std::borrow::Cow::Owned(s)
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p mur-core retrieve::skill_candidates::tests::text_includes_note_body_when_present`
Expected: PASS.

- [ ] **Step 5: Run the full retrieve test suite**

Run: `cargo test -p mur-core retrieve::`
Expected: all PASS — the existing `retrievable_accessors_reflect_manifest_and_stats` test (which asserts `text()` equals `abstract\ndescription` for a non-note skill) still holds, because `note` is `None` there.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/retrieve/skill_candidates.rs
git commit -m "feat(retrieve): LoadedSkill::text() includes note body for ranking"
```

---

## Task 5: Verification gate — full workspace and lints

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass. New tests added by this plan: 2 (types) + 2 (manifest mode) + 3 (validate) + 1 (skill_candidates text) = 8.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. Adding enum variants can trigger `clippy` on any non-wildcard match elsewhere — if clippy (or the build) flags a newly non-exhaustive `match` on `Category`/`ContentMode`, add the missing arm with the correct behavior (do not add a catch-all that silently swallows `Note`).

- [ ] **Step 3: Run `cargo fmt --check`**

Run: `cargo fmt --check`
Expected: clean. If not:

```bash
cargo fmt
git add -u
git commit --amend --no-edit
```

- [ ] **Step 4: Confirm a note skill round-trips on disk**

Run this manual check to confirm the canonical serializer preserves `content.note`:

```bash
cargo run -p mur-core -- skill validate --path /dev/stdin <<'EOF'
name: rust-error-handling
version: 1.0.0
publisher: human:test
category: note
description: Rust error handling reference
content:
  abstract: Rust error handling best practices
  note: |
    # Rust Error Handling
    Use anyhow for app errors, thiserror for library errors.
EOF
```

Expected: validation succeeds (no error printed; exit 0). If `mur skill validate` does not accept `/dev/stdin`, write the YAML to a temp file first and pass its path.

- [ ] **Step 5: Final commit if cleanup was needed**

If Steps 2-3 required fixes, the amend above handles it. Otherwise nothing extra to commit.

---

## Done state

After this plan:

- `Category::Note` and `ContentMode::Note` exist and serialize as `note`.
- `Content.note: Option<String>` holds a note's markdown body inline in `skill.yaml` (1a storage).
- `Content::mode()` recognizes note mode; `validate` enforces `category: note` ⇔ `content.note` and rejects multiple/zero modes with note-aware messages.
- `LoadedSkill::text()` includes the note body, so a `category: note` skill ranks on its content through `score_and_rank_generic` (Plan 1 + Plan 2) with no further changes.
- All existing skill tests still green.

**What this unlocks (next plans):**

- **Notes N2 — `mur notes` CLI facade** (`create`/`show`/`list`/`search`/`edit`): can write `category: note` skills and search them via `score_and_rank_generic`, exercising the whole foundation end-to-end.
- **Notes N5 — MCP `note_create`/`skill_search(category: note)`**.
- **Notes export/ingest** via the existing `parse_markdown` round-trip.

**What this does NOT do:**

- No CLI surface yet (`mur notes ...`) — separate plan.
- No vector embedding of note bodies into LanceDB — needs the corpus index plan.
- No Pattern removal — independent cleanup, separate plan.
