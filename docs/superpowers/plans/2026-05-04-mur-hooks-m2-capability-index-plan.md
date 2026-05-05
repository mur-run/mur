# mur Hooks M2 — Capability Index (L0) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a `~400 LOC` L0 capability index that `mur hook session-start` injects once per session — a compact markdown list of `name — description` pairs, budget-capped at 600 tokens, project-filtered.

**Architecture:** A new `inject::index` module provides three pure functions: `build()` (filter + sort patterns into index entries), `format_l0()` (render to markdown within a char budget), and `save()`/`load()` (persist to `~/.mur/index/capabilities.json`). `cmd_hook_session_start` calls these in sequence and prints the result to stdout, replacing the M1 stub. No LLM, no async, no embedding.

**Tech Stack:** Rust 2024 edition, `serde_json` (disk persistence), existing `YamlStore`, `mur_common::pattern::{Pattern, LifecycleStatus}`. Pattern fields are accessible directly via `Deref<Target = KnowledgeBase>` — `pattern.name`, `pattern.description`, `pattern.applies.projects`, `pattern.lifecycle.status`, `pattern.lifecycle.muted`, `pattern.importance`.

---

## Task 1: `inject::index` — types + `build()` function

**Files:**
- Create: `mur-core/src/inject/index.rs`
- Modify: `mur-core/src/inject/mod.rs`

**Step 1: Write the failing test** (put at the bottom of the new file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::{
        Applies, Content, Lifecycle, LifecycleStatus, Pattern, Tags, Tier,
    };
    use mur_common::knowledge::{DecayMeta, Evidence, KnowledgeBase, Links, Maturity};

    fn make_pattern(name: &str, desc: &str, importance: f64) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: name.to_owned(),
                description: desc.to_owned(),
                content: Content::Plain(String::new()),
                tier: Tier::Project,
                importance,
                confidence: 0.8,
                tags: Tags::default(),
                applies: Applies::default(),
                evidence: Evidence::default(),
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                maturity: Maturity::Stable,
                decay: DecayMeta::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            kind: None,
            attachments: Vec::new(),
        }
    }

    fn archived_pattern(name: &str) -> Pattern {
        let mut p = make_pattern(name, "archived", 0.9);
        p.base.lifecycle.status = LifecycleStatus::Archived;
        p
    }

    fn muted_pattern(name: &str) -> Pattern {
        let mut p = make_pattern(name, "muted", 0.9);
        p.base.lifecycle.muted = true;
        p
    }

    fn project_pattern(name: &str, project: &str) -> Pattern {
        let mut p = make_pattern(name, "project-specific", 0.7);
        p.base.applies.projects = vec![project.to_owned()];
        p
    }

    #[test]
    fn build_excludes_archived() {
        let patterns = vec![
            make_pattern("active", "Active pattern", 0.8),
            archived_pattern("gone"),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].name, "active");
    }

    #[test]
    fn build_excludes_muted() {
        let patterns = vec![
            make_pattern("visible", "Visible", 0.8),
            muted_pattern("silent"),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries.len(), 1);
    }

    #[test]
    fn build_sorts_by_importance_descending() {
        let patterns = vec![
            make_pattern("low", "Low", 0.3),
            make_pattern("high", "High", 0.9),
            make_pattern("mid", "Mid", 0.6),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries[0].name, "high");
        assert_eq!(idx.entries[1].name, "mid");
        assert_eq!(idx.entries[2].name, "low");
    }

    #[test]
    fn build_with_project_filters_correctly() {
        let patterns = vec![
            make_pattern("universal", "Universal (empty applies.projects)", 0.8),
            project_pattern("for-mur", "mur"),
            project_pattern("for-other", "other-project"),
        ];
        // "mur" project — should include universal + for-mur, exclude for-other
        let idx = build(&patterns, Some("mur"));
        let names: Vec<_> = idx.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"universal"));
        assert!(names.contains(&"for-mur"));
        assert!(!names.contains(&"for-other"));
    }

    #[test]
    fn build_with_no_project_includes_universal_only() {
        let patterns = vec![
            make_pattern("universal", "Universal", 0.8),
            project_pattern("specific", "mur"),
        ];
        // No project context — include only universal (empty applies.projects)
        let idx = build(&patterns, None);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].name, "universal");
    }

    #[test]
    fn build_empty_patterns_gives_empty_index() {
        let idx = build(&[], None);
        assert!(idx.entries.is_empty());
    }
}
```

**Step 2: Verify the test fails**

```bash
cargo test -p mur-core inject::index 2>&1 | head -10
```
Expected: compile error — module `index` not found.

**Step 3: Create `mur-core/src/inject/index.rs`** with the implementation:

```rust
use mur_common::pattern::{LifecycleStatus, Pattern};
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityEntry {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityIndex {
    pub entries: Vec<CapabilityEntry>,
    pub project: Option<String>,
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Build a capability index from a pattern list.
///
/// Filters out Archived and Muted patterns. If `project` is `Some`, includes
/// patterns whose `applies.projects` is empty (universal) OR contains the
/// project name. If `project` is `None`, only universal patterns are included.
/// Entries are sorted by importance descending.
#[allow(dead_code)] // called from cmd_hook_session_start in Task 4
pub fn build(patterns: &[Pattern], project: Option<&str>) -> CapabilityIndex {
    let mut entries: Vec<(f64, CapabilityEntry)> = patterns
        .iter()
        .filter(|p| {
            p.lifecycle.status != LifecycleStatus::Archived && !p.lifecycle.muted
        })
        .filter(|p| {
            let projs = &p.applies.projects;
            if projs.is_empty() {
                return true; // universal
            }
            match project {
                Some(proj) => projs.iter().any(|s| s == proj || s == "*"),
                None => false,
            }
        })
        .map(|p| {
            (
                p.importance,
                CapabilityEntry {
                    name: p.name.clone(),
                    description: p.description.clone(),
                },
            )
        })
        .collect();

    // Sort by importance descending, then name ascending for stability
    entries.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.name.cmp(&b.1.name))
    });

    CapabilityIndex {
        entries: entries.into_iter().map(|(_, e)| e).collect(),
        project: project.map(str::to_owned),
    }
}

// ── Tests (at bottom of file — copy from Step 1) ─────────────────────────────
```

Add the test module from Step 1 at the bottom of the file.

**Step 4: Add `pub mod index;` to `mur-core/src/inject/mod.rs`**

```
pub mod event;
pub mod hook;
pub mod index;
pub mod queue;
pub mod sync;
```

**Step 5: Run tests**

```bash
cargo test -p mur-core inject::index 2>&1 | tail -15
```
Expected: `ok. 6 passed`

**Note on Pattern struct:** If the `Pattern` struct field is `base: KnowledgeBase` (not `pub base`), access fields via `p.name` (Deref), `p.lifecycle.status`, `p.applies.projects`, `p.importance`. Check the actual field name by reading `mur-common/src/pattern.rs` around line 1 and `grep "struct Pattern"`.

**Step 6: Clippy + fmt**

```bash
cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error" | wc -l
cargo fmt -p mur-core
```

**Step 7: Commit**

```bash
git add mur-core/src/inject/index.rs mur-core/src/inject/mod.rs
git commit -m "feat(inject): CapabilityIndex builder — filter/sort/project-aware pattern index"
```

---

## Task 2: `format_l0()` — markdown formatter with budget cap

**Files:**
- Modify: `mur-core/src/inject/index.rs` — add `format_l0()` function + tests

**Step 1: Write the failing tests** (add to `tests` module in `index.rs`):

```rust
    #[test]
    fn format_l0_produces_header_and_list() {
        let idx = CapabilityIndex {
            entries: vec![
                CapabilityEntry { name: "tokio-async".into(), description: "Tokio async runtime patterns".into() },
                CapabilityEntry { name: "clap-derive".into(), description: "Clap derive API for CLI parsing".into() },
            ],
            project: Some("mur".into()),
        };
        let out = format_l0(&idx, 4000);
        assert!(out.contains("## mur learning index"), "must have header");
        assert!(out.contains("project: mur"), "must show project name");
        assert!(out.contains("`tokio-async`"), "must list pattern names");
        assert!(out.contains("Tokio async runtime patterns"), "must include descriptions");
        assert!(out.contains("mur recall"), "must have recall footer");
    }

    #[test]
    fn format_l0_without_project_omits_project_name() {
        let idx = CapabilityIndex {
            entries: vec![
                CapabilityEntry { name: "anyhow".into(), description: "Error handling".into() },
            ],
            project: None,
        };
        let out = format_l0(&idx, 4000);
        assert!(out.contains("## mur learning index"), "must have header");
        assert!(!out.contains("project:"), "must not show project when None");
    }

    #[test]
    fn format_l0_empty_index_returns_empty_string() {
        let idx = CapabilityIndex { entries: vec![], project: None };
        assert_eq!(format_l0(&idx, 4000), "");
    }

    #[test]
    fn format_l0_truncates_at_budget() {
        // Each entry is ~50 chars; budget of 200 chars should include only ~3-4 entries
        let entries: Vec<_> = (0..20)
            .map(|i| CapabilityEntry {
                name: format!("pattern-{i:02}"),
                description: format!("Description number {i}"),
            })
            .collect();
        let idx = CapabilityIndex { entries, project: None };
        let out = format_l0(&idx, 250); // tight budget
        // Should not include all 20 entries
        assert!(!out.contains("pattern-19"), "should truncate before last entry");
        // Should still have header
        assert!(out.contains("## mur learning index"));
    }

    #[test]
    fn format_l0_respects_600_token_cap() {
        // 600 tokens ≈ 2400 chars; verify format_l0 with default budget stays under
        let entries: Vec<_> = (0..50)
            .map(|i| CapabilityEntry {
                name: format!("pat-{i}"),
                description: "A reasonably long description for a pattern entry here".into(),
            })
            .collect();
        let idx = CapabilityIndex { entries, project: Some("myproject".into()) };
        let out = format_l0(&idx, 2400); // 600 tokens × 4 chars/token
        assert!(out.len() <= 2400 + 200, "output must fit within token budget (with footer)");
    }
```

**Step 2: Run to verify failure**

```bash
cargo test -p mur-core inject::index::tests::format_l0 2>&1 | head -10
```
Expected: compile error — `format_l0` not found.

**Step 3: Add `format_l0()` to `index.rs`** (above the test module):

```rust
/// Format a capability index as an L0 markdown injection.
///
/// `budget_chars` is the character budget (≈ tokens × 4). Returns empty string
/// if there are no entries. Truncates entry list when adding the next entry
/// would exceed the budget.
#[allow(dead_code)] // called from cmd_hook_session_start in Task 4
pub fn format_l0(index: &CapabilityIndex, budget_chars: usize) -> String {
    if index.entries.is_empty() {
        return String::new();
    }

    let header = match &index.project {
        Some(proj) => format!("## mur learning index (project: {proj})\n"),
        None => "## mur learning index\n".to_owned(),
    };
    let footer = "\nRun `mur recall <name>` to load full content of any item above.\n";

    // Reserve space for header + footer
    let overhead = header.len() + footer.len();
    let entry_budget = budget_chars.saturating_sub(overhead);

    let mut body = String::new();
    for entry in &index.entries {
        let line = format!("- `{}` — {}\n", entry.name, entry.description);
        if body.len() + line.len() > entry_budget {
            break;
        }
        body.push_str(&line);
    }

    if body.is_empty() {
        return String::new();
    }

    format!("{header}{body}{footer}")
}
```

**Step 4: Run tests**

```bash
cargo test -p mur-core inject::index 2>&1 | tail -15
```
Expected: all 11 tests pass (6 from Task 1 + 5 new).

**Step 5: Clippy + fmt + commit**

```bash
cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error" | wc -l
cargo fmt -p mur-core
git add mur-core/src/inject/index.rs
git commit -m "feat(inject): format_l0 — markdown formatter with char-budget truncation"
```

---

## Task 3: `save()` / `load()` — disk persistence

**Files:**
- Modify: `mur-core/src/inject/index.rs` — add `save()` + `load()` functions + tests

**Step 1: Add tests for persistence** (inside the existing `tests` module):

```rust
    #[test]
    fn save_and_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capabilities.json");

        let idx = CapabilityIndex {
            entries: vec![
                CapabilityEntry { name: "foo".into(), description: "Foo pattern".into() },
                CapabilityEntry { name: "bar".into(), description: "Bar pattern".into() },
            ],
            project: Some("testproject".into()),
        };

        save_to(&idx, &path).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].name, "foo");
        assert_eq!(loaded.project.as_deref(), Some("testproject"));
    }

    #[test]
    fn load_from_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let loaded = load_from(&path).unwrap();
        assert!(loaded.entries.is_empty());
        assert!(loaded.project.is_none());
    }
```

**Step 2: Run to verify failure**

```bash
cargo test -p mur-core inject::index::tests::save_and_load 2>&1 | head -10
```
Expected: compile error — `save_to` / `load_from` not found.

**Step 3: Add `save_to()`, `load_from()`, `save()`, `load()` to `index.rs`** (above test module):

```rust
/// Save index to an explicit path (used in tests with tempdir).
pub fn save_to(index: &CapabilityIndex, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(index)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load index from an explicit path; returns empty index if file missing.
pub fn load_from(path: &std::path::Path) -> anyhow::Result<CapabilityIndex> {
    if !path.exists() {
        return Ok(CapabilityIndex { entries: vec![], project: None });
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

/// Save capability index to `~/.mur/index/capabilities.json`.
#[allow(dead_code)]
pub fn save(index: &CapabilityIndex) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let path = home.join(".mur").join("index").join("capabilities.json");
    save_to(index, &path)
}

/// Load capability index from `~/.mur/index/capabilities.json`.
#[allow(dead_code)]
pub fn load() -> anyhow::Result<CapabilityIndex> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let path = home.join(".mur").join("index").join("capabilities.json");
    load_from(&path)
}
```

**Add `use anyhow::Result;` to imports at the top of `index.rs`** if not already there.

**Step 4: Run tests**

```bash
cargo test -p mur-core inject::index 2>&1 | tail -15
```
Expected: all 13 tests pass.

**Step 5: Clippy + fmt + commit**

```bash
cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error" | wc -l
cargo fmt -p mur-core
git add mur-core/src/inject/index.rs
git commit -m "feat(inject): capability index disk persistence — save_to/load_from with tempdir-testable helpers"
```

---

## Task 4: Wire `cmd_hook_session_start` + end-to-end tests

**Files:**
- Modify: `mur-core/src/cmd/hook.rs` — flesh out `cmd_hook_session_start`
- Create: `mur-core/tests/session_start_integration.rs`

**Step 1: Write the integration test**

Create `mur-core/tests/session_start_integration.rs`:

```rust
//! Verifies that inject::index correctly builds and formats an L0 index.
//! Does not test mur hook session-start end-to-end (requires real ~/.mur/patterns/).

use mur_core::inject::index::{CapabilityEntry, CapabilityIndex, format_l0};

#[test]
fn l0_output_fits_within_600_token_budget() {
    // 600 tokens ≈ 2400 chars
    let entries: Vec<_> = (0..30)
        .map(|i| CapabilityEntry {
            name: format!("pattern-{i}"),
            description: format!("A test description for pattern number {i} that is moderately long"),
        })
        .collect();
    let idx = CapabilityIndex {
        entries,
        project: Some("myproject".into()),
    };
    let out = format_l0(&idx, 2400);
    // 2400 chars budget + some slack for header/footer (both < 100 chars)
    assert!(
        out.len() <= 2600,
        "L0 output too long: {} chars",
        out.len()
    );
    assert!(out.contains("## mur learning index"));
    assert!(out.contains("project: myproject"));
    assert!(out.contains("mur recall"));
}

#[test]
fn l0_output_has_correct_format_per_entry() {
    let idx = CapabilityIndex {
        entries: vec![CapabilityEntry {
            name: "tokio-async-runtime".into(),
            description: "Tokio: spawn / select! / time::sleep".into(),
        }],
        project: None,
    };
    let out = format_l0(&idx, 4000);
    assert!(
        out.contains("- `tokio-async-runtime` — Tokio: spawn / select! / time::sleep"),
        "entry line format must be: - `name` — description\ngot:\n{out}"
    );
}

#[test]
fn empty_index_produces_no_output() {
    let idx = CapabilityIndex { entries: vec![], project: None };
    assert_eq!(format_l0(&idx, 4000), "");
}
```

**Step 2: Run to verify tests compile and pass**

```bash
cargo test -p mur-core --test session_start_integration 2>&1 | tail -10
```
Expected: 3 passed.

**Step 3: Flesh out `cmd_hook_session_start` in `hook.rs`**

Replace the current stub (lines ~105-111) with:

```rust
pub(crate) async fn cmd_hook_session_start(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::SessionStart, tool);
    let _ = enqueue(&event);

    // Build L0 capability index (project-filtered, sorted by importance)
    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;

    let project = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    let index = crate::inject::index::build(&patterns, project.as_deref());
    let _ = crate::inject::index::save(&index); // cache for daemon (M3)

    // 600 tokens × 4 chars/token = 2400 chars
    const L0_BUDGET_CHARS: usize = 2400;
    let output = crate::inject::index::format_l0(&index, L0_BUDGET_CHARS);

    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}
```

**Step 4: Build**

```bash
cargo build -p mur-core 2>&1 | grep "^error" | head -10
```
Fix any compile errors before continuing.

**Step 5: Run full test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: all tests pass.

**Step 6: Smoke test**

```bash
# Session-start with empty stdin — must not crash and may emit L0 index
echo '{"session_id": "test_session"}' | cargo run -q -- hook session-start --tool claude 2>&1 | head -10
```
Expected: either empty output (no patterns in `~/.mur/patterns/`) or a formatted L0 index beginning with `## mur learning index`.

**Step 7: Clippy + fmt**

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep "^error" | wc -l
cargo fmt --check 2>&1 || cargo fmt
```

**Step 8: Commit**

```bash
git add mur-core/src/cmd/hook.rs mur-core/tests/session_start_integration.rs
git commit -m "feat(cmd/hook): session-start now injects L0 capability index (M2)"
```

**Step 9: Push + update PR**

```bash
git push origin feat/m0-adaptive-gate
```
The existing PR #154 will update automatically with the M2 commits.

---

## Notes for the implementer

- **Pattern struct access:** `Pattern` uses `Deref<Target = KnowledgeBase>`, so `pattern.name`, `pattern.description`, `pattern.importance`, `pattern.lifecycle.status`, `pattern.lifecycle.muted`, `pattern.applies.projects` all work directly without `.base.`. Verify by looking at `mur-common/src/pattern.rs` if unsure.
- **`LifecycleStatus` import:** `use mur_common::pattern::LifecycleStatus;` — it's in the `pattern` module even though `LifecycleStatus` is defined there.
- **Test helper patterns:** If the compiler complains about `mur_common::knowledge::Evidence`, `mur_common::knowledge::Links`, etc. being private or having different paths, check `mur-common/src/lib.rs` for the public re-exports.
- **Budget arithmetic:** 600 tokens × 4 chars/token = 2400 chars. The `format_l0` function takes a `budget_chars: usize` parameter so callers control the budget without baking in magic numbers.
- **`#[allow(dead_code)]` on `save` / `load`:** The top-level `save()` / `load()` (which use `~/.mur/index/`) are called from `cmd_hook_session_start`, so their `#[allow(dead_code)]` can be removed once wired in Task 4.
- **M3 will consume the saved index:** `~/.mur/index/capabilities.json` is written here so the daemon (M3) can use it for pre-building the inbox. In M2 it's written but nothing reads it back except for completeness.
