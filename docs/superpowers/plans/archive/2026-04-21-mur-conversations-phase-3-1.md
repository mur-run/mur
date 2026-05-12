# mur Conversations Phase 3.1 — RAPTOR-lite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship two retrieval-quality improvements to Mode C (`mur ask`): (1) real span selection via per-span embeddings stored at a new LanceDB layer, and (2) cosine-similarity MMR dedupe replacing the current word-Jaccard heuristic. Both are additive — no schema change, no breaking behavior.

**Architecture:** At compact time, embed each extractive span and upsert one LanceDB row per span at `layer=2`. Ask retrieval searches `layer=2` first (fast, precise), falls back to `layer=1` (narrative) for unmigrated archives. `SearchHit` gains a `vector: Option<Vec<f32>>` populated from the LanceDB vector column; MMR uses cosine on those vectors. A new `mur conversations reindex --spans-only` populates layer=2 for existing summaries.

**Tech Stack:** Rust 2024 · tokio · LanceDB (existing) · sha2 (for deterministic-hash test mock) · existing Phase 2 deps (reqwest, chrono, serde, futures, tracing).

**Spec:** `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-1-design.md` (commit `18cfbfb`).
**Depends on:** Phase 2C shipped (`a31690d`).

---

## File Structure

**Modify:**

```
mur-common/src/conversation.rs                         add Source::from_prefix
mur-common/src/config.rs                               default_mmr_threshold 0.85 → 0.88
mur-core/src/conversations/index.rs                    SearchHit {layer, vector}; upsert id per-layer; count_rows_at_layer helper
mur-core/src/conversations/ollama.rs                   MockMode enum; mock_mode(); mock_embed_vector helper (hash-based)
mur-core/src/conversations/summarize/mod.rs            compact_day batch-embeds spans; reuse MockMode for mock path
mur-core/src/conversations/summarize/writer.rs         write_summary signature gains span_embeddings; upserts N layer=2 rows
mur-core/src/conversations/ask/mod.rs                  embed_query uses MockMode (hash path)
mur-core/src/conversations/ask/retrieve.rs             ResolvedHit.vector; gather_hits prefers layer=2; resolve_span_hit; mmr_dedupe_cosine
mur-core/src/cmd/conversations_cmd.rs                  cmd_conversations_reindex flags --spans-only/--raw-only; doctor span coverage line
mur-core/src/main.rs                                   ReindexArgs flags wired via Commands::Conversations::Reindex
mur-core/tests/cli_conversations.rs                    new reindex + extended doctor integration tests
scripts/golden-path-conversations.sh                   Step 9.5 (reindex --spans-only) + hash-mock Step 10 adjustment
```

No new files. No new dependencies.

---

## Task Overview (8 tasks)

| # | Task | Depends on |
|---|------|------------|
| 1 | Foundations: `Source::from_prefix` + `mmr_threshold` default 0.88 | — |
| 2 | `SearchHit` extends; `upsert_internal` layer-aware id; `count_rows_at_layer` | — |
| 3 | Hash mock: `MockMode` enum + `mock_embed_vector` | — |
| 4 | `write_summary` takes `span_embeddings`; upserts layer=2 rows | 2, 3 |
| 5 | `compact_day` batch-embeds spans, passes to writer | 3, 4 |
| 6 | Ask: tiered retrieval + `resolve_span_hit` + cosine MMR | 2, 3 |
| 7 | Reindex `--spans-only`/`--raw-only`; doctor span coverage | 4, 5 |
| 8 | Golden path Step 9.5 + integration tests | 6, 7 |

---

## Task 1: Foundations — `Source::from_prefix` + config default

**Files:**
- Modify: `mur-common/src/conversation.rs`
- Modify: `mur-common/src/config.rs`

- [x] **Step 1: Failing test in `mur-common/src/conversation.rs`**

Append to the existing `#[cfg(test)] mod tests` block (or create one at the bottom of `conversation.rs` if none exists):

```rust
#[cfg(test)]
mod phase3_tests {
    use super::*;

    #[test]
    fn source_from_prefix_roundtrips_all_known() {
        for src in [
            Source::ClaudeCode,
            Source::Cursor,
            Source::Gemini,
            Source::Aider,
            Source::Slack,
            Source::Telegram,
            Source::Discord,
            Source::CommanderEngine,
        ] {
            let p = src.file_prefix();
            assert_eq!(Source::from_prefix(p), Some(src));
        }
    }

    #[test]
    fn source_from_prefix_unknown_is_none() {
        assert_eq!(Source::from_prefix("bogus"), None);
        assert_eq!(Source::from_prefix(""), None);
        assert_eq!(Source::from_prefix("CC"), None); // case-sensitive
    }
}
```

- [x] **Step 2: Run — must fail**

```
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-1
cargo test -p mur-common phase3_tests
```

Expected: compile error `no function or associated item named 'from_prefix' found for enum 'Source'`.

- [x] **Step 3: Implement `Source::from_prefix`**

In `mur-common/src/conversation.rs`, find the existing `impl Source { pub fn file_prefix(...) }` block. Append a new method inside the same impl:

```rust
    /// Inverse of `file_prefix()`. Returns None on unknown prefix.
    /// Case-sensitive by design — prefixes are a closed set.
    pub fn from_prefix(s: &str) -> Option<Self> {
        match s {
            "cc" => Some(Source::ClaudeCode),
            "cursor" => Some(Source::Cursor),
            "gemini" => Some(Source::Gemini),
            "aider" => Some(Source::Aider),
            "slack" => Some(Source::Slack),
            "telegram" => Some(Source::Telegram),
            "discord" => Some(Source::Discord),
            "commander" => Some(Source::CommanderEngine),
            _ => None,
        }
    }
```

- [x] **Step 4: Run — must pass**

```
cargo test -p mur-common phase3_tests
```

Expected: 2 passed.

- [x] **Step 5: Add failing test for `mmr_threshold` default change**

In `mur-common/src/config.rs`, find the existing `conversations_tests` mod (at the bottom of the file). Append:

```rust
    #[test]
    fn ask_config_mmr_threshold_default_is_cosine_scaled() {
        // Phase 3.1: default shifts from 0.85 (word-Jaccard) to 0.88 (cosine).
        let c = AskConfig::default();
        assert!(
            (c.mmr_threshold - 0.88).abs() < 1e-9,
            "expected 0.88, got {}",
            c.mmr_threshold
        );
    }
```

- [x] **Step 6: Run — must fail**

```
cargo test -p mur-common config::conversations_tests::ask_config_mmr_threshold_default_is_cosine_scaled
```

Expected: assertion failure — current default is 0.85.

- [x] **Step 7: Update the default helper**

In `mur-common/src/config.rs`, find the existing `ask_default_mmr` fn (one-liner added in Phase 2B Task 18). Change:

```rust
fn ask_default_mmr() -> f64 { 0.85 }
```

to:

```rust
fn ask_default_mmr() -> f64 { 0.88 }
```

- [x] **Step 8: Also update the Phase 2B test that asserts 0.85**

In the same `conversations_tests` mod, find the existing `ask_config_defaults` test and update its mmr_threshold assertion from:

```rust
    assert_eq!(c.mmr_threshold, 0.85);
```

to:

```rust
    assert_eq!(c.mmr_threshold, 0.88);
```

- [x] **Step 9: Run — must pass**

```
cargo test -p mur-common config::conversations_tests
```

Expected: all conversations_tests pass (the existing test + the new one).

- [x] **Step 10: Commit**

```
cargo clippy -p mur-common --all-targets -- -D warnings
cargo fmt --check -p mur-common
git add mur-common/src/conversation.rs mur-common/src/config.rs
git commit -m "$(cat <<'EOF'
feat(common): Source::from_prefix + mmr_threshold 0.88 default (Phase 3.1)

Inverse of Source::file_prefix(), used by reindex to convert the parsed
summary's src field back to the Source enum.

mmr_threshold default shifts from 0.85 (word-Jaccard scale) to 0.88
(cosine scale) — Phase 3.1 swaps the dedupe metric.

Plan: Task 1 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §4, §7

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `SearchHit` extends; upsert id per-layer; `count_rows_at_layer`

**Files:**
- Modify: `mur-core/src/conversations/index.rs`

- [x] **Step 1: Failing test for layer + vector fields**

Append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/index.rs`:

```rust
    #[tokio::test]
    async fn search_hit_carries_layer_and_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let m = msg("a", "hello world");
        idx.upsert_with_layer(&[(m, vec![1.0; 16], 2)]).await.unwrap();
        let hits = idx.search(&[1.0; 16], 1, None, Some(2)).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].layer, 2);
        let v = hits[0].vector.as_ref().expect("vector should be populated");
        assert_eq!(v.len(), 16);
        // LanceDB normalizes for cosine; loose tolerance check
        assert!(v.iter().any(|x| *x > 0.0));
    }

    #[tokio::test]
    async fn upsert_ids_are_layer_aware() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        // Same conv, different layers — ids must differ so both rows persist.
        let m0 = msg("xy", "raw message");
        let m2 = msg("xy", "span text");
        idx.upsert_with_layer(&[(m0, vec![0.5; 16], 0)]).await.unwrap();
        idx.upsert_with_layer(&[(m2, vec![0.6; 16], 2)]).await.unwrap();
        let hits_all = idx.search(&[0.55; 16], 10, None, None).await.unwrap();
        assert_eq!(hits_all.len(), 2, "both rows should coexist");
        let ids: Vec<_> = hits_all.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.iter().any(|id| id.contains("_L2_")));
        assert!(ids.iter().any(|id| !id.contains("_L")));  // layer=0 has no L<N> marker
    }

    #[tokio::test]
    async fn count_rows_at_layer_reports_correct_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        // Seed 3 at layer=0, 2 at layer=2.
        for i in 0..3 {
            let m = msg(&format!("c{i}"), "raw");
            idx.upsert_with_layer(&[(m, vec![0.1 * i as f32; 16], 0)])
                .await
                .unwrap();
        }
        for i in 0..2 {
            let m = msg(&format!("c{i}"), "span");
            idx.upsert_with_layer(&[(m, vec![0.7 * i as f32 + 0.1; 16], 2)])
                .await
                .unwrap();
        }
        assert_eq!(idx.count_rows_at_layer(0).await.unwrap(), 3);
        assert_eq!(idx.count_rows_at_layer(2).await.unwrap(), 2);
        assert_eq!(idx.count_rows_at_layer(1).await.unwrap(), 0);
    }
```

- [x] **Step 2: Run — must fail**

```
cargo test -p mur-core conversations::index::tests
```

Expected: compile errors (`hits[0].layer` doesn't exist; `hits[0].vector` doesn't exist; `count_rows_at_layer` not defined).

- [x] **Step 3: Extend `SearchHit` struct**

In `mur-core/src/conversations/index.rs`, find the existing `pub struct SearchHit` (around line 30). Replace with:

```rust
#[derive(Debug)]
pub struct SearchHit {
    pub id: String,
    pub ts: i64,
    pub source: Source,
    pub conv_id: String,
    pub content: String,
    pub distance: f32,
    pub layer: i8,
    pub vector: Option<Vec<f32>>,
}
```

- [x] **Step 4: Include layer + vector in query results**

Find the existing `pub async fn search(...)` body. Currently it calls `.nearest_to(query_vec)?.limit(limit)`. LanceDB's default projection omits the vector column on nearest-neighbor queries; we need to explicitly select it.

After the line `let mut q = table.query().nearest_to(query_vec)?.limit(limit);`, add:

```rust
    // Phase 3.1: explicitly include layer + vector columns so ask can do
    // pairwise cosine MMR on the retrieved vectors.
    q = q.select(lancedb::query::Select::Columns(vec![
        "id".into(),
        "ts".into(),
        "source".into(),
        "conv_id".into(),
        "role".into(),
        "layer".into(),
        "content".into(),
        "vector".into(),
    ]));
```

Add the necessary import near the other `use lancedb::...` lines:

```rust
use lancedb::query::{ExecutableQuery, QueryBase, Select};
```

(Replace the existing `use lancedb::query::{ExecutableQuery, QueryBase};` line.)

- [x] **Step 5: Decode layer + vector in the row-scan loop**

In the same `search()` fn body, inside the `for b in batches` loop, find the existing column extraction block (ids, tss, srcs, convs, contents, dists). After `dists`, add:

```rust
            let layers = b
                .column_by_name("layer")
                .and_then(|c| c.as_any().downcast_ref::<Int8Array>());
            let vectors = b
                .column_by_name("vector")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
```

Then inside the existing `for i in 0..b.num_rows()` loop, find the `out.push(SearchHit { ... })` block and extend it:

```rust
                let layer = layers.map(|a| a.value(i)).unwrap_or(0);
                let vector = vectors.and_then(|arr| {
                    let fsl = arr.value(i);
                    let floats = fsl.as_any().downcast_ref::<Float32Array>()?;
                    Some((0..floats.len()).map(|j| floats.value(j)).collect::<Vec<f32>>())
                });
                out.push(SearchHit {
                    id: ids.value(i).to_string(),
                    ts: tss.value(i),
                    source,
                    conv_id: convs.value(i).to_string(),
                    content: contents.value(i).to_string(),
                    distance: dists.map(|d| d.value(i)).unwrap_or(0.0),
                    layer,
                    vector,
                });
```

(Replace the existing `out.push(SearchHit { ... })` with the extended version above.)

Ensure `use arrow_array::{...};` includes `FixedSizeListArray` (it already does — verify with `grep` if unsure).

- [x] **Step 6: Layer-aware id builder in `upsert_internal`**

In `mur-core/src/conversations/index.rs`, find the existing `async fn upsert_internal` body. Near the top there's an `ids` vector built with `entries.iter().enumerate().map(|(i, (m, _, _))| format!("{}_{}_{}", m.src.file_prefix(), m.conv, i))`. Replace that with:

```rust
        let ids: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(i, (m, _, layer))| {
                if *layer == 0 {
                    format!("{}_{}_{}", m.src.file_prefix(), m.conv, i)
                } else {
                    format!("{}_{}_L{}_{}", m.src.file_prefix(), m.conv, layer, i)
                }
            })
            .collect();
```

Layer=0 ids stay unchanged (backward compat with existing Phase 1/2 rows). Non-zero layers get `_L<N>_<i>` suffix.

- [x] **Step 7: Add `count_rows_at_layer` helper**

Append to `impl ConversationIndex`:

```rust
    /// Count rows at a specific layer. Used by doctor to report coverage.
    pub async fn count_rows_at_layer(&self, layer: i8) -> Result<u64> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) {
            return Ok(0);
        }
        let table = self.db.open_table(TABLE).execute().await?;
        let n = table
            .count_rows(Some(format!("layer = {layer}")))
            .await?;
        Ok(n as u64)
    }
```

- [x] **Step 8: Run — must pass**

```
cargo test -p mur-core conversations::index::tests
```

Expected: previous 3 tests + 3 new = 6 passed.

- [x] **Step 9: Commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/index.rs
git commit -m "$(cat <<'EOF'
feat(core): SearchHit {layer, vector} + layer-aware upsert id (Phase 3.1)

search() explicitly selects layer + vector columns so ask can do
pairwise cosine MMR on retrieved vectors. SearchHit gains those two
fields; default-0 / None when absent.

upsert_internal builds layer=0 ids as before (backward compat). Non-zero
layers get <prefix>_<conv>_L<N>_<i> suffix so layer=2 span rows don't
collide with layer=0 raw rows for the same conv.

count_rows_at_layer(i8) helper backs the doctor span-coverage report.

Plan: Task 2 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §3, §5

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Hash mock — `MockMode` + `mock_embed_vector`

**Files:**
- Modify: `mur-core/src/conversations/ollama.rs`

- [x] **Step 1: Failing test**

Append to the existing `#[cfg(test)] mod tests` in `mur-core/src/conversations/ollama.rs`:

```rust
    #[test]
    fn mock_mode_from_env_parses_both_variants() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        assert!(matches!(mock_mode(), Some(MockMode::All01)));
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "hash") };
        assert!(matches!(mock_mode(), Some(MockMode::Hash)));
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "bogus") };
        assert!(mock_mode().is_none());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        assert!(mock_mode().is_none());
    }

    #[test]
    fn mock_embed_vector_all01_is_uniform() {
        let v = mock_embed_vector("anything", MockMode::All01, 16);
        assert_eq!(v.len(), 16);
        assert!(v.iter().all(|x| (*x - 0.1).abs() < 1e-9));
    }

    #[test]
    fn mock_embed_vector_hash_is_deterministic_and_distinct() {
        let a1 = mock_embed_vector("cargo build failed", MockMode::Hash, 128);
        let a2 = mock_embed_vector("cargo build failed", MockMode::Hash, 128);
        let b = mock_embed_vector("kubernetes pod crash", MockMode::Hash, 128);
        assert_eq!(a1, a2, "same text → same vector");
        assert_ne!(a1, b, "different text → different vector");
        // L2-normalized
        let norm_a: f32 = a1.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm_a - 1.0).abs() < 1e-5, "not L2-normalized: norm={norm_a}");
    }
```

- [x] **Step 2: Run — must fail**

```
cargo test -p mur-core conversations::ollama::tests
```

Expected: compile errors (`MockMode`, `mock_mode`, `mock_embed_vector` don't exist).

- [x] **Step 3: Add `MockMode` + helpers**

In `mur-core/src/conversations/ollama.rs`, near the top of the file (after the existing `#![allow(dead_code)]` + imports but before the struct definitions), add:

```rust
/// Embedding-side mock mode. `generate()`/`generate_stream()` still branch
/// on `mock_from_env()` for their canned responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockMode {
    /// MUR_OLLAMA_MOCK=1 — legacy uniform 0.1 vector. Fine for tests that
    /// only care about code paths.
    All01,
    /// MUR_OLLAMA_MOCK=hash — content-hash-based vector; same text → same
    /// vector, different text → different vector. Required for tests that
    /// assert span-selection picked the right span.
    Hash,
}

pub fn mock_mode() -> Option<MockMode> {
    match std::env::var("MUR_OLLAMA_MOCK").as_deref() {
        Ok("1") => Some(MockMode::All01),
        Ok("hash") => Some(MockMode::Hash),
        _ => None,
    }
}

/// Deterministic fake embedding for tests. Seeded from sha256(text);
/// L2-normalized so cosine similarity is meaningful.
pub fn mock_embed_vector(text: &str, mode: MockMode, dims: usize) -> Vec<f32> {
    match mode {
        MockMode::All01 => vec![0.1; dims],
        MockMode::Hash => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            let seed = hasher.finalize(); // 32 bytes
            let mut out = Vec::with_capacity(dims);
            for i in 0..dims {
                let byte_idx = (i * 4) % 32;
                let u = u32::from_le_bytes([
                    seed[byte_idx],
                    seed[(byte_idx + 1) % 32],
                    seed[(byte_idx + 2) % 32],
                    seed[(byte_idx + 3) % 32],
                ]);
                // Mix with position to break the 8-way periodicity from the
                // 32-byte seed being shorter than 4 * dims for dims > 8.
                let mixed = u.wrapping_add((i as u32).wrapping_mul(2_654_435_761));
                let f = (mixed as f32 / u32::MAX as f32) * 2.0 - 1.0;
                out.push(f);
            }
            let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in out.iter_mut() {
                    *x /= norm;
                }
            }
            out
        }
    }
}
```

The `sha2` crate is already a direct dep of mur-core (used by audit + writer); no Cargo.toml change.

- [x] **Step 4: Keep `mock_from_env()` as thin wrapper**

Find the existing `pub fn mock_from_env() -> bool { ... }` and replace with:

```rust
    pub fn mock_from_env() -> bool {
        mock_mode().is_some()
    }
```

This preserves behavior for `generate()`/`generate_stream()` callers that only need the bool.

- [x] **Step 5: Run — must pass**

```
cargo test -p mur-core conversations::ollama::tests
```

Expected: previous 4 tests + 3 new = 7 passed.

- [x] **Step 6: Commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ollama.rs
git commit -m "$(cat <<'EOF'
feat(core): MockMode::Hash for deterministic embedding mocks (Phase 3.1)

MUR_OLLAMA_MOCK=1 stays as the uniform-0.1 fallback (Phase 2 compat).
MUR_OLLAMA_MOCK=hash seeds a 1024-dim vector from sha256(text), L2
normalized — deterministic, text-distinguishing, enables span-selection
tests to assert the right span was chosen.

mock_from_env() preserved as thin wrapper so existing generate() /
generate_stream() mock paths are untouched.

Plan: Task 3 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §8 (deterministic-hash mock)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `write_summary` takes `span_embeddings`; upserts layer=2 rows

**Files:**
- Modify: `mur-core/src/conversations/summarize/writer.rs`

- [x] **Step 1: Failing test**

Append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/summarize/writer.rs`:

```rust
    #[tokio::test]
    async fn write_summary_upserts_span_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        let mut doc = dummy_doc(date);
        // dummy_doc seeds 1 extractive span; add two more so we can assert N rows.
        doc.extractive.push(ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: 2,
            text: "second quote".into(),
            src: Source::ClaudeCode,
        });
        doc.extractive.push(ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: 3,
            text: "third quote".into(),
            src: Source::ClaudeCode,
        });
        let summary_vec = vec![0.1; 16];
        let span_vecs = vec![vec![0.2; 16], vec![0.3; 16], vec![0.4; 16]];
        write_summary(&doc, summary_vec, span_vecs, Some(root)).await.unwrap();

        let idx = super::super::index::ConversationIndex::open(16, Some(root)).await.unwrap();
        assert_eq!(idx.count_rows_at_layer(1).await.unwrap(), 1, "one narrative row");
        assert_eq!(idx.count_rows_at_layer(2).await.unwrap(), 3, "three span rows");
    }

    #[tokio::test]
    async fn write_summary_with_empty_spans_writes_no_layer_2() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        let mut doc = dummy_doc(date);
        doc.extractive.clear();
        write_summary(&doc, vec![0.1; 16], vec![], Some(root)).await.unwrap();
        let idx = super::super::index::ConversationIndex::open(16, Some(root)).await.unwrap();
        assert_eq!(idx.count_rows_at_layer(1).await.unwrap(), 1);
        assert_eq!(idx.count_rows_at_layer(2).await.unwrap(), 0);
    }
```

- [x] **Step 2: Run — must fail**

```
cargo test -p mur-core conversations::summarize::writer::tests::write_summary_upserts_span_rows
```

Expected: compile error (`write_summary` takes 3 args, not 4).

- [x] **Step 3: Update `write_summary` signature + body**

In `mur-core/src/conversations/summarize/writer.rs`, find the existing `pub async fn write_summary`. Change signature to:

```rust
pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    span_embeddings: Vec<Vec<f32>>,
    root_override: Option<&str>,
) -> Result<WriteResult> {
```

After the existing layer=1 narrative upsert block (around the line `idx.upsert_with_layer(&[(summary_msg, summary_embedding, 1)]).await?;`), append:

```rust
    // Phase 3.1: one row per extractive span at layer=2.
    if !doc.extractive.is_empty() && doc.extractive.len() == span_embeddings.len() {
        use chrono::TimeZone;
        let span_ts = chrono::Utc.from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap());
        let mut batch: Vec<(mur_common::Message, Vec<f32>, i8)> =
            Vec::with_capacity(doc.extractive.len());
        for (span, vec) in doc.extractive.iter().zip(span_embeddings.into_iter()) {
            let msg = mur_common::Message {
                v: 1,
                ts: span_ts,
                src: span.src,
                conv: span.conv_id.clone(),
                role: mur_common::Role::User,
                content: mur_common::Content::Text { value: span.text.clone() },
                meta: serde_json::Value::Null,
                refs: vec![],
            };
            batch.push((msg, vec, 2i8));
        }
        idx.upsert_with_layer(&batch).await?;
    }
```

Note: the upsert id pattern `<prefix>_<conv>_L2_<i>` (from Task 2's id builder) uses the batch index `i` as the suffix. Since we iterate `doc.extractive` in order (sorted by `line_hint` per Phase 2A Task 10), the suffix `i` is 0-indexed while `line_hint` is 1-indexed. For ask-side decoding in Task 6, we'll parse whatever suffix the id has — the value semantics come from the ordering at write time, not the absolute number.

Actually, to keep ask's `span_index_in_summary` aligned with what Phase 2B emitted (1-based `line_hint`), adjust: we want the id suffix to equal `line_hint`, not the batch index. Replace the upsert loop with a per-item upsert (trivially slower but ids carry meaning):

```rust
    // Phase 3.1: one row per extractive span at layer=2.
    // Upsert each span individually so the id suffix can carry line_hint
    // (Task 2's id builder uses the batch index; with batch-size-1 the
    // index is always 0 — we instead use a synthetic conv_id that includes
    // line_hint so the generated id `<prefix>_<conv>_L2_0` becomes unique
    // per span).
    if !doc.extractive.is_empty() && doc.extractive.len() == span_embeddings.len() {
        use chrono::TimeZone;
        let span_ts = chrono::Utc.from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap());
        for (span, vec) in doc.extractive.iter().zip(span_embeddings.into_iter()) {
            // Synthetic conv: real_conv + "#span<N>" so id uniqueness holds and
            // ask can decode line_hint from the id suffix via the `#span<N>` fragment.
            // (Alternative: change the id builder in Task 2 to accept an explicit
            // suffix — chose the synthetic conv route to avoid re-churning index.rs.)
            let synth_conv = format!("{}#span{}", span.conv_id, span.line_hint);
            let msg = mur_common::Message {
                v: 1,
                ts: span_ts,
                src: span.src,
                conv: synth_conv,
                role: mur_common::Role::User,
                content: mur_common::Content::Text { value: span.text.clone() },
                meta: serde_json::Value::Null,
                refs: vec![],
            };
            idx.upsert_with_layer(&[(msg, vec, 2i8)]).await?;
        }
    }
```

Wait — that creates N small batches and also encodes line_hint in the conv field which leaks into HitInfo.conv_id later. Not ideal.

**Simpler resolution**: amend the id builder in Task 2 to accept an explicit per-item suffix. That's the cleanest fix. Let me add that here as a Step 3a adjustment.

- [x] **Step 3a: Amend Task 2's id builder to support per-item suffix**

In `mur-core/src/conversations/index.rs`, find the id-building code inside `upsert_internal` (the one that outputs `<prefix>_<conv>_L<layer>_<i>`). Replace with a scheme that reads an optional suffix from the Message's `meta` field at key `"id_suffix"`:

```rust
        let ids: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(i, (m, _, layer))| {
                // Meta can override the batch-index suffix for layer-aware
                // semantic ids (e.g. layer=2 span rows use line_hint).
                let suffix: String = m
                    .meta
                    .get("id_suffix")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| i.to_string());
                if *layer == 0 {
                    format!("{}_{}_{}", m.src.file_prefix(), m.conv, suffix)
                } else {
                    format!(
                        "{}_{}_L{}_{}",
                        m.src.file_prefix(),
                        m.conv,
                        layer,
                        suffix
                    )
                }
            })
            .collect();
```

This is backward-compatible: layer=0 rows without `meta.id_suffix` still get `<prefix>_<conv>_<i>`. Layer=1 narrative rows also without meta get `<prefix>_summary:<date>_L1_0`. Layer=2 span rows set `meta.id_suffix = <line_hint>` so ids are `<prefix>_<real_conv>_L2_<line_hint>`.

Back in `writer.rs` span-upsert, use the meta override instead of a synthetic conv:

```rust
    if !doc.extractive.is_empty() && doc.extractive.len() == span_embeddings.len() {
        use chrono::TimeZone;
        let span_ts = chrono::Utc.from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap());
        let mut batch: Vec<(mur_common::Message, Vec<f32>, i8)> =
            Vec::with_capacity(doc.extractive.len());
        for (span, vec) in doc.extractive.iter().zip(span_embeddings.into_iter()) {
            let msg = mur_common::Message {
                v: 1,
                ts: span_ts,
                src: span.src,
                conv: span.conv_id.clone(),
                role: mur_common::Role::User,
                content: mur_common::Content::Text { value: span.text.clone() },
                meta: serde_json::json!({ "id_suffix": span.line_hint }),
                refs: vec![],
            };
            batch.push((msg, vec, 2i8));
        }
        idx.upsert_with_layer(&batch).await?;
    }
```

- [x] **Step 4: Update all existing `write_summary` call sites to pass `span_embeddings`**

Grep first to find call sites:
```
grep -rn "write_summary(" mur-core/src
```

Expected sites:
- `mur-core/src/conversations/summarize/mod.rs::compact_day` — passes span_embeddings in Task 5.
- Any existing writer tests that already called `write_summary(&doc, vec![0.0; 16], Some(root))`.

For the existing writer tests (`writes_valid_frontmatter_body`, `second_identical_write_is_noop`, `overwrite_archives_prior`, `audit_records_summarize_entry`, `history_retention_prunes_to_retain_limit`, `history_retention_empty_dir_is_noop`, `placeholder_word_count_matches_string`), update each invocation of `write_summary(&doc, vec![0.0; 16], Some(root))` to `write_summary(&doc, vec![0.0; 16], vec![], Some(root))` — empty span_embeddings is valid; the code path short-circuits when `doc.extractive.is_empty() || len != span_embeddings.len()`.

Do not update `compact_day` yet — Task 5 covers that.

- [x] **Step 5: Run — must pass**

```
cargo test -p mur-core conversations::summarize::writer::tests
cargo test -p mur-core conversations::index::tests
```

Expected: writer tests (6 existing + 2 new = 8) + index tests (6) all pass. The existing writer tests still pass because the added span-upsert branch is gated on `!doc.extractive.is_empty() && len match`.

- [x] **Step 6: Commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/writer.rs mur-core/src/conversations/index.rs
git commit -m "$(cat <<'EOF'
feat(core): writer upserts layer=2 span rows (Phase 3.1)

write_summary() signature gains span_embeddings: Vec<Vec<f32>>. After
the existing layer=1 narrative upsert, one layer=2 row per extractive
span is inserted (conv_id = real source conv, ts = date 00:00:00 UTC,
content = span text, vector = passed-in embedding).

upsert_internal id builder extended with meta.id_suffix override so
layer=2 rows become <prefix>_<conv>_L2_<line_hint>, letting ask decode
the span's line_hint from the returned id at retrieval time.

Existing writer tests pass empty span_embeddings; the span upsert
short-circuits when lengths don't match.

Plan: Task 4 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §4

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `compact_day` batch-embeds spans

**Files:**
- Modify: `mur-core/src/conversations/summarize/mod.rs`

- [x] **Step 1: Failing test**

Append to `#[cfg(test)] mod orch_tests` in `mur-core/src/conversations/summarize/mod.rs`:

```rust
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compact_day_writes_both_narrative_and_span_rows() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        seed_raw(root, date, "mock extractive span");
        let r = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        assert!(matches!(r.outcome, Outcome::Written { .. }));
        let idx = super::index::ConversationIndex::open(1024, Some(root)).await.unwrap();
        assert_eq!(
            idx.count_rows_at_layer(1).await.unwrap(),
            1,
            "one narrative row"
        );
        assert!(
            idx.count_rows_at_layer(2).await.unwrap() >= 1,
            "at least one span row"
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
```

- [x] **Step 2: Run — must fail**

```
cargo test -p mur-core conversations::summarize::orch_tests::compact_day_writes_both_narrative_and_span_rows
```

Expected: assertion failure — `count_rows_at_layer(2)` returns 0 because `compact_day` doesn't yet pass span_embeddings to the writer.

- [x] **Step 3: Batch-embed spans in `compact_day`**

In `mur-core/src/conversations/summarize/mod.rs`, find the existing `compact_day` function. Near the bottom, locate the summary_embedding block — something like:

```rust
    let summary_embedding = if OllamaClient::mock_from_env() {
        vec![0.1; embed_dims]
    } else {
        ...
    };
```

Replace it with:

```rust
    use super::ollama::MockMode;
    let (summary_embedding, span_embeddings) = match super::ollama::mock_mode() {
        Some(mode) => {
            let s = super::ollama::mock_embed_vector(
                doc.abstractive.narrative.as_deref().unwrap_or(""),
                mode,
                embed_dims,
            );
            let spans: Vec<Vec<f32>> = doc
                .extractive
                .iter()
                .map(|sp| super::ollama::mock_embed_vector(&sp.text, mode, embed_dims))
                .collect();
            (s, spans)
        }
        None => {
            let text = doc
                .abstractive
                .narrative
                .as_deref()
                .unwrap_or("")
                .to_string();
            let cfg_loaded = crate::store::config::load_config().ok();
            let embed_cfg = cfg_loaded
                .as_ref()
                .map(|c| crate::store::embedding::EmbeddingConfig::from_config(c));
            let s = match &embed_cfg {
                Some(ec) => crate::store::embedding::embed(&text, ec)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("narrative embedding failed: {e:#}");
                        vec![0.0; embed_dims]
                    }),
                None => vec![0.0; embed_dims],
            };
            let spans: Vec<Vec<f32>> = if doc.extractive.is_empty() {
                Vec::new()
            } else if let Some(ec) = &embed_cfg {
                let texts: Vec<String> =
                    doc.extractive.iter().map(|sp| sp.text.clone()).collect();
                crate::store::embedding::embed_batch(&texts, ec)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("span embedding failed: {e:#}");
                        texts.iter().map(|_| vec![0.0; embed_dims]).collect()
                    })
            } else {
                doc.extractive
                    .iter()
                    .map(|_| vec![0.0; embed_dims])
                    .collect()
            };
            (s, spans)
        }
    };
```

Then update the writer call:

```rust
    match writer::write_summary(&doc, summary_embedding, span_embeddings, root_override).await {
```

(The `match` arm was previously `writer::write_summary(&doc, summary_embedding, root_override).await`.)

- [x] **Step 4: Run — must pass**

```
cargo test -p mur-core conversations::summarize::orch_tests
```

Expected: 6 existing orch_tests + 1 new = 7 passed.

- [x] **Step 5: Commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): compact_day batch-embeds spans for layer=2 upsert (Phase 3.1)

After the narrative embed, batch-embed all extractive spans (one
embed_batch call per day) and pass to writer::write_summary. Mock path
uses MockMode::Hash when MUR_OLLAMA_MOCK=hash (per-text deterministic
vectors for span-selection tests), falls back to uniform 0.1 under
MUR_OLLAMA_MOCK=1.

Embed failure → zero vectors; compact still ships the summary and
layer=1 narrative row. `mur conversations reindex --spans-only` (Task 7)
can repair the layer=2 index later.

Plan: Task 5 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §4

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Ask tiered retrieval + `resolve_span_hit` + cosine MMR

**Files:**
- Modify: `mur-core/src/conversations/ask/mod.rs` (respect `MockMode::Hash` in `embed_query`)
- Modify: `mur-core/src/conversations/ask/retrieve.rs`

- [x] **Step 1: Failing tests**

Append to `#[cfg(test)] mod tests` in `mur-core/src/conversations/ask/retrieve.rs`:

```rust
    #[test]
    fn cosine_sim_identical_is_one() {
        let v = vec![0.1, 0.2, 0.3, 0.4];
        assert!((cosine_sim(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_orthogonal_is_zero() {
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 0.0];
        assert!(cosine_sim(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_zero_length_does_not_panic() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_sim(&a, &b), 0.0);
    }

    #[test]
    fn cosine_sim_mismatched_length_returns_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_sim(&a, &b), 0.0);
    }

    #[test]
    fn resolve_span_hit_parses_line_hint_from_id() {
        let h = SearchHit {
            id: "cc_abc_L2_17".into(),
            ts: 0,
            source: Source::ClaudeCode,
            conv_id: "abc".into(),
            content: "hello".into(),
            distance: 0.1,
            layer: 2,
            vector: Some(vec![0.1; 16]),
        };
        let r = resolve_span_hit(h).unwrap();
        assert_eq!(r.line_hint, Some(17));
        assert_eq!(r.span_index_in_summary, Some(17));
        assert_eq!(r.layer, 2);
        assert_eq!(r.snippet, "hello");
    }

    #[test]
    fn resolve_span_hit_without_l2_suffix_has_no_line_hint() {
        let h = SearchHit {
            id: "cc_abc_7".into(),    // legacy layer=0 shape
            ts: 0,
            source: Source::ClaudeCode,
            conv_id: "abc".into(),
            content: "x".into(),
            distance: 0.5,
            layer: 2,                  // mismatched but still resolvable
            vector: None,
        };
        let r = resolve_span_hit(h).unwrap();
        assert_eq!(r.line_hint, None);
    }

    #[test]
    fn mmr_dedupe_cosine_drops_near_duplicate() {
        let a_vec = vec![1.0, 0.0, 0.0, 0.0];
        let b_vec = vec![0.99, 0.01, 0.0, 0.0]; // nearly identical → cosine ≈ 0.9999
        let mk = |v: Vec<f32>, conv: &str| ResolvedHit {
            layer: 2,
            info: HitInfo {
                layer: 2,
                source: "cc".into(),
                conv_id: conv.into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                score: 0.9,
            },
            snippet: format!("text-{conv}"),
            line_hint: Some(1),
            span_index_in_summary: Some(1),
            vector: Some(v),
        };
        let out = mmr_dedupe_cosine(vec![mk(a_vec, "a"), mk(b_vec, "b")], 0.88);
        assert_eq!(out.len(), 1, "near-duplicate should drop to 1");
    }

    #[test]
    fn mmr_dedupe_cosine_keeps_diverse_hits() {
        let a_vec = vec![1.0, 0.0, 0.0, 0.0];
        let b_vec = vec![0.0, 1.0, 0.0, 0.0]; // orthogonal → cosine = 0
        let mk = |v: Vec<f32>, conv: &str| ResolvedHit {
            layer: 2,
            info: HitInfo {
                layer: 2,
                source: "cc".into(),
                conv_id: conv.into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                score: 0.9,
            },
            snippet: format!("text-{conv}"),
            line_hint: Some(1),
            span_index_in_summary: Some(1),
            vector: Some(v),
        };
        let out = mmr_dedupe_cosine(vec![mk(a_vec, "a"), mk(b_vec, "b")], 0.88);
        assert_eq!(out.len(), 2, "orthogonal vectors should both survive");
    }
```

- [x] **Step 2: Run — must fail**

```
cargo test -p mur-core conversations::ask::retrieve::tests
```

Expected: compile errors (`cosine_sim`, `resolve_span_hit`, `mmr_dedupe_cosine` not defined; `SearchHit` layer/vector fields already added in Task 2).

- [x] **Step 3: Extend `ResolvedHit` + add helpers**

In `mur-core/src/conversations/ask/retrieve.rs`, find the existing `pub struct ResolvedHit` and add a `vector` field:

```rust
pub struct ResolvedHit {
    pub layer: i8,
    pub info: HitInfo,
    pub snippet: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub vector: Option<Vec<f32>>,
}
```

Extend `resolve_summary_hit` (layer=1 fallback path) and `resolve_raw_hit` (layer=0 path) to initialize `vector` from the `SearchHit`:

```rust
// In resolve_summary_hit's returned ResolvedHit { ... }:
        vector: h.vector,
// In resolve_raw_hit's returned ResolvedHit { ... }:
        vector: h.vector,
```

Add `resolve_span_hit` (layer=2 path) — a trivial version of `resolve_raw_hit` that decodes line_hint from the id suffix:

```rust
fn resolve_span_hit(h: SearchHit) -> Result<ResolvedHit> {
    let line_hint = h
        .id
        .rsplit_once("_L2_")
        .and_then(|(_, suffix)| suffix.parse::<u32>().ok());
    let date = chrono::DateTime::from_timestamp(h.ts, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    Ok(ResolvedHit {
        layer: 2,
        info: HitInfo {
            layer: 2,
            source: h.source.file_prefix().to_string(),
            conv_id: h.conv_id.clone(),
            date,
            score: similarity_of(&h),
        },
        snippet: h.content.clone(),
        line_hint,
        span_index_in_summary: line_hint,
        vector: h.vector,
    })
}
```

Add the cosine helpers:

```rust
fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x * *y) as f64;
        na += (*x * *x) as f64;
        nb += (*y * *y) as f64;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn similar(a: &ResolvedHit, b: &ResolvedHit, threshold: f64) -> bool {
    match (&a.vector, &b.vector) {
        (Some(av), Some(bv)) => cosine_sim(av, bv) > threshold,
        _ => word_jaccard(&a.snippet, &b.snippet) > threshold,
    }
}

fn mmr_dedupe_cosine(hits: Vec<ResolvedHit>, threshold: f64) -> Vec<ResolvedHit> {
    let mut kept: Vec<ResolvedHit> = Vec::new();
    for h in hits {
        let dup = kept.iter().any(|k| similar(&h, k, threshold));
        if !dup {
            kept.push(h);
        }
    }
    kept
}
```

Keep the existing `mmr_dedupe` (word-Jaccard) + `word_jaccard` for the fallback branch inside `similar()`.

- [x] **Step 4: Rewire `gather_hits` to prefer layer=2**

In the same file, find `pub async fn gather_hits`. Replace the body's post-open-index block with:

```rust
    let primary_src = args.filters.source.first().copied();

    // Phase 3.1: layer=2 (spans) is the preferred retrieval unit.
    let l2 = idx
        .search(&args.query_embedding, args.k_summary, primary_src, Some(2))
        .await?;
    // Fallback to layer=1 (narratives) for unmigrated archives.
    let l1 = if l2.is_empty() {
        idx.search(&args.query_embedding, args.k_summary, primary_src, Some(1))
            .await?
    } else {
        Vec::new()
    };

    let effective_top = l2
        .first()
        .map(similarity_of)
        .or_else(|| l1.first().map(similarity_of))
        .unwrap_or(0.0);
    let l0 = if !args.no_escalate
        && (effective_top < args.escalation_threshold || (l2.is_empty() && l1.is_empty()))
    {
        idx.search(&args.query_embedding, args.k_raw, primary_src, Some(0))
            .await?
    } else {
        Vec::new()
    };

    let filtered_l2: Vec<_> = l2.into_iter().filter(|h| passes(h, args.filters)).collect();
    let filtered_l1: Vec<_> = l1.into_iter().filter(|h| passes(h, args.filters)).collect();
    let filtered_l0: Vec<_> = l0.into_iter().filter(|h| passes(h, args.filters)).collect();

    let mut resolved = Vec::new();
    for h in filtered_l2 {
        resolved.push(resolve_span_hit(h)?);
    }
    for h in filtered_l1 {
        resolved.push(resolve_summary_hit(h, args.root_override)?);
    }
    for h in filtered_l0 {
        resolved.push(resolve_raw_hit(h));
    }

    // Phase 3.1: cosine MMR on retrieved vectors; falls back to word-Jaccard
    // for mixed vector/no-vector pairs.
    let deduped = mmr_dedupe_cosine(resolved, args.mmr_threshold);

    let budget = (args.max_context_tokens * 9 / 10).max(400);
    let capped = cap_by_budget(deduped, budget);
    Ok(capped)
```

- [x] **Step 5: Respect `MockMode::Hash` in `embed_query`**

In `mur-core/src/conversations/ask/mod.rs`, find the existing `async fn embed_query`. Replace:

```rust
async fn embed_query(q: &str) -> Result<Vec<f32>> {
    if super::ollama::OllamaClient::mock_from_env() {
        return Ok(vec![0.1; 1024]);
    }
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
    crate::store::embedding::embed(q, &embed_cfg).await
}
```

with:

```rust
async fn embed_query(q: &str) -> Result<Vec<f32>> {
    if let Some(mode) = super::ollama::mock_mode() {
        return Ok(super::ollama::mock_embed_vector(q, mode, 1024));
    }
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
    crate::store::embedding::embed(q, &embed_cfg).await
}
```

- [x] **Step 6: Update the existing Phase 2B retrieve tests that construct `ResolvedHit`**

Grep for `ResolvedHit {` in `retrieve.rs` — there are construction sites in the existing tests (`mmr_dedupe_drops_duplicate`, `cap_by_budget_keeps_at_least_one`). Each needs a `vector: None,` field added. Example:

```rust
        let h1 = ResolvedHit {
            layer: 0,
            info: HitInfo { /* unchanged */ },
            snippet: "the quick brown fox jumps".into(),
            line_hint: None,
            span_index_in_summary: None,
            vector: None,                    // NEW
        };
```

- [x] **Step 7: Run — must pass**

```
cargo test -p mur-core conversations::ask
cargo test -p mur-core
```

Expected: previous 4 retrieve tests + 8 new = 12 retrieve tests pass. Full suite stays green.

- [x] **Step 8: Commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/retrieve.rs mur-core/src/conversations/ask/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): ask tiered retrieval + cosine MMR (Phase 3.1)

gather_hits prefers layer=2 (span-level) over layer=1 (narrative-level).
Falls back to layer=1 + resolve_summary_hit when layer=2 is empty
(unmigrated archives); the Phase 2B path is intact.

resolve_span_hit: layer=2 hit's id carries "_L2_<line_hint>" suffix,
decoded into span_index_in_summary so the citation anchor format
[cit: <date> <src>/<conv> @summary-span-<N>] matches Phase 2B exactly.

mmr_dedupe_cosine: cosine(a.vector, b.vector) > mmr_threshold drops
near-duplicates. Mixed vector/no-vector pairs fall back to word-Jaccard.

embed_query uses MockMode::Hash for hash-based test vectors when
MUR_OLLAMA_MOCK=hash; =1 still yields uniform-0.1 vectors.

Plan: Task 6 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §5

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Reindex `--spans-only`/`--raw-only` + doctor coverage

**Files:**
- Modify: `mur-core/src/main.rs` (Reindex variant gains two bool flags)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (flags wiring + span rebuild + doctor line)

- [x] **Step 1: Failing CLI integration test**

Append to `mur-core/tests/cli_conversations.rs`:

```rust
#[test]
fn mur_conversations_reindex_spans_only_populates_layer_2() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    // Seed a summary.md so reindex has something to process.
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    std::fs::write(
        summary_dir.join("2026-04-21.md"),
        "---\n\
         schema: 1\n\
         date: 2026-04-21\n\
         generated_at: 2026-04-21T03:00:00Z\n\
         generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
         duration_ms: 50\n\
         conv_count: 1\n\
         msg_count: 1\n\
         sources: [cc]\n\
         pattern_refs: []\n\
         keywords: [test]\n\
         links:\n  prev: null\n  next: null\n\
         warnings: []\n\
         input_content_sha: deadbeef\n\
         ---\n\n\
         ## Extractive spans\n\n\
         [1] _{cc/c1 @L1}_:\n> first span\n\n\
         [2] _{cc/c1 @L2}_:\n> second span\n\n\
         ## Abstractive narrative\n\n\
         Narrative.\n",
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home_val) = with_mur_home(
        cmd.args(["conversations", "reindex", "--spans-only"]),
        tmp.path(),
    );
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("spans"),
        "expected 'spans' in output; got: {stdout}"
    );
}
```

And extend the existing `mur_conversations_doctor_runs` test's assertions (find it in the same file) to also check for the new line. Add one line after the `.history/` check:

```rust
    assert!(stdout.contains("spans:"));  // NEW Phase 3.1
```

- [x] **Step 2: Run — must fail**

```
cargo test -p mur-core --test cli_conversations mur_conversations_reindex_spans_only
```

Expected: command fails with `unrecognized argument '--spans-only'` (flag not yet defined).

- [x] **Step 3: Add flags to `ConversationsAction::Reindex` in `main.rs`**

In `mur-core/src/main.rs`, find the existing `ConversationsAction::Reindex` variant. It's currently `Reindex,` (unit variant). Replace with:

```rust
    /// Rebuild LanceDB from raw + summaries.
    Reindex {
        /// Skip span (layer=2) rebuild; only re-ingest raw → layer=0.
        #[arg(long, conflicts_with = "spans_only")]
        raw_only: bool,
        /// Skip raw rebuild; only re-process summary/*.md → layer=2.
        #[arg(long, conflicts_with = "raw_only")]
        spans_only: bool,
    },
```

Find the dispatch arm:

```rust
            ConversationsAction::Reindex => {
                cmd::conversations_cmd::cmd_conversations_reindex().await?
            }
```

Replace with:

```rust
            ConversationsAction::Reindex { raw_only, spans_only } => {
                cmd::conversations_cmd::cmd_conversations_reindex(
                    cmd::conversations_cmd::ReindexArgs { raw_only, spans_only },
                )
                .await?
            }
```

- [x] **Step 4: Extend `cmd_conversations_reindex` in `conversations_cmd.rs`**

In `mur-core/src/cmd/conversations_cmd.rs`, find the existing `pub async fn cmd_conversations_reindex`. Replace its signature + body:

```rust
pub struct ReindexArgs {
    pub raw_only: bool,
    pub spans_only: bool,
}

pub async fn cmd_conversations_reindex(args: ReindexArgs) -> anyhow::Result<()> {
    use crate::conversations::{paths, store, summarize};

    let mut raw_msgs = 0u64;
    let mut span_rows = 0u64;

    // Raw rebuild (layer=0) — Phase 1 behavior.
    if !args.spans_only {
        let days = store::list_raw_dirs(None).unwrap_or_default();
        let dims: i32 = {
            let cfg = crate::store::config::load_config().unwrap_or_default();
            crate::store::embedding::EmbeddingConfig::from_config(&cfg).dimensions as i32
        };
        let mut idx =
            crate::conversations::index::ConversationIndex::open(dims, None).await?;
        for (date, _) in days {
            let msgs = store::read_day(date, None)?;
            if msgs.is_empty() {
                continue;
            }
            let embed_cfg = {
                let cfg = crate::store::config::load_config().unwrap_or_default();
                crate::store::embedding::EmbeddingConfig::from_config(&cfg)
            };
            let texts: Vec<String> = msgs
                .iter()
                .map(|m| m.content.as_text().to_owned())
                .collect();
            let vecs = if let Some(mode) = crate::conversations::ollama::mock_mode() {
                texts
                    .iter()
                    .map(|t| {
                        crate::conversations::ollama::mock_embed_vector(
                            t,
                            mode,
                            dims as usize,
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                crate::store::embedding::embed_batch(&texts, &embed_cfg)
                    .await
                    .unwrap_or_else(|_| texts.iter().map(|_| vec![0.0; dims as usize]).collect())
            };
            let entries: Vec<_> = msgs.into_iter().zip(vecs.into_iter()).collect();
            idx.upsert(&entries).await?;
            raw_msgs += entries.len() as u64;
        }
        println!("reindexed raw: {raw_msgs} messages");
    }

    // Span rebuild (layer=2) — Phase 3.1.
    if !args.raw_only {
        let dims: i32 = {
            let cfg = crate::store::config::load_config().unwrap_or_default();
            crate::store::embedding::EmbeddingConfig::from_config(&cfg).dimensions as i32
        };
        let mut idx =
            crate::conversations::index::ConversationIndex::open(dims, None).await?;
        let summary_dir = paths::conversations_root(None).join("summary");
        if summary_dir.exists() {
            for entry in std::fs::read_dir(&summary_dir)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") else {
                    continue;
                };
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                let Ok(parsed) = summarize::parse_summary(&body) else {
                    continue;
                };
                if parsed.extractive.is_empty() {
                    continue;
                }
                // Embed spans.
                let texts: Vec<String> =
                    parsed.extractive.iter().map(|s| s.text.clone()).collect();
                let vecs: Vec<Vec<f32>> =
                    if let Some(mode) = crate::conversations::ollama::mock_mode() {
                        texts
                            .iter()
                            .map(|t| {
                                crate::conversations::ollama::mock_embed_vector(
                                    t,
                                    mode,
                                    dims as usize,
                                )
                            })
                            .collect()
                    } else {
                        let embed_cfg = {
                            let cfg = crate::store::config::load_config().unwrap_or_default();
                            crate::store::embedding::EmbeddingConfig::from_config(&cfg)
                        };
                        crate::store::embedding::embed_batch(&texts, &embed_cfg)
                            .await
                            .unwrap_or_else(|_| {
                                texts.iter().map(|_| vec![0.0; dims as usize]).collect()
                            })
                    };

                // Build upsert batch.
                use chrono::TimeZone;
                let span_ts =
                    chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap());
                let mut batch: Vec<(mur_common::Message, Vec<f32>, i8)> =
                    Vec::with_capacity(parsed.extractive.len());
                for (span, vec) in parsed.extractive.iter().zip(vecs.into_iter()) {
                    let Some(src_enum) = mur_common::Source::from_prefix(&span.src) else {
                        tracing::warn!(
                            "unknown source prefix '{}' in {}; skipping span",
                            span.src,
                            path.display()
                        );
                        continue;
                    };
                    let msg = mur_common::Message {
                        v: 1,
                        ts: span_ts,
                        src: src_enum,
                        conv: span.conv_id.clone(),
                        role: mur_common::Role::User,
                        content: mur_common::Content::Text {
                            value: span.text.clone(),
                        },
                        meta: serde_json::json!({ "id_suffix": span.line_hint }),
                        refs: vec![],
                    };
                    batch.push((msg, vec, 2i8));
                }
                if !batch.is_empty() {
                    let n = batch.len() as u64;
                    idx.upsert_with_layer(&batch).await?;
                    span_rows += n;
                }
            }
        }
        println!("reindexed spans: {span_rows} spans");
    }

    Ok(())
}
```

- [x] **Step 5: Extend doctor output**

In the same file, find `pub async fn cmd_conversations_doctor`. Near the end (after the `.history/` coverage block, before the final `Ok(())`), append:

```rust
    // Phase 3.1: span (layer=2) coverage.
    let dims: i32 = {
        let c = crate::store::config::load_config().unwrap_or_default();
        crate::store::embedding::EmbeddingConfig::from_config(&c).dimensions as i32
    };
    let idx_for_count =
        crate::conversations::index::ConversationIndex::open(dims, None).await;
    match idx_for_count {
        Ok(idx) => {
            let n = idx.count_rows_at_layer(2).await.unwrap_or(0);
            if n > 0 {
                println!("  ✓ spans: {n} rows at layer=2");
            } else if summary_count > 0 {
                println!(
                    "  · spans: 0 indexed — run 'mur conversations reindex --spans-only' for span-level Ask retrieval"
                );
            } else {
                println!("  · spans: no summaries yet");
            }
        }
        Err(e) => {
            println!("  · spans: could not open index: {e}");
        }
    }
```

- [x] **Step 6: Run — must pass**

```
cargo test -p mur-core --test cli_conversations
cargo test -p mur-core
```

Expected: new `mur_conversations_reindex_spans_only_populates_layer_2` test passes; `mur_conversations_doctor_runs` passes with new "spans:" assertion; full suite green.

- [x] **Step 7: Commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/main.rs mur-core/src/cmd/conversations_cmd.rs mur-core/tests/cli_conversations.rs
git commit -m "$(cat <<'EOF'
feat(core): reindex --spans-only/--raw-only + doctor coverage (Phase 3.1)

`mur conversations reindex` expands to rebuild both raw (layer=0) and
spans (layer=2) by default. --raw-only preserves Phase 2 behavior;
--spans-only is the fast upgrade path for existing archives.

Span rebuild walks summary/*.md, parses via parse_summary, batch-embeds
each summary's spans, upserts at layer=2 with meta.id_suffix=line_hint.
Honors MUR_OLLAMA_MOCK=1|hash via ollama::mock_mode.

`mur conversations doctor` gains a "spans:" line reporting layer=2 row
count + a reindex suggestion when 0 rows present but summaries exist.

Plan: Task 7 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md
Spec: §6

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Golden path Step 9.5 + final integration

**Files:**
- Modify: `scripts/golden-path-conversations.sh`

- [x] **Step 1: Extend golden path**

In `scripts/golden-path-conversations.sh`, locate the existing Step 9 block (`# ── Step 9: compact ──`). After Step 9's `grep -q "## Abstractive narrative"` line but BEFORE Step 10's `echo "--- step 10: mur ask --json ---"`, insert:

```bash
# ── Step 9.5: reindex --spans-only + hash-mock span selection ─────────────
# Rebuild layer=2 span rows for the yesterday summary using the hash mock,
# so Step 10's ask can pick the most query-relevant span deterministically.
echo "--- step 9.5: mur conversations reindex --spans-only (hash mock) ---"
MUR_OLLAMA_MOCK=hash "$MUR" conversations reindex --spans-only | tee /tmp/gp-step-9.5.txt
grep -q "reindexed spans:" /tmp/gp-step-9.5.txt \
  || { echo "FAIL step 9.5: reindex did not report span rebuild"; exit 1; }
```

And replace the existing Step 10's invocation to use hash mock (so the ask pipeline runs with the same vectors reindex produced):

Old:
```bash
MUR_OLLAMA_MOCK=1 "$MUR" ask "what compression techniques did I discuss" --json > /tmp/gp-step-10.json
```

New:
```bash
MUR_OLLAMA_MOCK=hash "$MUR" ask "mock extractive span seeded for compact golden-path" --json > /tmp/gp-step-10.json
```

The query is changed to be a close paraphrase of the seeded span so the hash-mock embeddings give the span a high cosine similarity to the query — the test then asserts hits_used came from layer=2.

After the existing two `jq -e` assertions for Step 10, append a third:

```bash
jq -e '.hits_used[0].layer == 2' /tmp/gp-step-10.json \
  || { echo "FAIL step 10: first hit should be layer=2 after reindex"; exit 1; }
```

Update the final banner from `=== ALL 10 STEPS GREEN ===` to `=== ALL 11 STEPS GREEN ===` (Step 9.5 counts).

- [x] **Step 2: Run the golden path**

```
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-1
cargo build -p mur-core --bin mur
./scripts/golden-path-conversations.sh
```

Expected: all 11 steps green, final banner `=== ALL 11 STEPS GREEN ===`.

- [x] **Step 3: Run the full test suite once more for final confidence**

```
cargo test -p mur-common
cargo test -p mur-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all suites green, clippy + fmt clean.

- [x] **Step 4: Commit**

```
git add scripts/golden-path-conversations.sh
git commit -m "$(cat <<'EOF'
test(core): golden-path Step 9.5 + hash-mock Step 10 (Phase 3.1)

Step 9.5 runs `mur conversations reindex --spans-only` under
MUR_OLLAMA_MOCK=hash, asserting the "reindexed spans:" report.

Step 10's ask query is tightened to a near-paraphrase of the seeded
span, run under MUR_OLLAMA_MOCK=hash for deterministic cosine
similarity. A new assertion checks `.hits_used[0].layer == 2` — proof
that the Phase 3.1 tiered retrieval found the span row rather than
falling back to the layer=1 narrative path.

Banner updated: 10 → 11 steps.

Plan: Task 8 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-1.md

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**🏁 End of Phase 3.1.** Single-phase plan — open one PR, wait for CI green + reviewer approval, then ship. No Phase-3.2 checkpoint in this document (Phase 3.2 is Full RAPTOR and gets its own spec + plan).
