# mur Conversations Phase 3.5 — LLM-Abstractive Hit Compression Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Stage 1b — an Ollama-backed abstractive hit-summarization stage — between the existing Stage 1 (heuristic compress) and Stage 2 (drop history) of `ask::prompt::render`, so overflow queries preserve multi-turn history at the cost of a one-time, cache-backed LLM call per hit.

**Architecture:** Two new modules under `mur-core/src/conversations/ask/` — `cache.rs` (pure file-per-key K/V) and `abstractive.rs` (prompt template, timeout, validator, per-hit orchestration). `prompt::render` becomes `async`, gains an optional `AbstractiveCtx`, and threads final (possibly-mutated) hits + `Stage1bSummary` back to `ask_stream`, which surfaces them via `AskEvent::Done` → `AskResponse.stage_1b`. A `Compression { Heuristic, Abstractive }` enum drives provenance on `ResolvedHit`/`Citation` and flows into plain-mode `(summarized)` suffixes, JSON `compressed` fields, and the `· N summarized` footer segment.

**Tech Stack:** Rust 2024, tokio, reqwest, serde, sha2, hex, anyhow, tracing. No new crate dependencies — `sha2` and `hex` are already in the workspace.

**Base directory for all paths in this plan:** `/Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-5/`. Every path below is relative to that worktree root.

**Source of truth:** `docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-5-design.md` (design §1–§11). Reference it when judgment calls arise on anything not pinned here.

---

## Task 1: Config — add `summarize_hits_enabled` + `summarize_model` to `AskConfig`

**Files:**
- Modify: `mur-common/src/config.rs:319-397` (AskConfig struct, Default impl, default helpers)
- Modify: `mur-common/src/config.rs:751-811` (existing `#[cfg(test)] mod tests` block for AskConfig)

- [ ] **Step 1: Write the failing tests**

Append the following tests inside the existing `#[cfg(test)] mod tests` block (the one starting around line 751 that already tests `ask_config_defaults`):

```rust
#[test]
fn ask_config_default_summarize_hits_enabled_is_true() {
    let c = AskConfig::default();
    assert!(c.summarize_hits_enabled);
}

#[test]
fn ask_config_default_summarize_model_is_none() {
    let c = AskConfig::default();
    assert!(c.summarize_model.is_none());
}

#[test]
fn ask_config_yaml_roundtrip_preserves_summarize_fields() {
    let y = r#"
conversations:
  ask:
    summarize_hits_enabled: false
    summarize_model: qwen3:4b
"#;
    let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
    let conv: ConversationsConfig =
        serde_yaml::from_value(v["conversations"].clone()).unwrap();
    assert!(!conv.ask.summarize_hits_enabled);
    assert_eq!(conv.ask.summarize_model.as_deref(), Some("qwen3:4b"));
}

#[test]
fn ask_config_yaml_without_summarize_fields_uses_defaults() {
    // Phase 3.5 must be additive: an existing config.yaml with NO
    // summarize_* keys must still parse and default to enabled=true,
    // model=None.
    let y = r#"
conversations:
  ask:
    model: qwen3:14b
"#;
    let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
    let conv: ConversationsConfig =
        serde_yaml::from_value(v["conversations"].clone()).unwrap();
    assert!(conv.ask.summarize_hits_enabled);
    assert!(conv.ask.summarize_model.is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p mur-common ask_config_default_summarize`
Expected: FAIL — `summarize_hits_enabled`/`summarize_model` not defined on `AskConfig`.

- [ ] **Step 3: Add the two new fields to `AskConfig` struct and its `Default` impl**

In `mur-common/src/config.rs`, inside `pub struct AskConfig { ... }` (around line 319), append two fields right after the existing `compress_hits_enabled` field:

```rust
    #[serde(default = "ask_default_summarize_hits_enabled")]
    pub summarize_hits_enabled: bool,
    #[serde(default)]
    pub summarize_model: Option<String>,
```

In the same file, inside `impl Default for AskConfig { fn default() -> Self { Self { ... } } }` (around line 346), append two fields right after `compress_hits_enabled: ask_default_compress_hits_enabled()`:

```rust
            summarize_hits_enabled: ask_default_summarize_hits_enabled(),
            summarize_model: None,
```

Also add a helper function alongside the other `ask_default_*` helpers (after `ask_default_compress_hits_enabled` around line 395):

```rust
fn ask_default_summarize_hits_enabled() -> bool {
    true
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-common`
Expected: PASS. All existing AskConfig tests still pass; the four new ones pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(conversations): Phase 3.5 Task 1 — AskConfig.summarize_hits_enabled + summarize_model"
```

---

## Task 2: `cache.rs` — file-per-key filesystem cache

**Files:**
- Create: `mur-core/src/conversations/ask/cache.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs:7-14` (add `pub mod cache;`)

- [ ] **Step 1: Declare the new module**

In `mur-core/src/conversations/ask/mod.rs`, insert a new line after `pub mod compress;` (around line 9):

```rust
pub mod cache;
```

- [ ] **Step 2: Write the failing tests in the new module**

Create `mur-core/src/conversations/ask/cache.rs` with module skeleton + full test suite:

```rust
//! File-per-key filesystem cache for Phase 3.5 abstractive hit summaries.
//!
//! Pure I/O, no LLM knowledge. Values are UTF-8 text. Keys are 64-char lowercase
//! hex (sha256). Layout: `~/.mur/conversations/cache/abstractive/<key>.txt`.
//! Writes use temp + rename for atomicity (matches `store/yaml.rs`).
#![allow(dead_code)] // wired by Task 5 (abstractive::compress_hit).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Root for all abstractive-summary cache files.
pub fn cache_dir(root_override: Option<&str>) -> PathBuf {
    super::super::paths::conversations_root(root_override)
        .join("cache")
        .join("abstractive")
}

/// Deterministic cache key: `sha256("mur-abstract-v1" || "|" || model || "|" ||
/// target_tokens || "|" || content)` → 64-char lowercase hex.
/// Bump the version prefix literal (`"mur-abstract-v1"`) whenever the prompt
/// template or validator semantics change, so old cache entries naturally
/// become misses rather than requiring a sweep.
pub fn cache_key(model: &str, target_tokens: usize, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"mur-abstract-v1|");
    h.update(model.as_bytes());
    h.update(b"|");
    h.update(target_tokens.to_le_bytes());
    h.update(b"|");
    h.update(content.as_bytes());
    hex::encode(h.finalize())
}

/// Read a value by key. Any filesystem error (missing file, permission denied,
/// I/O error) returns `None` — misses are the common case and must never
/// surface as errors to the overflow cascade.
pub fn cache_get(key: &str, root_override: Option<&str>) -> Option<String> {
    let path = cache_dir(root_override).join(format!("{key}.txt"));
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::debug!(?path, err = ?e, "cache miss (read error)");
            None
        }
    }
}

/// Write a value under a key. Uses temp-file + rename for atomicity.
/// Creates `cache_dir()` on first call.
pub fn cache_put(key: &str, value: &str, root_override: Option<&str>) -> Result<()> {
    let dir = cache_dir(root_override);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {dir:?}"))?;
    let final_path = dir.join(format!("{key}.txt"));
    let tmp_path = dir.join(format!("{key}.txt.tmp"));
    std::fs::write(&tmp_path, value).with_context(|| format!("write {tmp_path:?}"))?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {tmp_path:?} → {final_path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_tmp<R>(f: impl FnOnce(&str) -> R) -> R {
        let tmp = tempfile::tempdir().unwrap();
        f(tmp.path().to_str().unwrap())
    }

    #[test]
    fn cache_key_is_stable() {
        let a = cache_key("qwen3:14b", 128, "hello world");
        let b = cache_key("qwen3:14b", 128, "hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn cache_key_differs_by_model() {
        let a = cache_key("qwen3:14b", 128, "hello");
        let b = cache_key("qwen3:4b", 128, "hello");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_differs_by_target_tokens() {
        let a = cache_key("m", 128, "hello");
        let b = cache_key("m", 256, "hello");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_differs_by_content() {
        let a = cache_key("m", 128, "hello");
        let b = cache_key("m", 128, "world");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_put_then_get_roundtrip() {
        with_tmp(|root| {
            let key = cache_key("m", 64, "content A");
            cache_put(&key, "summary of A", Some(root)).unwrap();
            assert_eq!(cache_get(&key, Some(root)).as_deref(), Some("summary of A"));
        });
    }

    #[test]
    fn cache_get_missing_returns_none() {
        with_tmp(|root| {
            let key = cache_key("m", 64, "never written");
            assert!(cache_get(&key, Some(root)).is_none());
        });
    }

    #[test]
    fn cache_put_is_atomic_no_tmp_left_behind() {
        with_tmp(|root| {
            let key = cache_key("m", 64, "content");
            cache_put(&key, "val", Some(root)).unwrap();
            let dir = cache_dir(Some(root));
            let tmp_path = dir.join(format!("{key}.txt.tmp"));
            assert!(!tmp_path.exists(), "temp file must be renamed away");
            let final_path = dir.join(format!("{key}.txt"));
            assert!(final_path.exists());
        });
    }

    #[test]
    fn cache_put_creates_dir_on_first_call() {
        with_tmp(|root| {
            let dir = cache_dir(Some(root));
            assert!(!dir.exists(), "precondition: dir missing");
            let key = cache_key("m", 64, "x");
            cache_put(&key, "y", Some(root)).unwrap();
            assert!(dir.exists(), "cache_put should create the dir");
        });
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p mur-core cache::tests`
Expected: FAIL on module-not-found (before Step 1) or compile-error.

After Step 1 the module exists, so now re-run:
Run: `cargo test -p mur-core cache::tests`
Expected: All 7 tests PASS (the module's own code implements the test behaviour).

(TDD note: because this module has no pre-existing consumer, tests and code land together — the discipline here is _write test first within the file_, not _separate the commits_. If you prefer strict red-green, stub each fn to `unimplemented!()` first, confirm failure, then fill in.)

- [ ] **Step 4: `cargo fmt` + `cargo clippy` clean**

Run: `cargo fmt -p mur-core && cargo clippy -p mur-core -- -D warnings`
Expected: zero diffs, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/conversations/ask/cache.rs mur-core/src/conversations/ask/mod.rs
git commit -m "feat(conversations): Phase 3.5 Task 2 — ask::cache file-per-key filesystem cache"
```

---

## Task 3: `Compression` enum + `compressed` field on `ResolvedHit` and `Citation`

**Files:**
- Modify: `mur-core/src/conversations/ask/retrieve.rs:11-19` (add field to `ResolvedHit`)
- Modify: `mur-core/src/conversations/ask/mod.rs:55-65` (add field to `Citation`)
- Modify: `mur-core/src/conversations/ask/retrieve.rs` (all 6 `ResolvedHit { ... }` construction sites inside this file — lines 154, 174, 197, 224, 251, 415, 437, 467)
- Modify: `mur-core/src/conversations/ask/compress.rs:185-189`, `:233-247` (two struct-init sites + test helper)
- Modify: `mur-core/src/conversations/ask/prompt.rs:202-216`, `:254-267`, `:273-287`, `:399-412`, `:441-454` (five test struct-init sites)

We will place `enum Compression` in the new `abstractive.rs` module (created in Task 5). For now, while Task 5 hasn't landed yet, define it in `mod.rs` alongside `Citation` and move it into `abstractive.rs` with a re-export in Task 5 if desired. To avoid churn we'll put it in `mod.rs` permanently — it's a small enum and `mod.rs` is already the hub for shared ask types.

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/conversations/ask/mod.rs`'s existing `#[cfg(test)] mod tests` block (it's at the bottom of the file — the block that already holds `filters_default_shape`):

```rust
#[test]
fn compression_enum_serializes_lowercase() {
    assert_eq!(
        serde_json::to_string(&Compression::Heuristic).unwrap(),
        "\"heuristic\""
    );
    assert_eq!(
        serde_json::to_string(&Compression::Abstractive).unwrap(),
        "\"abstractive\""
    );
}

#[test]
fn citation_omits_compressed_field_when_none() {
    let c = Citation {
        id: 1,
        date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
        source: "cc".into(),
        conv_id: "c1".into(),
        line_hint: Some(1),
        span_index_in_summary: None,
        snippet: "s".into(),
        score: 0.9,
        compressed: None,
    };
    let j = serde_json::to_string(&c).unwrap();
    assert!(!j.contains("compressed"), "expected field omitted, got: {j}");
}

#[test]
fn citation_emits_compressed_field_when_set() {
    let c = Citation {
        id: 1,
        date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
        source: "cc".into(),
        conv_id: "c1".into(),
        line_hint: Some(1),
        span_index_in_summary: None,
        snippet: "s".into(),
        score: 0.9,
        compressed: Some(Compression::Abstractive),
    };
    let j = serde_json::to_string(&c).unwrap();
    assert!(j.contains("\"compressed\":\"abstractive\""), "got: {j}");
}

#[test]
fn citation_deserializes_legacy_json_without_compressed_field() {
    // Backwards compat: TurnRecord.citations is serde-persisted as JSONL in
    // ask-session.jsonl across versions. A pre-3.5 record has no `compressed`
    // key — must still parse.
    let j = r#"{"id":1,"date":"2026-04-22","source":"cc","conv_id":"c1","line_hint":1,"span_index_in_summary":null,"snippet":"s","score":0.9}"#;
    let c: Citation = serde_json::from_str(j).expect("legacy Citation must parse");
    assert!(c.compressed.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core conversations::ask::tests::compression_enum`
Expected: FAIL — `Compression` not defined.

- [ ] **Step 3: Add the `Compression` enum to `mod.rs`**

In `mur-core/src/conversations/ask/mod.rs`, add this enum just above the `Citation` struct (immediately before the existing `#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)] pub struct Citation`):

```rust
/// Provenance marker for hit snippets that have been reduced before going
/// into the LLM prompt. `Heuristic` → Phase 3.4 extractive compression (free).
/// `Abstractive` → Phase 3.5 LLM-summarized (paid). Written by the later
/// transformation, so a hit touched by both ends up marked `Abstractive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Heuristic,
    Abstractive,
}
```

- [ ] **Step 4: Extend `Citation` with `compressed: Option<Compression>`**

In the same file, extend the `Citation` struct:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    pub id: u32,
    pub date: chrono::NaiveDate,
    pub source: String, // file_prefix
    pub conv_id: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub snippet: String,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressed: Option<Compression>,
}
```

The `#[serde(default)]` is load-bearing: it lets pre-3.5 records in `ask-session.jsonl` deserialize without the field. The `skip_serializing_if` keeps new records minimal.

- [ ] **Step 5: Extend `ResolvedHit` with `compressed: Option<Compression>`**

In `mur-core/src/conversations/ask/retrieve.rs`, add a field to `ResolvedHit`:

```rust
#[derive(Debug, Clone)]
pub struct ResolvedHit {
    pub layer: i8,
    pub info: HitInfo,
    pub snippet: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub vector: Option<Vec<f32>>,
    pub compressed: Option<super::Compression>,
}
```

- [ ] **Step 6: Fix every `ResolvedHit { ... }` construction site**

There are exactly 13 constructor sites across 4 files (identified via `grep -n "ResolvedHit {"`). Add `compressed: None,` as the final field to each of these sites. The list:

1. `mur-core/src/conversations/ask/retrieve.rs:154` — inside `resolve_summary_hit` return.
2. `mur-core/src/conversations/ask/retrieve.rs:174` — inside `resolve_raw_hit` return.
3. `mur-core/src/conversations/ask/retrieve.rs:197` — inside `resolve_span_hit`-style branch.
4. `mur-core/src/conversations/ask/retrieve.rs:224` — weekly rollup branch.
5. `mur-core/src/conversations/ask/retrieve.rs:251` — monthly rollup branch.
6. `mur-core/src/conversations/ask/retrieve.rs:415` — test helper `mk`.
7. `mur-core/src/conversations/ask/retrieve.rs:437` — test helper `mk` (second).
8. `mur-core/src/conversations/ask/retrieve.rs:467` — test literal `h1`.
9. `mur-core/src/conversations/ask/compress.rs:185` — inside `compress_one` `ResolvedHit { snippet: new_snippet, ..h }`. **NOTE**: this one uses struct-update syntax (`..h`), so it automatically picks up `compressed: None` from `h`. Do **not** add a field here; the update syntax already carries it through. When we tag with `Heuristic` in Task 4 we'll change this line.
10. `mur-core/src/conversations/ask/compress.rs:233` — test helper `hit()`. Add `compressed: None,`.
11. `mur-core/src/conversations/ask/prompt.rs:202` — test helper `hit_raw()`. Add `compressed: None,`.
12. `mur-core/src/conversations/ask/prompt.rs:254` — test literal in `cite_anchor_layer_3_week_format`. Add `compressed: None,`.
13. `mur-core/src/conversations/ask/prompt.rs:273` — test literal in `cite_anchor_layer_4_month_format`. Add `compressed: None,`.
14. `mur-core/src/conversations/ask/prompt.rs:399` — test literal in `render_compresses_hits_on_overflow_when_enabled`. Add `compressed: None,`.
15. `mur-core/src/conversations/ask/prompt.rs:441` — test literal in `render_does_not_compress_when_disabled`. Add `compressed: None,`.

(Site 9 gets handled in Task 4 — skip it for this task.)

For each of sites 1–8 and 10–15, the compiler will emit `missing field 'compressed'` errors; add the literal:

```rust
compressed: None,
```

as the last field of each struct literal.

- [ ] **Step 7: Update `citations_map` to carry `compressed` from hits into Citations**

In `mur-core/src/conversations/ask/mod.rs`, find the `citations_map` fn (around line 337). Extend the `Citation { ... }` construction so the new field is populated:

```rust
fn citations_map(hits: &[retrieve::ResolvedHit]) -> std::collections::HashMap<String, Citation> {
    let mut m = std::collections::HashMap::new();
    for (i, h) in hits.iter().enumerate() {
        let anchor = prompt::cite_anchor(h);
        m.insert(
            anchor.clone(),
            Citation {
                id: (i + 1) as u32,
                date: h.info.date,
                source: h.info.source.clone(),
                conv_id: h.info.conv_id.clone(),
                line_hint: h.line_hint,
                span_index_in_summary: h.span_index_in_summary,
                snippet: h.snippet.clone(),
                score: h.info.score,
                compressed: h.compressed,
            },
        );
    }
    m
}
```

- [ ] **Step 8: Run tests to verify all pass**

Run: `cargo test -p mur-core && cargo test -p mur-common`
Expected: PASS. All existing tests still pass; the three new Citation tests pass.

- [ ] **Step 9: `cargo fmt` + `cargo clippy` clean**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: zero diffs, zero warnings.

- [ ] **Step 10: Commit**

```bash
git add mur-core/src/conversations/ask/mod.rs mur-core/src/conversations/ask/retrieve.rs mur-core/src/conversations/ask/compress.rs mur-core/src/conversations/ask/prompt.rs
git commit -m "feat(conversations): Phase 3.5 Task 3 — Compression enum + compressed field on ResolvedHit/Citation"
```

---

## Task 4: Tag Phase 3.4 heuristic output with `Compression::Heuristic`

**Files:**
- Modify: `mur-core/src/conversations/ask/compress.rs:161-189` (inside `compress_one`)

- [ ] **Step 1: Write the failing test**

Append this test to the `#[cfg(test)] mod tests` block at the bottom of `mur-core/src/conversations/ask/compress.rs` (after `compress_hits_preserves_citation_metadata`):

```rust
#[test]
fn compress_hits_tags_modified_hits_with_heuristic() {
    // A hit that actually gets compressed (>= MIN_CHARS + MIN_SENTENCES) must
    // come back tagged Compression::Heuristic. Phase 3.5 provenance contract.
    let long = (0..12)
        .map(|i| format!("Info sentence number {i} with extended body text content."))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(long.len() >= 400);
    let h = hit(&long);
    let out = compress_hits(vec![h], "info", 150);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].compressed,
        Some(crate::conversations::ask::Compression::Heuristic),
        "compressed hits must carry Heuristic provenance for Phase 3.5 to honor it"
    );
}

#[test]
fn compress_hits_leaves_skipped_hits_untagged() {
    // SKIP branch (too short) → compressed stays None.
    let h = hit("Short hit. Just two sentences.");
    let out = compress_hits(vec![h], "query", 10);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].compressed, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core compress::tests::compress_hits_tags_modified_hits_with_heuristic`
Expected: FAIL — `out[0].compressed` is `None` but test expects `Some(Heuristic)`.

- [ ] **Step 3: Tag modified hits with Heuristic in `compress_one`**

In `mur-core/src/conversations/ask/compress.rs`, modify the tail of `compress_one` (currently around line 185):

Old:
```rust
    let new_snippet = sorted
        .iter()
        .map(|&i| sentences[i])
        .collect::<Vec<_>>()
        .join(" ");
    ResolvedHit {
        snippet: new_snippet,
        ..h
    }
}
```

New:
```rust
    let new_snippet = sorted
        .iter()
        .map(|&i| sentences[i])
        .collect::<Vec<_>>()
        .join(" ");
    ResolvedHit {
        snippet: new_snippet,
        compressed: Some(super::Compression::Heuristic),
        ..h
    }
}
```

Note: SKIP branch (too short or too few sentences) above this path returns `h` unchanged — `compressed` stays whatever it was (likely `None`). The tag only applies when a real mutation happened.

- [ ] **Step 4: Run tests to verify all pass**

Run: `cargo test -p mur-core compress::tests`
Expected: PASS — both new tests + all existing compress tests.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/conversations/ask/compress.rs
git commit -m "feat(conversations): Phase 3.5 Task 4 — tag Phase 3.4 compressed hits with Compression::Heuristic"
```

---

## Task 5: `abstractive.rs` — per-hit compression (prompt, timeout, validator, cache integration)

**Files:**
- Create: `mur-core/src/conversations/ask/abstractive.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs` (add `pub mod abstractive;`)

- [ ] **Step 1: Declare the module**

In `mur-core/src/conversations/ask/mod.rs`, below `pub mod cache;` (added in Task 2), add:

```rust
pub mod abstractive;
```

- [ ] **Step 2: Write the new module with tests**

Create `mur-core/src/conversations/ask/abstractive.rs`:

```rust
//! Phase 3.5 Stage 1b — LLM-abstractive hit compression.
//!
//! Sits between Phase 3.4's heuristic Stage 1 and the existing Stage 2
//! (drop-oldest-history) in `prompt::render`'s overflow cascade. Per-hit,
//! largest-first, sequential; every call is wrapped in a 5-second timeout
//! and soft-fails (warn + keep original). Results cache to
//! `~/.mur/conversations/cache/abstractive/<sha256>.txt`.
//!
//! See `docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-5-design.md`.
#![allow(dead_code)] // wired by Task 8 (prompt::render integration).

use super::cache;
use super::retrieve::ResolvedHit;
use crate::conversations::ollama::{
    GenerateOptions, GenerateRequest, OllamaClient,
};
use std::time::Duration;

/// Prompt-version marker baked into cache keys. Bump when the prompt template
/// or validator changes — existing cached entries become natural misses
/// rather than needing a sweep.
pub const PROMPT_VERSION: &str = "mur-abstract-v1";

/// Fixed per-call timeout. Hardcoded by design (see spec §2 non-goals).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Floor for `target_tokens_per_hit` — never ask the LLM to squeeze below
/// ~60 tokens (prevents degenerate 1-word summaries).
pub const MIN_TARGET_TOKENS_PER_HIT: usize = 60;

/// Minimum content length before Stage 1b considers a hit. Mirrors
/// `compress::COMPRESS_MIN_CHARS` — no LLM call is worth it for < 400 chars.
pub const MIN_CONTENT_CHARS: usize = 400;

const SYSTEM_TEMPLATE: &str = "You compress text for retrieval context. Preserve entities, \
dates, numbers, and decisions. Do not add facts. Output only the summary — no preamble, \
no markdown.";

fn user_template(target_tokens: usize, content: &str) -> String {
    format!("Summarize the following in ≤{target_tokens} tokens.\n\n{content}")
}

/// Per-run Stage 1b context. Built once in `ask_stream` from `AskConfig`.
pub struct AbstractiveCtx<'a> {
    pub client: &'a OllamaClient,
    pub model: &'a str,
    pub timeout: Duration,
    pub root_override: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressOutcome {
    /// Fresh compression — Ollama call succeeded, cache was written.
    Compressed,
    /// Cache short-circuited — no LLM call made.
    CacheHit,
    /// Soft-fail — reason tag. See `SkipReason::*` constants.
    Skipped(&'static str),
}

pub mod skip_reason {
    pub const TIMEOUT: &str = "timeout";
    pub const EMPTY: &str = "empty";
    pub const NOT_SHORTER: &str = "not_shorter";
    pub const OLLAMA_ERR: &str = "ollama_err";
    pub const TOO_SHORT: &str = "too_short";
}

/// Aggregated stats from one `run_stage_1b` invocation. Drives log lines
/// and JSON output. `skipped` is per-hit detail for `tracing::warn!`.
pub struct Stage1bSummary {
    pub processed: usize,
    pub compressed_count: usize,
    pub cache_hits: usize,
    pub skipped: Vec<(usize, &'static str)>,
    pub duration_ms: u64,
}

/// Serializable slim projection for `AskResponse.stage_1b`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Stage1bStats {
    pub compressed_count: usize,
    pub cache_hits: usize,
    pub skipped_count: usize,
    pub duration_ms: u64,
}

impl Stage1bSummary {
    pub fn to_stats(&self) -> Stage1bStats {
        Stage1bStats {
            compressed_count: self.compressed_count,
            cache_hits: self.cache_hits,
            skipped_count: self.skipped.len(),
            duration_ms: self.duration_ms,
        }
    }
}

/// Compress one hit. Soft-fails on every error path; never bubbles up.
///
/// Algorithm (spec §5):
/// 1. Short-circuit if content < `MIN_CONTENT_CHARS` → `Skipped(TOO_SHORT)`.
/// 2. Compute cache key. If hit, load and apply. Return `CacheHit`.
/// 3. Call Ollama wrapped in `tokio::time::timeout(ctx.timeout)`.
/// 4. Validate: non-empty after trim, strictly shorter than original.
/// 5. On success: write cache, mutate `hit.snippet`, tag `compressed =
///    Some(Abstractive)`, return `Compressed`.
pub async fn compress_hit(
    ctx: &AbstractiveCtx<'_>,
    hit: &mut ResolvedHit,
    target_tokens: usize,
) -> CompressOutcome {
    if hit.snippet.len() < MIN_CONTENT_CHARS {
        return CompressOutcome::Skipped(skip_reason::TOO_SHORT);
    }
    let target = target_tokens.max(MIN_TARGET_TOKENS_PER_HIT);
    let key = cache::cache_key(ctx.model, target, &hit.snippet);

    if let Some(cached) = cache::cache_get(&key, ctx.root_override) {
        if !cached.is_empty() && cached.len() < hit.snippet.len() {
            hit.snippet = cached;
            hit.compressed = Some(super::Compression::Abstractive);
            return CompressOutcome::CacheHit;
        }
        // Cached value invalid (empty, or unexpectedly not-shorter because
        // hit content drifted) — fall through and try a fresh call.
        tracing::debug!(
            key,
            cached_len = cached.len(),
            orig_len = hit.snippet.len(),
            "cache entry present but invalid, retrying"
        );
    }

    let prompt = user_template(target, &hit.snippet);
    let req = GenerateRequest {
        model: ctx.model,
        prompt: &prompt,
        system: Some(SYSTEM_TEMPLATE),
        stream: false,
        options: GenerateOptions {
            temperature: Some(0.0),
            top_p: None,
            num_predict: Some(target as u32 * 2),
            stop: Vec::new(),
        },
    };

    let call = ctx.client.generate(req);
    let out = match tokio::time::timeout(ctx.timeout, call).await {
        Err(_) => {
            tracing::warn!(target, len = hit.snippet.len(), "stage-1b timeout");
            return CompressOutcome::Skipped(skip_reason::TIMEOUT);
        }
        Ok(Err(e)) => {
            tracing::warn!(target, err = ?e, "stage-1b ollama error");
            return CompressOutcome::Skipped(skip_reason::OLLAMA_ERR);
        }
        Ok(Ok(resp)) => resp.response,
    };

    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        return CompressOutcome::Skipped(skip_reason::EMPTY);
    }
    if trimmed.len() >= hit.snippet.len() {
        return CompressOutcome::Skipped(skip_reason::NOT_SHORTER);
    }

    if let Err(e) = cache::cache_put(&key, &trimmed, ctx.root_override) {
        // Cache write failure is non-fatal — still apply the summary.
        tracing::warn!(key, err = ?e, "stage-1b cache write failed");
    }

    hit.snippet = trimmed;
    hit.compressed = Some(super::Compression::Abstractive);
    CompressOutcome::Compressed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::ask::HitInfo;
    use crate::conversations::ENV_LOCK;
    use std::time::Duration;

    fn long_hit(n_sentences: usize) -> ResolvedHit {
        let snippet = (0..n_sentences)
            .map(|i| format!("Hit body fact {i} with some supporting narrative text."))
            .collect::<Vec<_>>()
            .join(" ");
        ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "c1".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                score: 0.9,
            },
            snippet,
            line_hint: Some(1),
            span_index_in_summary: None,
            vector: None,
            compressed: None,
        }
    }

    fn ctx<'a>(client: &'a OllamaClient, root: &'a str) -> AbstractiveCtx<'a> {
        AbstractiveCtx {
            client,
            model: "qwen3:14b",
            timeout: Duration::from_millis(200),
            root_override: Some(root),
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_skips_when_content_too_short() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(1);
        h.snippet = "tiny".into();
        let o = compress_hit(&ctx(&client, tmp.path().to_str().unwrap()), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::TOO_SHORT));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_success_shortens_snippet_and_writes_cache() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut h = long_hit(20); // ~1100 chars, way over MIN_CONTENT_CHARS
        let orig_len = h.snippet.len();
        let o = compress_hit(&ctx(&client, root), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Compressed);
        assert!(h.snippet.len() < orig_len, "mock summary must be shorter");
        assert_eq!(h.compressed, Some(super::super::Compression::Abstractive));
        // Cache entry should exist.
        let key = cache::cache_key("qwen3:14b", 128, &long_hit(20).snippet);
        let cached = cache::cache_get(&key, Some(root));
        assert_eq!(cached.as_deref(), Some(h.snippet.as_str()));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_cache_hit_on_second_call_skips_llm() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut h1 = long_hit(20);
        compress_hit(&ctx(&client, root), &mut h1, 128).await;
        let mut h2 = long_hit(20);
        let o = compress_hit(&ctx(&client, root), &mut h2, 128).await;
        assert_eq!(o, CompressOutcome::CacheHit);
        assert_eq!(h2.snippet, h1.snippet);
        assert_eq!(h2.compressed, Some(super::super::Compression::Abstractive));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_respects_timeout() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "timeout") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(30));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(20);
        let o = compress_hit(
            &AbstractiveCtx {
                client: &client,
                model: "qwen3:14b",
                timeout: Duration::from_millis(100),
                root_override: Some(tmp.path().to_str().unwrap()),
            },
            &mut h,
            128,
        )
        .await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::TIMEOUT));
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_skips_on_empty_response() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "empty") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(20);
        let o = compress_hit(&ctx(&client, tmp.path().to_str().unwrap()), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::EMPTY));
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_skips_when_not_shorter() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "not_shorter") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(20);
        let o = compress_hit(&ctx(&client, tmp.path().to_str().unwrap()), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::NOT_SHORTER));
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
```

- [ ] **Step 3: Run tests — they should FAIL on the timeout / empty / not_shorter cases**

Run: `cargo test -p mur-core conversations::ask::abstractive`
Expected: 3 success tests PASS (success, cache-hit, too-short); 3 FAIL (timeout/empty/not_shorter) because the mock doesn't yet honor `MUR_ABSTRACTIVE_MOCK_FAIL`. That's the lead-in for Task 7.

**Do not skip ahead.** Commit now so Task 7's mock landing makes the red tests green:

- [ ] **Step 4: Mark the three currently-red tests `#[ignore]` until Task 7 lands the mock**

Edit `mur-core/src/conversations/ask/abstractive.rs` and add `#[ignore = "mock FAIL hook lands in Task 7"]` immediately above each of:
- `fn compress_hit_respects_timeout`
- `fn compress_hit_skips_on_empty_response`
- `fn compress_hit_skips_when_not_shorter`

Re-run `cargo test -p mur-core conversations::ask::abstractive` and expect all non-ignored tests to pass, 3 ignored.

- [ ] **Step 5: `cargo fmt` + `cargo clippy` clean**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: zero diffs, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/conversations/ask/abstractive.rs mur-core/src/conversations/ask/mod.rs
git commit -m "feat(conversations): Phase 3.5 Task 5 — abstractive::compress_hit (per-hit LLM summarize)"
```

---

## Task 6: `abstractive.rs` — `run_stage_1b` orchestrator

**Files:**
- Modify: `mur-core/src/conversations/ask/abstractive.rs` (append orchestrator + tests)

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `mur-core/src/conversations/ask/abstractive.rs`:

```rust
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn run_stage_1b_early_exits_when_fit_after_two_hits() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
    let client = OllamaClient::new("http://unused", Duration::from_secs(1));
    let tmp = tempfile::tempdir().unwrap();
    // 5 hits, all eligible, of varying length. Budget is tight but the
    // first two compressions will likely fit it.
    let mut hits: Vec<ResolvedHit> = (0..5).map(|_| long_hit(20)).collect();
    let orig_total: usize = hits.iter().map(|h| h.snippet.len()).sum();
    let max_context_chars = orig_total - 200; // force overflow on char-ish metric
    let summary = run_stage_1b(
        &ctx(&client, tmp.path().to_str().unwrap()),
        &mut hits,
        orig_total,
        max_context_chars,
    )
    .await;
    assert!(
        summary.processed >= 1,
        "at least one hit must be touched; got {}",
        summary.processed
    );
    assert!(
        summary.processed < 5,
        "early-exit should prevent touching all 5; got {}",
        summary.processed
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn run_stage_1b_largest_first_order() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
    let client = OllamaClient::new("http://unused", Duration::from_secs(1));
    let tmp = tempfile::tempdir().unwrap();
    let mut hits = vec![long_hit(5), long_hit(30), long_hit(10)];
    let big_idx_before = 1; // middle hit is the largest
    let orig_big_len = hits[big_idx_before].snippet.len();
    let orig_total: usize = hits.iter().map(|h| h.snippet.len()).sum();
    // Budget: just enough that only ONE compression is needed.
    let max_context_chars = orig_total - (orig_big_len / 3);
    let summary = run_stage_1b(
        &ctx(&client, tmp.path().to_str().unwrap()),
        &mut hits,
        orig_total,
        max_context_chars,
    )
    .await;
    assert!(summary.compressed_count >= 1);
    // The largest hit should have shrunk.
    assert!(
        hits[big_idx_before].snippet.len() < orig_big_len,
        "largest-first: biggest hit should be touched first"
    );
    assert_eq!(hits[big_idx_before].compressed, Some(super::super::Compression::Abstractive));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn run_stage_1b_noop_when_already_fits() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    let client = OllamaClient::new("http://unused", Duration::from_secs(1));
    let tmp = tempfile::tempdir().unwrap();
    let mut hits = vec![long_hit(5)];
    let orig_total: usize = hits.iter().map(|h| h.snippet.len()).sum();
    let summary = run_stage_1b(
        &ctx(&client, tmp.path().to_str().unwrap()),
        &mut hits,
        orig_total,
        orig_total + 10_000, // huge budget → no overshoot
    )
    .await;
    assert_eq!(summary.processed, 0);
    assert_eq!(summary.compressed_count, 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core conversations::ask::abstractive::tests::run_stage_1b`
Expected: FAIL — `run_stage_1b` not defined.

- [ ] **Step 3: Implement `run_stage_1b`**

In `mur-core/src/conversations/ask/abstractive.rs`, append (before the `#[cfg(test)]` block):

```rust
/// Orchestrate Stage 1b: sort hits largest-first, sequentially compress
/// until the token overshoot is resolved or candidates are exhausted.
///
/// `cur_tokens` and `max_context_tokens` are caller-measured in _tokens_ (not
/// chars) but the per-hit char → token conversion uses the same `len / 4`
/// heuristic `prompt::tokens_est` uses, so they're proportional. Re-measuring
/// between hits happens by the caller after this function returns — this fn
/// operates on pre-computed overshoot to avoid owning the `prompt::render`
/// responsibility of re-building the full prompt string.
///
/// Invariants:
/// - Sorted by `hit.snippet.len()` descending at entry — stable sort by
///   original index on ties so deterministic.
/// - Early-exits when estimated post-compression tokens ≤ max.
/// - `target_tokens_per_hit` floored at `MIN_TARGET_TOKENS_PER_HIT`.
pub async fn run_stage_1b(
    ctx: &AbstractiveCtx<'_>,
    hits: &mut [ResolvedHit],
    cur_tokens: usize,
    max_context_tokens: usize,
) -> Stage1bSummary {
    let start = std::time::Instant::now();
    let mut summary = Stage1bSummary {
        processed: 0,
        compressed_count: 0,
        cache_hits: 0,
        skipped: Vec::new(),
        duration_ms: 0,
    };
    if cur_tokens <= max_context_tokens {
        summary.duration_ms = start.elapsed().as_millis() as u64;
        return summary;
    }

    // Index list sorted largest-first (by snippet byte length). Keep indices so
    // we can mutate the original slice in place via &mut hits[idx].
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| hits[b].snippet.len().cmp(&hits[a].snippet.len()).then(a.cmp(&b)));

    let mut remaining: isize = order.len() as isize;
    let mut cur_tokens = cur_tokens;

    for idx in order {
        if cur_tokens <= max_context_tokens {
            break;
        }
        let overshoot = cur_tokens.saturating_sub(max_context_tokens);
        let rem_denom = remaining.max(1) as usize;
        // ceil-div for "share out" the overshoot reduction.
        let reduce_by = overshoot.div_ceil(rem_denom);
        let cur_hit_tokens = hits[idx].snippet.len() / 4;
        let target = cur_hit_tokens
            .saturating_sub(reduce_by)
            .max(MIN_TARGET_TOKENS_PER_HIT);

        let before_tokens = cur_hit_tokens;
        let outcome = compress_hit(ctx, &mut hits[idx], target).await;
        summary.processed += 1;
        match outcome {
            CompressOutcome::Compressed => summary.compressed_count += 1,
            CompressOutcome::CacheHit => summary.cache_hits += 1,
            CompressOutcome::Skipped(reason) => summary.skipped.push((idx, reason)),
        }
        let after_tokens = hits[idx].snippet.len() / 4;
        let delta = before_tokens.saturating_sub(after_tokens);
        cur_tokens = cur_tokens.saturating_sub(delta);
        remaining -= 1;
    }

    summary.duration_ms = start.elapsed().as_millis() as u64;
    summary
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core conversations::ask::abstractive::tests`
Expected: All non-ignored tests PASS (including the three new `run_stage_1b` tests).

- [ ] **Step 5: `cargo fmt` + clippy**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/conversations/ask/abstractive.rs
git commit -m "feat(conversations): Phase 3.5 Task 6 — run_stage_1b orchestrator (largest-first + early-exit)"
```

---

## Task 7: Ollama mock — abstractive branch + `MUR_ABSTRACTIVE_MOCK_FAIL` hook

**Files:**
- Modify: `mur-core/src/conversations/ollama.rs:128-146` (async `generate` — add failure-mode sleep)
- Modify: `mur-core/src/conversations/ollama.rs:248-281` (`mock_generate` — add abstractive branch)
- Modify: `mur-core/src/conversations/ask/abstractive.rs` — unignore the three tests from Task 5

- [ ] **Step 1: Extend `mock_generate` with the abstractive-prompt branch**

In `mur-core/src/conversations/ollama.rs`, inside `fn mock_generate` (around line 248), restructure the if/else chain so the abstractive branch runs FIRST (before other branches). Replace the existing body of `mock_generate` with:

```rust
fn mock_generate(req: &GenerateRequest<'_>) -> GenerateResponse {
    let is_abstractive = req
        .system
        .map(|s| s.contains("You compress text for retrieval context"))
        .unwrap_or(false);

    let response = if is_abstractive {
        // Phase 3.5 abstractive mock. Honor MUR_ABSTRACTIVE_MOCK_FAIL for
        // soft-fail tests. Default path: echo a short deterministic summary
        // that is strictly shorter than the input content, so the validator
        // in abstractive::compress_hit accepts it.
        match std::env::var("MUR_ABSTRACTIVE_MOCK_FAIL").as_deref() {
            Ok("empty") => String::new(),
            Ok("not_shorter") => req.prompt.to_string() + " [MOCK PADDING MAKES THIS LONGER]",
            // `timeout` is handled upstream in `OllamaClient::generate` via
            // an actual `tokio::time::sleep`; if we reach here the caller's
            // timeout wasn't lower than the sleep, so produce a normal summary.
            _ => {
                // The prompt body starts after "\n\n". Take first 40 chars of
                // that, then " [mock summary]". Deterministic, strictly
                // shorter than the input for any content ≥ ~56 chars.
                let body = req.prompt.splitn(2, "\n\n").nth(1).unwrap_or(req.prompt);
                let first_40: String = body.chars().take(40).collect();
                format!("{first_40} [mock summary]")
            }
        }
    } else if req.prompt.contains("Extract the 1-3 most informative spans") {
        r#"[{"role":"user","conv_id":"mock","line_hint":1,"text":"mock extractive span"}]"#
            .to_string()
    } else if req.prompt.contains("narrative paragraph") {
        if req.prompt.contains("one week") || req.prompt.contains("one-week") {
            "Mock narrative: this week the developer shipped several fixes and refactors."
                .to_string()
        } else if req.prompt.contains("one month") || req.prompt.contains("one-month") {
            "Mock narrative: this month saw major work on the conversations archive.".to_string()
        } else {
            "Mock narrative: today the developer explored mock compression.".to_string()
        }
    } else if req.prompt.contains("Standalone question:") {
        extract_latest_question_from_condense_prompt(req.prompt)
    } else if req.prompt.contains("[cit:") {
        "Mock answer about the archive [cit: 2026-04-19 claude-code/mock:L1].".to_string()
    } else {
        format!("mock response for model={}", req.model)
    };
    GenerateResponse {
        response,
        done: true,
        model: req.model.to_string(),
        prompt_eval_count: 10,
        eval_count: 20,
    }
}
```

- [ ] **Step 2: Add timeout simulation in `generate`**

In `mur-core/src/conversations/ollama.rs`, modify `impl OllamaClient { pub async fn generate(...) }` (around line 128). Replace the mock short-circuit block:

Old:
```rust
    pub async fn generate(&self, req: GenerateRequest<'_>) -> Result<GenerateResponse> {
        if Self::mock_from_env() {
            return Ok(mock_generate(&req));
        }
        let url = format!("{}/api/generate", self.endpoint.trim_end_matches('/'));
```

New:
```rust
    pub async fn generate(&self, req: GenerateRequest<'_>) -> Result<GenerateResponse> {
        if Self::mock_from_env() {
            // Phase 3.5: simulate a slow LLM when the caller opts in via
            // MUR_ABSTRACTIVE_MOCK_FAIL=timeout AND the request looks
            // abstractive. Lets `tokio::time::timeout` fire in tests without
            // a real server.
            let is_abstractive = req
                .system
                .map(|s| s.contains("You compress text for retrieval context"))
                .unwrap_or(false);
            if is_abstractive
                && std::env::var("MUR_ABSTRACTIVE_MOCK_FAIL").as_deref() == Ok("timeout")
            {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            return Ok(mock_generate(&req));
        }
        let url = format!("{}/api/generate", self.endpoint.trim_end_matches('/'));
```

- [ ] **Step 3: Un-ignore the three Task-5 tests**

In `mur-core/src/conversations/ask/abstractive.rs`, remove the three `#[ignore = "mock FAIL hook lands in Task 7"]` lines you added in Task 5 Step 4.

- [ ] **Step 4: Add mock-branch sanity tests to ollama.rs**

Append to the existing `#[cfg(test)] mod tests` in `mur-core/src/conversations/ollama.rs`:

```rust
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn mock_abstractive_branch_returns_shorter_summary() {
    let _env_guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
    let client = OllamaClient::new("http://unused", Duration::from_secs(1));
    let body: String = "fact ".repeat(30);
    let prompt = format!("Summarize the following in ≤64 tokens.\n\n{body}");
    let req = GenerateRequest {
        model: "m",
        prompt: &prompt,
        system: Some(
            "You compress text for retrieval context. Preserve entities, dates, numbers.",
        ),
        stream: false,
        options: GenerateOptions::default(),
    };
    let resp = client.generate(req).await.unwrap();
    assert!(resp.response.contains("[mock summary]"));
    assert!(resp.response.len() < body.len());
    unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn mock_abstractive_fail_empty_returns_empty() {
    let _env_guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "empty") };
    let client = OllamaClient::new("http://unused", Duration::from_secs(1));
    let req = GenerateRequest {
        model: "m",
        prompt: "Summarize the following in ≤64 tokens.\n\nlong body here",
        system: Some("You compress text for retrieval context."),
        stream: false,
        options: GenerateOptions::default(),
    };
    let resp = client.generate(req).await.unwrap();
    assert_eq!(resp.response, "");
    unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
    unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
}
```

- [ ] **Step 5: Run the full ask + ollama test suite**

Run: `cargo test -p mur-core conversations::ask::abstractive conversations::ollama`
Expected: PASS on everything (including the previously-ignored timeout/empty/not_shorter).

- [ ] **Step 6: `cargo fmt` + clippy**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/conversations/ollama.rs mur-core/src/conversations/ask/abstractive.rs
git commit -m "feat(conversations): Phase 3.5 Task 7 — Ollama mock abstractive branch + MUR_ABSTRACTIVE_MOCK_FAIL"
```

---

## Task 8: Wire Stage 1b into `prompt::render` (make it async)

**Files:**
- Modify: `mur-core/src/conversations/ask/prompt.rs:15-125` (struct + render fn)
- Modify: `mur-core/src/conversations/ask/prompt.rs:196-462` (tests — `.await` + destructure new fields)
- Modify: `mur-core/src/conversations/ask/mod.rs` (only one call site — already inside async `ask_stream`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `mur-core/src/conversations/ask/prompt.rs`:

```rust
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn render_fires_stage_1b_when_compression_alone_insufficient() {
    use crate::conversations::ask::abstractive::AbstractiveCtx;
    use crate::conversations::ollama::OllamaClient;
    use crate::conversations::ENV_LOCK;
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
    let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
    let tmp = tempfile::tempdir().unwrap();
    let ctx = AbstractiveCtx {
        client: &client,
        model: "qwen3:14b",
        timeout: std::time::Duration::from_secs(1),
        root_override: Some(tmp.path().to_str().unwrap()),
    };
    // 3 long hits + tight budget so Stage 1 heuristic compression alone
    // still overruns, forcing Stage 1b.
    let big = (0..50)
        .map(|i| format!("Sentence number {i} with plenty of supporting body."))
        .collect::<Vec<_>>()
        .join(" ");
    let hits = vec![hit_raw("a", &big), hit_raw("b", &big), hit_raw("c", &big)];
    let r = super::render(
        "q?",
        &[],
        hits,
        500,
        100,
        true,
        true,
        Some(&ctx),
    )
    .await;
    assert!(r.stage_1b.is_some(), "Stage 1b summary must surface when fired");
    let s = r.stage_1b.unwrap();
    assert!(
        s.compressed_count + s.cache_hits > 0,
        "Stage 1b should have touched at least one hit; got {s:?}"
    );
    // At least one returned hit should now be Abstractive-tagged.
    assert!(
        r.final_hits.iter().any(|h| h.compressed == Some(super::super::Compression::Abstractive)),
        "expected at least one hit to be tagged Abstractive"
    );
    unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn render_stage_1b_none_when_disabled() {
    use crate::conversations::ENV_LOCK;
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
    let big = (0..50)
        .map(|i| format!("Sentence number {i} with plenty of supporting body."))
        .collect::<Vec<_>>()
        .join(" ");
    let hits = vec![hit_raw("a", &big)];
    let r = super::render(
        "q?",
        &[],
        hits,
        500,
        100,
        true,
        /* summarize_enabled */ false,
        None,
    )
    .await;
    assert!(r.stage_1b.is_none());
    unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
}
```

- [ ] **Step 2: Change the `render` signature to async + add Stage 1b**

In `mur-core/src/conversations/ask/prompt.rs`, extend `RenderedPrompt`:

```rust
pub struct RenderedPrompt {
    pub system: String,
    pub user: String,
    pub tokens_est: usize,
    pub valid_citations: Vec<String>,
    /// Post-cascade hits (after any compression / no-op). Caller uses this
    /// to build `citations_map` so the `compressed` provenance flag flows
    /// through to `Citation.compressed`. Size equals input `hits.len()`.
    pub final_hits: Vec<super::retrieve::ResolvedHit>,
    /// Present only if Stage 1b fired.
    pub stage_1b: Option<super::abstractive::Stage1bSummary>,
}
```

Rewrite the `pub fn render(...)` signature to async, taking an owned `Vec<ResolvedHit>` and the new Stage 1b params:

```rust
pub async fn render(
    question: &str,
    prior_turns: &[super::session::TurnRecord],
    hits: Vec<super::retrieve::ResolvedHit>,
    max_context_tokens: usize,
    response_tokens: usize,
    compress_enabled: bool,
    summarize_enabled: bool,
    abstractive_ctx: Option<&super::abstractive::AbstractiveCtx<'_>>,
) -> RenderedPrompt {
    let system = SYSTEM_PROMPT.to_string();
    let truncated_question = truncate_chars(question, 2000);

    let mut history_cursor = 0usize;
    let mut trimmed_hits = hits.len();
    let mut active_hits: Vec<ResolvedHit> = hits;

    let (mut user, mut valid_citations) = render_ctx_and_user(
        &active_hits,
        prior_turns,
        history_cursor,
        trimmed_hits,
        &truncated_question,
    );
    let mut cur_tokens = tokens_est(&system, &user, response_tokens);

    // Stage 1 — Phase 3.4 heuristic compression (unchanged; see prior comment).
    if cur_tokens > max_context_tokens && compress_enabled {
        let overage_chars = cur_tokens
            .saturating_sub(max_context_tokens)
            .saturating_mul(4);
        let total_chars: usize = active_hits.iter().map(|h| h.snippet.len()).sum();
        let ratio = 1.0 - (overage_chars as f64 / total_chars.max(1) as f64).min(0.6);
        let avg = total_chars / active_hits.len().max(1);
        let target = (avg as f64 * ratio) as usize;
        active_hits = super::compress::compress_hits(active_hits, question, target);
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Stage 1b — Phase 3.5 LLM-abstractive compression. Fires only when
    // still over budget AND caller enabled it AND ctx is provided.
    let mut stage_1b: Option<super::abstractive::Stage1bSummary> = None;
    if cur_tokens > max_context_tokens && summarize_enabled && let Some(ctx) = abstractive_ctx {
        let summary = super::abstractive::run_stage_1b(
            ctx,
            &mut active_hits,
            cur_tokens,
            max_context_tokens,
        )
        .await;
        // Emit per-hit skipped logs at call site (callsite has richer context
        // than the async boundary would).
        for (idx, reason) in &summary.skipped {
            tracing::warn!(hit_idx = idx, reason, "stage-1b skipped");
        }
        stage_1b = Some(summary);
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Stage 2 — drop oldest history turns (existing Phase 3.3 behavior).
    while cur_tokens > max_context_tokens && history_cursor < prior_turns.len() {
        history_cursor += 1;
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Stage 3 — shrink hits from the tail.
    while cur_tokens > max_context_tokens && trimmed_hits > 1 {
        trimmed_hits -= 1;
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Drop hits beyond the `trimmed_hits` cutoff so `final_hits` mirrors what
    // the prompt actually references. Callers building `citations_map` from
    // `final_hits` then only see citations that can be anchored.
    active_hits.truncate(trimmed_hits);

    RenderedPrompt {
        system,
        user,
        tokens_est: cur_tokens,
        valid_citations,
        final_hits: active_hits,
        stage_1b,
    }
}
```

- [ ] **Step 3: Update every existing test in `prompt.rs` to the new signature**

Each existing test that calls `render(...)` must:
- Become `#[tokio::test]` (wrap with `async fn`).
- Pass `hits` by-value (it already does via `hits.to_vec()` or direct literal — check each).
- Add the two new args: `summarize_enabled: false`, `abstractive_ctx: None`.
- `.await` the result.

For each of the following tests in `prompt.rs`, update their signature and body:

- `render_shrinks_hits_on_overflow` (line ~234)
- `render_lists_valid_citations_in_order` (line ~244)
- `render_includes_chat_history_section_when_prior_turns_non_empty` (line ~311)
- `render_omits_chat_history_section_when_prior_turns_empty` (line ~327)
- `render_drops_oldest_history_first_on_budget_overflow` (line ~334)
- `render_falls_through_to_hit_shrinking_when_history_exhausted` (line ~364)
- `render_compresses_hits_on_overflow_when_enabled` (line ~391)
- `render_does_not_compress_when_disabled` (line ~432)

Example — replace:
```rust
#[test]
fn render_shrinks_hits_on_overflow() {
    let hits = (0..20)
        .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(3000)))
        .collect::<Vec<_>>();
    let r = render("question?", &[], &hits, 6000, 1024, true);
    assert!(r.valid_citations.len() < hits.len());
    assert!(!r.valid_citations.is_empty());
}
```

with:

```rust
#[tokio::test]
async fn render_shrinks_hits_on_overflow() {
    let hits: Vec<_> = (0..20)
        .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(3000)))
        .collect();
    let n = hits.len();
    let r = render("question?", &[], hits, 6000, 1024, true, false, None).await;
    assert!(r.valid_citations.len() < n);
    assert!(!r.valid_citations.is_empty());
}
```

Apply the equivalent mechanical update to the other seven tests (each now owns its Vec instead of referencing `hits.len()` afterwards — capture `n = hits.len()` before moving, as shown).

- [ ] **Step 4: Update the caller in `ask_stream`**

In `mur-core/src/conversations/ask/mod.rs`, modify `ask_stream` (around line 173). Build the AbstractiveCtx and pass it + the new hits-by-value + flag through:

Replace:
```rust
    // 3. Build prompt
    let prompt = prompt::render(
        &req.question,
        &req.prior_turns,
        &hits,
        req.max_context_tokens,
        req.response_tokens,
        req.compress_enabled,
    );

    let hit_events: Vec<AskEvent> = hits
        .iter()
        .map(|h| AskEvent::HitInfo(h.info.clone()))
        .collect();
```

With:
```rust
    // 3. Build prompt (incl. Phase 3.5 Stage 1b when enabled).
    let ollama_client = crate::conversations::ollama::OllamaClient::new(
        &req.endpoint,
        req.timeout,
    );
    let summarize_model_owned: Option<String> = req
        .summarize_model
        .clone()
        .or_else(|| Some(req.model.clone()));
    let abstractive_ctx_owned = summarize_model_owned.as_ref().map(|m| {
        abstractive::AbstractiveCtx {
            client: &ollama_client,
            model: m.as_str(),
            timeout: abstractive::CALL_TIMEOUT,
            root_override,
        }
    });

    // Emit HitInfo events off the ORIGINAL hits (not final_hits) so the
    // downstream session record still reflects retrieval state, not
    // compression state.
    let hit_events: Vec<AskEvent> = hits
        .iter()
        .map(|h| AskEvent::HitInfo(h.info.clone()))
        .collect();

    let prompt = prompt::render(
        &req.question,
        &req.prior_turns,
        hits,
        req.max_context_tokens,
        req.response_tokens,
        req.compress_enabled,
        req.summarize_enabled,
        abstractive_ctx_owned.as_ref(),
    )
    .await;

    let stage_1b_stats = prompt.stage_1b.as_ref().map(|s| s.to_stats());
```

Then update `citations_map(&hits)` to use `prompt.final_hits` instead:

Replace:
```rust
    let citation_events_by_anchor = citations_map(&hits);
```

With:
```rust
    let citation_events_by_anchor = citations_map(&prompt.final_hits);
```

(`hits` is moved into `render` — you can only reference `prompt.final_hits` after that point.)

Also replace the `hits_as_mode_b(&hits)` call on the ollama-unavailable branch (line ~209) — at that point `hits` has been moved. Use `prompt.final_hits`:

Replace:
```rust
        Err(e) => {
            let mode_b = hits_as_mode_b(&hits);
```

With:
```rust
        Err(e) => {
            let mode_b = hits_as_mode_b(&prompt.final_hits);
```

- [ ] **Step 5: Thread `stage_1b_stats` to `AskEvent::Done`**

This happens in Task 9 alongside the `AskResponse.stage_1b` field. For now, silence the `stage_1b_stats` unused warning by binding it to `_`:

```rust
    let _stage_1b_stats = prompt.stage_1b.as_ref().map(|s| s.to_stats());
```

Remove this `_` binding in Task 9 and actually use the value.

Also: `AskRequest` doesn't yet have `summarize_enabled` / `summarize_model` (Task 9 adds them). For this task, add default values to unblock compilation:

In `AskRequest` (struct at line 31), add **temporarily** just below `compress_enabled`:

```rust
    pub summarize_enabled: bool,
    pub summarize_model: Option<String>,
```

In the test at `ask_end_to_end_mock_empty_hits` (line ~388), add after `compress_enabled: true`:

```rust
            summarize_enabled: true,
            summarize_model: None,
```

In `cmd/conversations_cmd.rs` at `cmd_ask` (line ~1156), add after `compress_enabled: ask_cfg.compress_hits_enabled`:

```rust
            summarize_enabled: ask_cfg.summarize_hits_enabled,
            summarize_model: ask_cfg.summarize_model.clone(),
```

- [ ] **Step 6: Run the full test suite**

Run: `cargo test -p mur-core && cargo test -p mur-common`
Expected: PASS. The two new render tests pass; all existing prompt.rs tests pass (with their `.await` + new args); no regressions.

- [ ] **Step 7: `cargo fmt` + clippy**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/conversations/ask/prompt.rs mur-core/src/conversations/ask/mod.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "feat(conversations): Phase 3.5 Task 8 — wire Stage 1b into prompt::render (async + AbstractiveCtx)"
```

---

## Task 9: `AskRequest` + `AskEvent::Done` + `AskResponse.stage_1b`

**Files:**
- Modify: `mur-core/src/conversations/ask/mod.rs` (AskRequest already has the two fields from Task 8; AskResponse, AskEvent::Done, ask_stream `Done` yield, ask() collection)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (cmd_ask — consume `stage_1b`; already wires the request fields)

- [ ] **Step 1: Write the failing tests**

Append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/ask/mod.rs`:

```rust
#[test]
fn ask_response_omits_stage_1b_when_none() {
    let r = AskResponse {
        answer: "".into(),
        citations: vec![],
        hits_used: vec![],
        degraded_to_mode_b: false,
        tokens_in: 0,
        tokens_out: 0,
        duration_ms: 0,
        rewritten_question: None,
        rewriter_status: session::RewriterStatus::Skipped,
        stage_1b: None,
    };
    let j = serde_json::to_string(&r).unwrap();
    assert!(!j.contains("stage_1b"), "expected field omitted, got: {j}");
}

#[test]
fn ask_response_emits_stage_1b_when_set() {
    let r = AskResponse {
        answer: "".into(),
        citations: vec![],
        hits_used: vec![],
        degraded_to_mode_b: false,
        tokens_in: 0,
        tokens_out: 0,
        duration_ms: 0,
        rewritten_question: None,
        rewriter_status: session::RewriterStatus::Skipped,
        stage_1b: Some(abstractive::Stage1bStats {
            compressed_count: 2,
            cache_hits: 1,
            skipped_count: 0,
            duration_ms: 120,
        }),
    };
    let j = serde_json::to_string(&r).unwrap();
    assert!(j.contains("\"stage_1b\""));
    assert!(j.contains("\"compressed_count\":2"));
    assert!(j.contains("\"cache_hits\":1"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core conversations::ask::tests::ask_response_omits_stage_1b_when_none`
Expected: FAIL — field `stage_1b` not on `AskResponse`.

- [ ] **Step 3: Add `stage_1b` to `AskResponse`**

In `mur-core/src/conversations/ask/mod.rs`, extend `AskResponse` (currently around line 76):

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct AskResponse {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub hits_used: Vec<HitInfo>,
    pub degraded_to_mode_b: bool,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
    pub rewritten_question: Option<String>,
    pub rewriter_status: session::RewriterStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_1b: Option<abstractive::Stage1bStats>,
}
```

- [ ] **Step 4: Extend `AskEvent::Done` with `stage_1b`**

Replace:
```rust
    Done {
        tokens_in: usize,
        tokens_out: usize,
        degraded: bool,
        duration_ms: u64,
    },
```

With:
```rust
    Done {
        tokens_in: usize,
        tokens_out: usize,
        degraded: bool,
        duration_ms: u64,
        stage_1b: Option<abstractive::Stage1bStats>,
    },
```

- [ ] **Step 5: Update every `AskEvent::Done` yield site**

In `mur-core/src/conversations/ask/mod.rs` — there are 4 yield sites in `ask_stream`:

1. Embed-failure branch (around line 121) — add `stage_1b: None,`.
2. Retrieve-failure branch (around line 150) — add `stage_1b: None,`.
3. Empty-hits branch (around line 163) — add `stage_1b: None,`.
4. Ollama-unavailable branch (around line 214) — add `stage_1b: stage_1b_stats_for_closure.clone(),`. You need to clone the stats into a variable the `try_stream!` macro captures.
5. Normal finishing `Done` at bottom (around line 258) — yield `stage_1b: stage_1b_stats.clone()`.

Concrete change for the ollama-unavailable branch — hoist the stats clone BEFORE the `try_stream!`:

```rust
    let stage_1b_stats = prompt.stage_1b.as_ref().map(|s| s.to_stats());
    // ... existing generate::stream_answer match ...
    let stream = match generate::stream_answer(...).await {
        Ok(s) => s,
        Err(e) => {
            let mode_b = hits_as_mode_b(&prompt.final_hits);
            let stage_1b_err = stage_1b_stats.clone();
            return Ok(Box::pin(try_stream! {
                for evt in hit_events { yield evt; }
                yield AskEvent::Token(mode_b);
                yield AskEvent::Done {
                    tokens_in,
                    tokens_out: 0,
                    degraded: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    stage_1b: stage_1b_err.clone(),
                };
                yield AskEvent::Error(format!("ollama unavailable: {e:#}"));
            }));
        }
    };
```

And the normal finishing branch near the end of `ask_stream`:

```rust
        yield AskEvent::Done {
            tokens_in,
            tokens_out,
            degraded: false,
            duration_ms: start.elapsed().as_millis() as u64,
            stage_1b: stage_1b_stats.clone(),
        };
```

(Note: the `_stage_1b_stats` `_` binding introduced in Task 8 Step 5 is now replaced by the real `stage_1b_stats` variable; remove the underscore.)

- [ ] **Step 6: Update `ask()` to collect `stage_1b`**

In the same file, update `ask()` (around line 267). Replace:

```rust
            AskEvent::Done {
                tokens_in: ti,
                tokens_out: to,
                degraded: d,
                duration_ms: ms,
            } => {
                tokens_in = ti;
                tokens_out = to;
                degraded = d;
                duration_ms = ms;
            }
```

With:
```rust
            AskEvent::Done {
                tokens_in: ti,
                tokens_out: to,
                degraded: d,
                duration_ms: ms,
                stage_1b: sb,
            } => {
                tokens_in = ti;
                tokens_out = to;
                degraded = d;
                duration_ms = ms;
                stage_1b_final = sb;
            }
```

Add a local binding before the `while let` loop:
```rust
    let mut stage_1b_final: Option<abstractive::Stage1bStats> = None;
```

And include `stage_1b` when building the final `AskResponse`:
```rust
    Ok(AskResponse {
        answer,
        citations,
        hits_used,
        degraded_to_mode_b: degraded,
        tokens_in,
        tokens_out,
        duration_ms,
        rewritten_question: match rewriter_status {
            session::RewriterStatus::Skipped => None,
            _ => Some(retrieval_query),
        },
        rewriter_status,
        stage_1b: stage_1b_final,
    })
```

- [ ] **Step 7: Update `cmd_ask` streaming loop**

In `mur-core/src/cmd/conversations_cmd.rs`, around line 1212 (`AskEvent::Done { ... } => {}`), extend the match arm to accept and carry `stage_1b`:

```rust
                ask::AskEvent::Done {
                    tokens_in: ti,
                    tokens_out: to,
                    degraded: d,
                    duration_ms,
                    stage_1b: sb,
                } => {
                    tokens_in = ti;
                    tokens_out = to;
                    degraded = d;
                    duration = duration_ms;
                    stage_1b_done = sb;
                }
```

Add a local binding outside the loop:
```rust
        let mut stage_1b_done: Option<crate::conversations::ask::abstractive::Stage1bStats> = None;
```

Then include `stage_1b: stage_1b_done.clone()` when building both the mid-loop `AskResponse` (around line 1232) and the final one at line 1247:

```rust
        ask::AskResponse {
            answer: answer.clone(),
            citations: citations.clone(),
            hits_used: hits_used.clone(),
            degraded_to_mode_b: degraded,
            tokens_in,
            tokens_out,
            duration_ms: duration,
            rewritten_question: match rewriter_status {
                ask::session::RewriterStatus::Skipped => None,
                _ => Some(rewrite.rewritten.clone()),
            },
            rewriter_status,
            stage_1b: stage_1b_done.clone(),
        }
```

And the outer returned `AskResponse` at line 1247:

```rust
        ask::AskResponse {
            answer,
            citations,
            hits_used,
            degraded_to_mode_b: degraded,
            tokens_in,
            tokens_out,
            duration_ms: duration,
            rewritten_question: match rewriter_status {
                ask::session::RewriterStatus::Skipped => None,
                _ => Some(rewrite.rewritten.clone()),
            },
            rewriter_status,
            stage_1b: stage_1b_done,
        }
```

- [ ] **Step 8: Run the full test suite**

Run: `cargo test -p mur-core && cargo test -p mur-common`
Expected: PASS, including the two new `ask_response_*` tests.

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/conversations/ask/mod.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "feat(conversations): Phase 3.5 Task 9 — Stage1bStats on AskResponse/AskEvent::Done"
```

---

## Task 10: Plain-mode citation suffix `(summarized)`

**Files:**
- Modify: `mur-core/src/conversations/ask/format.rs:7-26` (`render_citations_block`)

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/ask/format.rs`:

```rust
#[test]
fn citations_block_suffixes_summarized_for_abstractive() {
    let c = vec![
        Citation {
            id: 1,
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            source: "cc".into(),
            conv_id: "c1".into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            snippet: "sample".into(),
            score: 0.9,
            compressed: Some(crate::conversations::ask::Compression::Abstractive),
        },
        Citation {
            id: 2,
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            source: "cc".into(),
            conv_id: "c2".into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            snippet: "sample2".into(),
            score: 0.9,
            compressed: Some(crate::conversations::ask::Compression::Heuristic),
        },
        Citation {
            id: 3,
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            source: "cc".into(),
            conv_id: "c3".into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            snippet: "sample3".into(),
            score: 0.9,
            compressed: None,
        },
    ];
    let block = render_citations_block(&c);
    // Abstractive → (summarized) suffix.
    assert!(
        block.contains("cc/c1:L1") && block.contains("(summarized)"),
        "expected (summarized) next to c1, got:\n{block}"
    );
    // Heuristic → NOT suffixed.
    let lines: Vec<&str> = block.lines().collect();
    let c2_line = lines.iter().find(|l| l.contains("cc/c2:L1")).unwrap();
    assert!(
        !c2_line.contains("(summarized)"),
        "heuristic must NOT be marked summarized in plain mode; got: {c2_line}"
    );
    // None → unchanged.
    let c3_line = lines.iter().find(|l| l.contains("cc/c3:L1")).unwrap();
    assert!(!c3_line.contains("(summarized)"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core format::tests::citations_block_suffixes_summarized_for_abstractive`
Expected: FAIL — current output has no `(summarized)` suffix.

- [ ] **Step 3: Implement the suffix**

In `mur-core/src/conversations/ask/format.rs`, modify `render_citations_block`:

```rust
pub fn render_citations_block(citations: &[Citation]) -> String {
    let mut out = String::new();
    if citations.is_empty() {
        return out;
    }
    out.push_str("\nCitations:\n");
    for c in citations {
        let anchor = match (c.line_hint, c.span_index_in_summary) {
            (_, Some(idx)) => format!(
                "[cit: {} {}/{} @summary-span-{}]",
                c.date, c.source, c.conv_id, idx
            ),
            (Some(line), _) => format!("[cit: {} {}/{}:L{}]", c.date, c.source, c.conv_id, line),
            _ => format!("[cit: {} {}/{}]", c.date, c.source, c.conv_id),
        };
        let preview: String = c.snippet.chars().take(120).collect();
        let suffix = if c.compressed == Some(crate::conversations::ask::Compression::Abstractive) {
            " (summarized)"
        } else {
            ""
        };
        out.push_str(&format!("  {anchor}\n    — {preview}{suffix}\n"));
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core format::tests`
Expected: PASS — new test + all existing format tests.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/conversations/ask/format.rs
git commit -m "feat(conversations): Phase 3.5 Task 10 — plain-mode (summarized) suffix on abstractive citations"
```

---

## Task 11: Footer — `· N summarized` segment

**Files:**
- Modify: `mur-core/src/conversations/ask/format.rs:28-42` (`render_footer`)

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/ask/format.rs`:

```rust
#[test]
fn footer_includes_summarized_segment_when_stage_1b_fired() {
    let mut r = sample_resp();
    r.stage_1b = Some(crate::conversations::ask::abstractive::Stage1bStats {
        compressed_count: 2,
        cache_hits: 0,
        skipped_count: 0,
        duration_ms: 50,
    });
    let f = render_footer(&r);
    assert!(f.contains("2 summarized"), "expected '· 2 summarized' in footer, got: {f}");
}

#[test]
fn footer_omits_summarized_segment_when_stage_1b_none() {
    let r = sample_resp();
    assert!(r.stage_1b.is_none());
    let f = render_footer(&r);
    assert!(!f.contains("summarized"));
}

#[test]
fn footer_omits_summarized_segment_when_compressed_count_zero() {
    let mut r = sample_resp();
    r.stage_1b = Some(crate::conversations::ask::abstractive::Stage1bStats {
        compressed_count: 0,
        cache_hits: 0,
        skipped_count: 1,
        duration_ms: 10,
    });
    let f = render_footer(&r);
    assert!(!f.contains("summarized"), "Stage 1b fired but nothing compressed — no segment");
}
```

Also update `sample_resp` (around line 52) so it initializes `stage_1b: None`:

```rust
    fn sample_resp() -> AskResponse {
        AskResponse {
            answer: "Mock answer [cit: 2026-04-19 cc/a:L1]".into(),
            citations: vec![Citation {
                id: 1,
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                source: "cc".into(),
                conv_id: "a".into(),
                line_hint: Some(1),
                span_index_in_summary: None,
                snippet: "sample snippet text".into(),
                score: 0.87,
                compressed: None,
            }],
            hits_used: vec![],
            degraded_to_mode_b: false,
            tokens_in: 100,
            tokens_out: 20,
            duration_ms: 500,
            rewritten_question: None,
            rewriter_status: crate::conversations::ask::session::RewriterStatus::Skipped,
            stage_1b: None,
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core format::tests::footer_includes_summarized_segment_when_stage_1b_fired`
Expected: FAIL.

- [ ] **Step 3: Implement the segment**

In `mur-core/src/conversations/ask/format.rs`, modify `render_footer`:

```rust
pub fn render_footer(resp: &AskResponse) -> String {
    let tag = if resp.degraded_to_mode_b {
        " · Mode B fallback"
    } else {
        ""
    };
    // Phase 3.5: insert "· N summarized" between hit count and latency when
    // Stage 1b compressed at least one hit. Heuristic count (Stage 1) is
    // intentionally not shown here — matches plain-mode provenance philosophy.
    let summarized_seg = match &resp.stage_1b {
        Some(s) if s.compressed_count > 0 => format!(" · {} summarized", s.compressed_count),
        _ => String::new(),
    };
    format!(
        "({} hits{} · {}ms · {}→{} tokens{})\n",
        resp.citations.len(),
        summarized_seg,
        resp.duration_ms,
        resp.tokens_in,
        resp.tokens_out,
        tag,
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core format::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/conversations/ask/format.rs
git commit -m "feat(conversations): Phase 3.5 Task 11 — footer segment '· N summarized' when Stage 1b fires"
```

---

## Task 12: CLI integration tests — four Phase 3.5 scenarios

**Files:**
- Modify: `mur-core/tests/cli_conversations.rs` (append four new tests at end, before the final `}` or end-of-file)

- [ ] **Step 1: Write the four failing tests**

Append to `mur-core/tests/cli_conversations.rs` (after the existing `mur_conversations_rollup_force_still_regenerates` test, around line 738 — verify the exact end-of-file with `tail`):

```rust
/// Phase 3.5: with a tight budget + long hits, Stage 1b should fire, JSON
/// should carry `.stage_1b.compressed_count > 0`, and the plain-mode footer
/// should include "summarized".
#[test]
fn mur_ask_stage_1b_fires_on_overflow() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // Tight max_context_tokens + summarize_hits_enabled (default true).
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n    compress_hits_enabled: true\n",
    )
    .unwrap();

    // Seed a day with one long JSONL line so retrieve surfaces something.
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let raw = mur_home.join("conversations").join("raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    let long_body = "fact ".repeat(120);
    let line = serde_json::json!({
        "v": 1,
        "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code",
        "conv": "c1",
        "role": "user",
        "content": {"t": "text", "v": long_body},
        "meta": {},
        "refs": []
    });
    std::fs::write(
        raw.join("cc_c1.jsonl"),
        serde_json::to_string(&line).unwrap() + "\n",
    )
    .unwrap();

    // Compact to produce summary + spans.
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("compact");
    assert!(out.status.success(), "compact failed: {}", String::from_utf8_lossy(&out.stderr));

    // Ask with JSON output.
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("mur ask --json");
    assert!(out.status.success(), "ask failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect(&format!("parse JSON: {stdout}"));
    // Key Phase 3.5 assertion: .stage_1b.compressed_count > 0 when overflow triggers abstractive path.
    // (It's possible the budget was loose enough that only Stage 1 fired — in
    // that case the field may be absent. Gate the test on "present AND > 0"
    // vs. "absent/zero" to avoid flakiness; the budget above is tuned to
    // force Stage 1b under the mock shrinker.)
    let compressed = v.pointer("/stage_1b/compressed_count").and_then(|n| n.as_u64()).unwrap_or(0);
    let cache_hits = v.pointer("/stage_1b/cache_hits").and_then(|n| n.as_u64()).unwrap_or(0);
    assert!(
        compressed + cache_hits > 0,
        "expected Stage 1b compressed_count+cache_hits > 0 under tight budget; got JSON: {stdout}"
    );
}

/// Phase 3.5: setting `summarize_hits_enabled: false` must short-circuit
/// Stage 1b. JSON must either omit `stage_1b` or have zero counts.
#[test]
fn mur_ask_stage_1b_disabled_via_config() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: false\n    compress_hits_enabled: true\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what did I ship?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("mur ask --json");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    // Either the field is absent, or its counts are all zero.
    let stage_1b_present_and_nonzero = v.get("stage_1b").is_some_and(|s| {
        s.get("compressed_count").and_then(|n| n.as_u64()).unwrap_or(0) > 0
    });
    assert!(
        !stage_1b_present_and_nonzero,
        "Stage 1b must not fire when disabled; got: {stdout}"
    );
}

/// Phase 3.5: second ask over the same seeded archive and question should
/// see `.stage_1b.cache_hits > 0` (fewer fresh compressions, more cache
/// hits) when the first ask's inputs warm the cache.
#[test]
fn mur_ask_stage_1b_cache_hits_on_second_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d").to_string();
    let raw = mur_home.join("conversations").join("raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    let long_body = "fact ".repeat(120);
    let line = serde_json::json!({
        "v": 1, "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code", "conv": "c1", "role": "user",
        "content": {"t": "text", "v": long_body}, "meta": {}, "refs": []
    });
    std::fs::write(raw.join("cc_c1.jsonl"), serde_json::to_string(&line).unwrap() + "\n").unwrap();

    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path()).env("MUR_OLLAMA_MOCK", "1")
        .output().expect("compact");

    // First ask — starts new session.
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path()).env("MUR_OLLAMA_MOCK", "1")
        .output().expect("ask 1");

    // Second ask — identical question → same cache key. Use --continue or
    // new-session; either way cache is keyed on model + target + content,
    // not on session state.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path()).env("MUR_OLLAMA_MOCK", "1")
        .output().expect("ask 2");
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout2).expect("parse JSON");
    let cache_hits = v.pointer("/stage_1b/cache_hits").and_then(|n| n.as_u64()).unwrap_or(0);
    assert!(
        cache_hits > 0,
        "second ask should see cache_hits > 0; got JSON: {stdout2}"
    );
}

/// Phase 3.5: when Stage 1b hits a timeout, the ask must still succeed
/// (soft-fail). The answer is produced from the original un-summarized
/// hits; exit code is zero.
#[test]
fn mur_ask_stage_1b_soft_fails_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d").to_string();
    let raw = mur_home.join("conversations").join("raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    let long_body = "fact ".repeat(120);
    let line = serde_json::json!({
        "v": 1, "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code", "conv": "c1", "role": "user",
        "content": {"t": "text", "v": long_body}, "meta": {}, "refs": []
    });
    std::fs::write(raw.join("cc_c1.jsonl"), serde_json::to_string(&line).unwrap() + "\n").unwrap();

    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path()).env("MUR_OLLAMA_MOCK", "1")
        .output().expect("compact");

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home).env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .env("MUR_ABSTRACTIVE_MOCK_FAIL", "timeout")
        .output().expect("ask with FAIL=timeout");
    assert!(
        out.status.success(),
        "soft-fail: ask must still exit 0 when Stage 1b times out; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    // stage_1b should be present with skipped_count > 0.
    let skipped = v.pointer("/stage_1b/skipped_count").and_then(|n| n.as_u64()).unwrap_or(0);
    assert!(skipped > 0, "timeout must register as skipped_count > 0; got: {stdout}");
}
```

**Note about test-time timeouts:** the `MUR_ABSTRACTIVE_MOCK_FAIL=timeout` branch in `OllamaClient::generate` sleeps 10 seconds. The `AbstractiveCtx::timeout` is `CALL_TIMEOUT = 5s`. So the test will wait ~5s before each timed-out hit returns `Skipped(timeout)`. Keep the seeded content size modest — one day × one conversation — so only ~1 hit gets that treatment and the test runs in well under a minute.

- [ ] **Step 2: Run the four tests**

Run: `cargo test -p mur-core --test cli_conversations mur_ask_stage_1b -- --test-threads=1`
Expected: all four PASS. Running single-threaded avoids env-var races on `MUR_ABSTRACTIVE_MOCK_FAIL`.

- [ ] **Step 3: `cargo fmt` + clippy**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add mur-core/tests/cli_conversations.rs
git commit -m "test(conversations): Phase 3.5 Task 12 — four CLI integration tests for Stage 1b"
```

---

## Task 13: Golden path — Step 18

**Files:**
- Modify: `scripts/golden-path-conversations.sh:225-228` (add Step 18, update final banner)

- [ ] **Step 1: Insert Step 18 before the final banner**

In `scripts/golden-path-conversations.sh`, replace the final lines (currently):

```bash
echo ""
echo "=== ALL 17 STEPS GREEN ==="
```

with:

```bash
# ── Step 18: Phase 3.5 Stage 1b fires under tight budget ──────────────
echo "--- step 18: mur ask --json (expect .stage_1b compressed_count+cache_hits > 0) ---"
# Nudge the budget tight on the fly for this one invocation via a config
# override. The archive is already populated from earlier steps.
GP_CONFIG_OVERRIDE="$TMPHOME/.mur/config.yaml"
# Back up existing config (if any) and drop a tighter one.
if [ -f "$GP_CONFIG_OVERRIDE" ]; then
    cp "$GP_CONFIG_OVERRIDE" "$GP_CONFIG_OVERRIDE.bak"
fi
cat > "$GP_CONFIG_OVERRIDE" <<'EOF'
conversations:
  ask:
    max_context_tokens: 400
    summarize_hits_enabled: true
    compress_hits_enabled: true
EOF

MUR_OLLAMA_MOCK=1 "$MUR" ask --json "what did I ship this week?" > /tmp/gp-step-18.json 2>/tmp/gp-step-18.err
test -s /tmp/gp-step-18.json || { echo "FAIL step 18: empty JSON output; stderr:"; cat /tmp/gp-step-18.err; exit 1; }
# stage_1b may be absent on non-overflow — but under this budget it MUST fire.
jq -e '(.stage_1b.compressed_count // 0) + (.stage_1b.cache_hits // 0) > 0' /tmp/gp-step-18.json \
  || { echo "FAIL step 18: Stage 1b didn't fire (compressed+cache_hits = 0); JSON:"; cat /tmp/gp-step-18.json; exit 1; }

# Restore prior config to avoid surprising downstream re-runs.
if [ -f "$GP_CONFIG_OVERRIDE.bak" ]; then
    mv "$GP_CONFIG_OVERRIDE.bak" "$GP_CONFIG_OVERRIDE"
else
    rm -f "$GP_CONFIG_OVERRIDE"
fi

echo ""
echo "=== ALL 18 STEPS GREEN ==="
```

- [ ] **Step 2: Run the golden path end-to-end**

Run: `bash scripts/golden-path-conversations.sh`
Expected: exits 0; final line is `=== ALL 18 STEPS GREEN ===`. Requires a fresh build (`./build.sh` or `cargo build --release` first, depending on what the script uses — check top of script).

If the script pins `MUR` to a specific binary, rebuild first: `cargo build --release -p mur-core && bash scripts/golden-path-conversations.sh`.

- [ ] **Step 3: Commit**

```bash
git add scripts/golden-path-conversations.sh
git commit -m "test(conversations): Phase 3.5 Task 13 — golden path Step 18 (Stage 1b overflow)"
```

---

## Task 14: README + docs — call out Stage 1b + config surface

**Files:**
- Modify: `README.md` (sidenote on `mur ask` under tight budgets — ≤5 lines)

- [ ] **Step 1: Find the section to touch**

Run: `grep -n "mur ask\|compress_hits_enabled\|overflow" README.md | head -20`
Expected: several hits under `mur ask` config coverage. If `compress_hits_enabled` is mentioned, extend that paragraph; if not, add a short subsection.

- [ ] **Step 2: Update the `mur ask` config section**

Add the following lines to the README section that documents `ask.*` config (use whatever surrounding prose style the file uses — do NOT introduce new headings unless the file already has a per-field table). Minimum content:

```
`summarize_hits_enabled` (default `true`) — under tight context budgets the
`mur ask` overflow cascade runs an Ollama LLM to abstractively summarize the
longest hits (Stage 1b). Results are cached per hit at
`~/.mur/conversations/cache/abstractive/`, so the first overflow query over a
given hit pays the LLM latency once. Disable to restore pre-3.5 behavior
(drop history first). Hardcoded per-call timeout: 5s; soft-fails on any
error.

`summarize_model` (default `null` → falls back to `model`) — override the
answer model for Stage 1b. Pair with a faster model like `qwen3:4b` to trade
accuracy for speed on the summarization hop.
```

- [ ] **Step 3: Verify no broken claims**

Run: `cargo run -p mur-core -- verify --file README.md` if that works in your environment. Otherwise skim visually.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: Phase 3.5 Task 14 — README note on summarize_hits_enabled + summarize_model"
```

---

## Final Verification

- [ ] **Step 1: Full test pass**

Run:
```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```
Expected: zero format diffs, zero clippy warnings, all tests green.

- [ ] **Step 2: Golden path green**

Run: `bash scripts/golden-path-conversations.sh`
Expected: `=== ALL 18 STEPS GREEN ===`.

- [ ] **Step 3: Spec cross-check**

Re-read `docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-5-design.md` §11 "Success criteria" items 1–6. Confirm each:

1. Overflow preserves history: Task 8 inserts Stage 1b BEFORE Stage 2 ✓
2. Cold-cache latency ≤ 15s: `CALL_TIMEOUT = 5s`, typical 1–3 hits ✓
3. Soft-fail never errors: Task 5 skip-reason taxonomy + soft-fail tests ✓
4. `(summarized)` marker only on Abstractive: Task 10 test covers this ✓
5. Golden path 18 steps: Task 13 ✓
6. Zero behavior change on non-overflow: Stage 1b gated on `cur_tokens > max_context_tokens`, default-on is pure addition (prior cascade still runs for under-budget queries unchanged) ✓

---

## Notes for the implementing agent

1. **Worktree path.** Every file path in this plan is relative to `/Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-5/`. Do all work there; do not touch `main` directly.
2. **Rust edition 2024 let-chains.** The spec and this plan use `if ... && let Some(x) = y` in a couple of places. That's supported by the project (CLAUDE.md confirms). Keep the style.
3. **Env-var races in tests.** `MUR_OLLAMA_MOCK` and `MUR_ABSTRACTIVE_MOCK_FAIL` are process-global. The existing suite uses `crate::conversations::ENV_LOCK` (a `std::sync::Mutex`) to serialize tests that set envs. Follow that pattern in every new `#[tokio::test]` that sets either var, and remove the var on both the success and failure paths.
4. **`#![allow(dead_code)]` at top of new modules** is acceptable — the project convention (see `compress.rs`, `ollama.rs`) attaches it until the caller lands.
5. **Never make `prompt::render` async without updating `ask_stream`** — there is exactly one non-test caller and it's in `ask_stream`, which is already async. Test callers become `#[tokio::test]`.
6. **Heuristic count is NOT in the footer.** Only `Abstractive` count. Aligned with plain-mode philosophy (heuristic is lossless-ish, abstractive is lossy).
7. **Session JSONL compat.** Task 3 added `#[serde(default, skip_serializing_if = "Option::is_none")]` to `Citation.compressed` — essential for reading old `ask-session.jsonl` files after upgrading. Do not drop that attribute.
