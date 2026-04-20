# mur Sources — Phase 1.2 Obsidian Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the first real `KnowledgeSource` adapter (Obsidian), replace the P1.1 stubs in `LanceDbStore` with working `upsert` / `search` / `delete` / `list_external_ids` / `count`, and deliver a minimum-viable end-to-end loop: `mur source add obsidian` → `mur source sync` → `mur source search` returns hits from the vault.

**Architecture:** Obsidian adapter walks the user's vault (`*.md` files, `.obsidian/` + `.trash/` + user-excluded folders skipped), reads each file, parses YAML frontmatter, emits markdown-heading-aware chunks, and the sync orchestrator embeds them and upserts into a new LanceDB `sources` table. Deletion is detected each sync via a set-diff between the adapter's current `external_id` set and `VectorStore::list_external_ids(source_id)`. Unified retrieve across patterns + sources is deferred to P1.3; P1.2 adds only a minimal `mur source search` command that directly queries the sources table.

**Tech Stack:** Rust edition 2024, Tokio, async-trait, anyhow, LanceDB (existing), pulldown-cmark (new, markdown parser), walkdir (existing), serde_yaml (existing), tracing (existing). File watcher is NOT in scope for P1.2 (deferred to P1.4).

**Spec reference:** `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md` §6, §7.1, §8 (scoping wider), §11 (P1.2 line), §9.1 (per-adapter tests).

**Depends on:** P1.1 foundation branch (`feat/sources-p1.1`). Start P1.2 from P1.1 HEAD (post-merge to main, or branch off P1.1).

---

## File Structure

P1.2 touches these files:

```
mur-core/
  Cargo.toml                                # MODIFY: + pulldown-cmark
  src/
    store/vector/
      lancedb.rs                            # MODIFY: replace 5 trait stubs with real impls; add sources-table helpers
    store/vector/tests.rs                   # MODIFY: un-ignore upsert/delete conformance tests
    sources/
      mod.rs                                # MODIFY: + pub mod adapters; + pub mod chunker; + pub mod sync;
      chunker/
        mod.rs                              # NEW: chunker namespace
        markdown.rs                         # NEW: heading-aware markdown chunker
      adapters/
        mod.rs                              # NEW: adapter namespace
        obsidian.rs                         # NEW: ObsidianAdapter impl of KnowledgeSource
      sync.rs                               # NEW: sync orchestrator (sync_source + types)
    cmd/
      source_cmd.rs                         # MODIFY: replace every `bail!` with real handler
      mod.rs                                # NO CHANGE (source_cmd already registered in P1.1)
  tests/
    obsidian_e2e.rs                         # NEW: end-to-end integration test
```

**Design choices**:
- LanceDB `sources` table lives alongside `patterns` inside the same `~/.mur/index/` connection. Table name is the string `"sources"` — distinct from the pattern table `"patterns"` (existing).
- Upsert strategy is **delete-by-chunk_id then insert** (simpler than `merge_insert`; the difference is micro-performance not correctness).
- Cursor for Obsidian is an **RFC3339 timestamp string** stored in `SourceInstance.sync.last_cursor`. First sync uses `None`, subsequent sync uses `max(updated_at)` of the previous run's returned docs.
- `external_id` for Obsidian is the **relative path from vault root** (`notes/ideas/foo.md`).
- The MVP sources-only search goes under `mur source search <query>` (a new verb). It does NOT touch the existing `mur search` code path — that integration is P1.3.

---

## Task 0: Prep — Verify Baseline

**Files:** None (verification).

- [ ] **Step 1: Confirm worktree/branch state**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.1 || cd <wherever-p1.1-branch-lives>
git branch --show-current
git log --oneline -3
```

If P1.1 has been merged to main, branch from main:
```bash
git checkout main
git pull
git worktree add .worktrees/sources-p1.2 -b feat/sources-p1.2 main
cd .worktrees/sources-p1.2
```

Otherwise continue on `feat/sources-p1.1` (P1.2 builds directly on P1.1 commits).

- [ ] **Step 2: Baseline green**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

Record counts. P1.1 baseline was 1246 passing. Everything at the start of P1.2 should pass. If tests fail, STOP and investigate.

---

## Task 1: LanceDB `sources` Table Schema + Openers

**Files:**
- Modify: `mur-core/src/store/vector/lancedb.rs`

- [ ] **Step 1: Write a failing test asserting the sources table can be opened/created**

Inside the existing `#[cfg(test)] mod tests { ... }` block at the bottom of `mur-core/src/store/vector/lancedb.rs`, APPEND:

```rust
    #[tokio::test]
    async fn open_or_create_sources_table_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
        // First call creates
        store.ensure_sources_table().await.unwrap();
        // Second call is a no-op
        store.ensure_sources_table().await.unwrap();
        // Row count zero
        let c = <LanceDbStore as VectorStore>::count(&store, None).await.unwrap();
        assert_eq!(c, 0);
    }
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test -p mur-core store::vector::lancedb::tests::open_or_create_sources_table_is_idempotent
```

Expected: compile error (`ensure_sources_table` not found).

- [ ] **Step 3: Add the schema and helper methods**

Still in `mur-core/src/store/vector/lancedb.rs`, inside the `impl LanceDbStore { ... }` inherent block (NOT the trait impl), ADD these methods. Place after the existing `search(..., item_type)` method and before the `fn schema(...)` helper:

```rust
/// Name of the LanceDB table that stores source chunks (separate from `patterns`).
pub const SOURCES_TABLE: &str = "sources";

/// Arrow schema for the sources table.
pub fn sources_schema(dimensions: i32) -> Schema {
    Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("source_id", DataType::Utf8, false),
        Field::new("external_id", DataType::Utf8, false),
        Field::new("ordinal", DataType::UInt64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("heading_path", DataType::Utf8, false), // JSON-encoded array
        Field::new("char_start", DataType::UInt64, false),
        Field::new("char_end", DataType::UInt64, false),
        Field::new("updated_at_ms", DataType::Int64, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dimensions,
            ),
            false,
        ),
    ])
}

impl LanceDbStore {
    /// Create the `sources` table if it doesn't exist. Idempotent.
    pub async fn ensure_sources_table(&self) -> Result<()> {
        let tables = self.db.table_names().execute().await?;
        if tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(());
        }
        let schema = sources_schema(self.dimensions);
        // Empty RecordBatchIterator to create a table with the schema.
        let empty: Vec<std::result::Result<RecordBatch, arrow_schema::ArrowError>> = Vec::new();
        let reader = RecordBatchIterator::new(empty, Arc::new(schema));
        self.db
            .create_table(SOURCES_TABLE, Box::new(reader) as Box<dyn arrow_array::RecordBatchReader + Send>)
            .execute()
            .await
            .context("creating sources table")?;
        Ok(())
    }
}
```

**IMPORTANT**: the `sources_schema` function + `SOURCES_TABLE` constant are file-level items (not inside any `impl`). `ensure_sources_table` lives in the same `impl LanceDbStore { ... }` block that already holds `open`, `build_index`, etc.

If the `RecordBatchIterator::new(empty, schema)` call fails to typecheck (e.g. generic inference issues with empty `Vec`), annotate the element type explicitly — the turbofish form `Vec::<std::result::Result<RecordBatch, arrow_schema::ArrowError>>::new()` should satisfy the inference.

- [ ] **Step 4: Run the test**

```bash
cargo test -p mur-core store::vector::lancedb::tests::open_or_create_sources_table_is_idempotent
```

Expected: PASS.

Also run the full store::vector test module to confirm no regression:
```bash
cargo test -p mur-core store::vector
```

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/store/vector/lancedb.rs
git commit -m "feat(vector): LanceDB sources table schema + ensure_sources_table helper"
```

---

## Task 2: `LanceDbStore::upsert` Real Implementation

**Files:**
- Modify: `mur-core/src/store/vector/lancedb.rs`

Strategy: delete-then-insert. For each batch of chunks, we (a) delete any rows with the same `chunk_id`, then (b) append the new rows. `chunk_id` is UUIDv4 so collisions on re-upsert are rare; the delete step handles the re-sync case where the adapter recomputes chunks for a modified document (different `chunk_id` but same `external_id` — the `external_id`-level reconciliation happens in `delete_by_external_ids` called BEFORE upsert in the sync flow).

- [ ] **Step 1: Write failing roundtrip test**

At the bottom of the existing tests module in `mur-core/src/store/vector/lancedb.rs`, APPEND:

```rust
    #[tokio::test]
    async fn sources_upsert_and_search_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
        store.ensure_sources_table().await.unwrap();

        let now = chrono::Utc::now();
        let mk_chunk = |id: &str, ext: &str, text: &str, embed: Vec<f32>| -> super::EmbeddedChunk {
            super::EmbeddedChunk {
                chunk_id: id.into(),
                source_id: "obsidian:test".into(),
                external_id: ext.into(),
                ordinal: 0,
                text: text.into(),
                heading_path: vec!["Section".into()],
                char_range: (0, text.len()),
                updated_at: now,
                embedding: embed,
            }
        };

        let v_a: Vec<f32> = (0..TEST_DIM as usize).map(|i| (i as f32 * 0.01).sin()).collect();
        let v_b: Vec<f32> = (0..TEST_DIM as usize).map(|i| (i as f32 * 0.01).cos()).collect();

        <LanceDbStore as super::VectorStore>::upsert(
            &store,
            &[mk_chunk("c1", "doc-a", "alpha text", v_a.clone())],
        )
        .await
        .unwrap();
        <LanceDbStore as super::VectorStore>::upsert(
            &store,
            &[mk_chunk("c2", "doc-b", "bravo text", v_b.clone())],
        )
        .await
        .unwrap();

        // Search by v_a should return c1 (closer) first.
        let hits = <LanceDbStore as super::VectorStore>::search(
            &store,
            &v_a,
            5,
            &super::SearchFilter::default(),
        )
        .await
        .unwrap();
        assert!(!hits.is_empty(), "expected hits");
        assert_eq!(hits[0].chunk_id, "c1");
        assert_eq!(hits[0].source_id, "obsidian:test");
        assert_eq!(hits[0].external_id, "doc-a");
        assert_eq!(hits[0].heading_path, vec!["Section".to_string()]);
    }
```

- [ ] **Step 2: Run test — expect FAIL**

```bash
cargo test -p mur-core store::vector::lancedb::tests::sources_upsert_and_search_roundtrip
```

Expected: PANIC / bail — `LanceDbStore::upsert is a stub` (because the trait method still bails).

- [ ] **Step 3: Replace the `upsert` stub with a real implementation**

In the `#[async_trait] impl VectorStore for LanceDbStore { ... }` block, REPLACE the entire `upsert` method body with:

```rust
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        self.ensure_sources_table().await?;

        // Delete any existing rows with these chunk_ids (idempotent upsert).
        let ids: Vec<String> = chunks.iter().map(|c| format!("'{}'", c.chunk_id.replace('\'', "''"))).collect();
        let predicate = format!("chunk_id IN ({})", ids.join(","));
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        // `delete` is OK to call even when no rows match; it won't error.
        let _ = table.delete(&predicate).await;

        // Build the columns.
        let chunk_ids: Vec<&str> = chunks.iter().map(|c| c.chunk_id.as_str()).collect();
        let source_ids: Vec<&str> = chunks.iter().map(|c| c.source_id.as_str()).collect();
        let external_ids: Vec<&str> = chunks.iter().map(|c| c.external_id.as_str()).collect();
        let ordinals: Vec<u64> = chunks.iter().map(|c| c.ordinal as u64).collect();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let heading_paths: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.heading_path).unwrap_or_else(|_| "[]".into()))
            .collect();
        let heading_path_refs: Vec<&str> = heading_paths.iter().map(|s| s.as_str()).collect();
        let char_starts: Vec<u64> = chunks.iter().map(|c| c.char_range.0 as u64).collect();
        let char_ends: Vec<u64> = chunks.iter().map(|c| c.char_range.1 as u64).collect();
        let updated_at_ms: Vec<i64> = chunks
            .iter()
            .map(|c| c.updated_at.timestamp_millis())
            .collect();

        // Build FixedSizeList of vectors.
        let all_vectors: Vec<f32> = chunks.iter().flat_map(|c| c.embedding.clone()).collect();
        let values = Float32Array::from(all_vectors);
        let item_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array =
            FixedSizeListArray::new(item_field, self.dimensions, Arc::new(values), None);

        let schema = sources_schema(self.dimensions);
        use arrow_array::{Int64Array, UInt64Array};
        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![
                Arc::new(StringArray::from(chunk_ids)),
                Arc::new(StringArray::from(source_ids)),
                Arc::new(StringArray::from(external_ids)),
                Arc::new(UInt64Array::from(ordinals)),
                Arc::new(StringArray::from(texts)),
                Arc::new(StringArray::from(heading_path_refs)),
                Arc::new(UInt64Array::from(char_starts)),
                Arc::new(UInt64Array::from(char_ends)),
                Arc::new(Int64Array::from(updated_at_ms)),
                Arc::new(vector_array),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(schema));
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
        table.add(reader).execute().await?;
        Ok(())
    }
```

You may need to tweak imports at the top of the file:
- Ensure `arrow_array::{Int64Array, UInt64Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray}` are all imported.
- `arrow_schema::{DataType, Field, Schema}` should already be imported.

If LanceDB's `table.delete(predicate)` signature doesn't match (the 0.26.x API may have changed), consult the `lancedb` crate source and adapt. The conventional signature is `async fn delete(&self, predicate: &str) -> Result<()>`.

- [ ] **Step 4: Run the test**

```bash
cargo test -p mur-core store::vector::lancedb::tests::sources_upsert_and_search_roundtrip
```

Expected: will still FAIL because `search` is also a stub. That's OK — the upsert half works, the assertion trips on `search`. Move on to Task 3.

To verify upsert alone succeeds, run:
```bash
cargo test -p mur-core store::vector::lancedb::tests::open_or_create_sources_table_is_idempotent
```
Still passes. Good.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/store/vector/lancedb.rs
git commit -m "feat(vector): LanceDbStore::upsert real impl (delete-then-insert)"
```

---

## Task 3: `LanceDbStore::search` (trait method) Real Implementation

**Files:**
- Modify: `mur-core/src/store/vector/lancedb.rs`

- [ ] **Step 1: Replace the `search` trait-method stub**

In the `impl VectorStore for LanceDbStore` block, REPLACE the `search` method body with:

```rust
    async fn search(
        &self,
        query_vec: &[f32],
        k: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<Hit>> {
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(vec![]);
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;

        let mut query = table.vector_search(query_vec.to_vec()).context("vector_search")?;

        // Build WHERE predicate from filter.
        let mut predicates: Vec<String> = Vec::new();
        if let Some(ids) = &filter.source_ids
            && !ids.is_empty()
        {
            let escaped: Vec<String> = ids
                .iter()
                .map(|s| format!("'{}'", s.replace('\'', "''")))
                .collect();
            predicates.push(format!("source_id IN ({})", escaped.join(",")));
        }
        if let Some(since) = filter.since {
            predicates.push(format!("updated_at_ms >= {}", since.timestamp_millis()));
        }
        if !predicates.is_empty() {
            query = query.only_if(predicates.join(" AND "));
        }

        let results = query
            .limit(k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut hits: Vec<Hit> = Vec::new();
        for batch in &results {
            use arrow_array::{Int64Array, StringArray};
            let chunk_ids = batch
                .column_by_name("chunk_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column chunk_id missing or wrong type")?;
            let source_ids = batch
                .column_by_name("source_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column source_id missing")?;
            let external_ids = batch
                .column_by_name("external_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column external_id missing")?;
            let texts = batch
                .column_by_name("text")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column text missing")?;
            let heading_paths = batch
                .column_by_name("heading_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column heading_path missing")?;
            let updated_at_ms = batch
                .column_by_name("updated_at_ms")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .context("column updated_at_ms missing")?;
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .context("column _distance missing")?;

            for i in 0..batch.num_rows() {
                let d = distances.value(i);
                // LanceDB default distance is L2 squared; similarity = 1/(1+d).
                let score = 1.0 / (1.0 + d);
                let hp: Vec<String> = serde_json::from_str(heading_paths.value(i)).unwrap_or_default();
                let ms = updated_at_ms.value(i);
                let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                    .unwrap_or_else(|| chrono::Utc::now());
                hits.push(Hit {
                    chunk_id: chunk_ids.value(i).to_string(),
                    source_id: source_ids.value(i).to_string(),
                    external_id: external_ids.value(i).to_string(),
                    score,
                    text: texts.value(i).to_string(),
                    heading_path: hp,
                    updated_at: ts,
                });
            }
        }
        Ok(hits)
    }
```

This uses Rust 2024's `let ... && ...` chained conditions which are already used elsewhere in this crate.

- [ ] **Step 2: Run the roundtrip test**

```bash
cargo test -p mur-core store::vector::lancedb::tests::sources_upsert_and_search_roundtrip
```

Expected: PASS. The inserted `c1` chunk with vector `v_a` should be the top hit when searched with `v_a`.

- [ ] **Step 3: Also verify conformance smoke still passes**

```bash
cargo test -p mur-core store::vector
```

Expected: all non-ignored tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/store/vector/lancedb.rs
git commit -m "feat(vector): LanceDbStore::search real impl with source_id/since filter"
```

---

## Task 4: `LanceDbStore::list_external_ids` + `count` Real Implementations

**Files:**
- Modify: `mur-core/src/store/vector/lancedb.rs`

- [ ] **Step 1: Failing test**

Append to the tests module:

```rust
    #[tokio::test]
    async fn sources_list_external_ids_and_count_work() {
        let tmp = TempDir::new().unwrap();
        let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
        let now = chrono::Utc::now();
        let zeros = vec![0.0_f32; TEST_DIM as usize];

        let chunks: Vec<super::EmbeddedChunk> = (0..3)
            .map(|i| super::EmbeddedChunk {
                chunk_id: format!("cid-{i}"),
                source_id: "obsidian:test".into(),
                external_id: format!("doc-{i}"),
                ordinal: 0,
                text: "x".into(),
                heading_path: vec![],
                char_range: (0, 1),
                updated_at: now,
                embedding: zeros.clone(),
            })
            .collect();

        <LanceDbStore as super::VectorStore>::upsert(&store, &chunks).await.unwrap();

        let ids = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "obsidian:test")
            .await
            .unwrap();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["doc-0", "doc-1", "doc-2"]);

        let all = <LanceDbStore as super::VectorStore>::count(&store, None).await.unwrap();
        assert_eq!(all, 3);

        let scoped = <LanceDbStore as super::VectorStore>::count(&store, Some("obsidian:test"))
            .await
            .unwrap();
        assert_eq!(scoped, 3);

        let other =
            <LanceDbStore as super::VectorStore>::count(&store, Some("nope")).await.unwrap();
        assert_eq!(other, 0);
    }
```

- [ ] **Step 2: Run to confirm FAIL (bail)**

```bash
cargo test -p mur-core store::vector::lancedb::tests::sources_list_external_ids_and_count_work
```

- [ ] **Step 3: Replace `list_external_ids` body**

In `impl VectorStore for LanceDbStore`, REPLACE `list_external_ids` body:

```rust
    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>> {
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(vec![]);
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let batches = table
            .query()
            .only_if(format!(
                "source_id = '{}'",
                source_id.replace('\'', "''")
            ))
            .select(lancedb::query::Select::Columns(vec!["external_id".to_string()]))
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        use arrow_array::StringArray;
        let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for batch in &batches {
            let col = batch
                .column_by_name("external_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column external_id missing")?;
            for i in 0..batch.num_rows() {
                set.insert(col.value(i).to_string());
            }
        }
        Ok(set.into_iter().collect())
    }
```

- [ ] **Step 4: Replace `count` body**

```rust
    async fn count(&self, source_id: Option<&str>) -> Result<usize> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(0);
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let total = match source_id {
            None => table.count_rows(None).await?,
            Some(sid) => {
                table
                    .count_rows(Some(format!(
                        "source_id = '{}'",
                        sid.replace('\'', "''")
                    )))
                    .await?
            }
        };
        Ok(total)
    }
```

**Note**: if `Table::count_rows` takes a different arg shape in lancedb 0.26.x (e.g. `count_rows(&self, predicate: Option<String>)` vs `Option<&str>`), adjust to the actual signature. The conventional form accepts `Option<String>` for a predicate; a `None` counts all rows.

- [ ] **Step 5: Run test**

```bash
cargo test -p mur-core store::vector::lancedb::tests::sources_list_external_ids_and_count_work
```

Expected: PASS.

Also re-run the full store::vector module to catch regressions:
```bash
cargo test -p mur-core store::vector
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/store/vector/lancedb.rs
git commit -m "feat(vector): LanceDbStore::list_external_ids and count real impls"
```

---

## Task 5: `LanceDbStore::delete_by_external_ids` + `delete_by_source` Real Implementations

**Files:**
- Modify: `mur-core/src/store/vector/lancedb.rs`

- [ ] **Step 1: Failing test**

Append:

```rust
    #[tokio::test]
    async fn sources_delete_operations_work() {
        let tmp = TempDir::new().unwrap();
        let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
        let now = chrono::Utc::now();
        let zeros = vec![0.0_f32; TEST_DIM as usize];

        let chunks: Vec<super::EmbeddedChunk> = (0..4)
            .map(|i| super::EmbeddedChunk {
                chunk_id: format!("cid-{i}"),
                source_id: if i < 2 { "src:a".into() } else { "src:b".into() },
                external_id: format!("doc-{i}"),
                ordinal: 0,
                text: "x".into(),
                heading_path: vec![],
                char_range: (0, 1),
                updated_at: now,
                embedding: zeros.clone(),
            })
            .collect();

        <LanceDbStore as super::VectorStore>::upsert(&store, &chunks).await.unwrap();

        // Delete one doc from src:a
        <LanceDbStore as super::VectorStore>::delete_by_external_ids(
            &store,
            "src:a",
            &["doc-0".to_string()],
        )
        .await
        .unwrap();
        let remaining_a = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "src:a")
            .await
            .unwrap();
        assert_eq!(remaining_a, vec!["doc-1"]);

        // delete_by_source removes all src:b rows
        <LanceDbStore as super::VectorStore>::delete_by_source(&store, "src:b").await.unwrap();
        let remaining_b = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "src:b")
            .await
            .unwrap();
        assert!(remaining_b.is_empty());

        // src:a is untouched
        let still_a = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "src:a")
            .await
            .unwrap();
        assert_eq!(still_a, vec!["doc-1"]);
    }
```

- [ ] **Step 2: Run — expect FAIL**

```bash
cargo test -p mur-core store::vector::lancedb::tests::sources_delete_operations_work
```

- [ ] **Step 3: Replace both delete method bodies**

```rust
    async fn delete_by_external_ids(
        &self,
        source_id: &str,
        external_ids: &[String],
    ) -> Result<()> {
        if external_ids.is_empty() {
            return Ok(());
        }
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(());
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let escaped: Vec<String> = external_ids
            .iter()
            .map(|e| format!("'{}'", e.replace('\'', "''")))
            .collect();
        let predicate = format!(
            "source_id = '{}' AND external_id IN ({})",
            source_id.replace('\'', "''"),
            escaped.join(",")
        );
        table.delete(&predicate).await?;
        Ok(())
    }

    async fn delete_by_source(&self, source_id: &str) -> Result<()> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(());
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let predicate = format!("source_id = '{}'", source_id.replace('\'', "''"));
        table.delete(&predicate).await?;
        Ok(())
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mur-core store::vector::lancedb
```

Expected: all new tests pass, including `sources_delete_operations_work`. Existing patterns tests still pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/store/vector/lancedb.rs
git commit -m "feat(vector): LanceDbStore::delete_by_external_ids and delete_by_source"
```

---

## Task 6: Un-ignore the Conformance Macro Tests

The P1.1 conformance macro left `conformance_upsert_and_search` and `conformance_delete_by_source_clears` marked `#[ignore]`. Now that the impls are real, remove those markers.

**Files:**
- Modify: `mur-core/src/store/vector/tests.rs`

- [ ] **Step 1: Remove the two `#[ignore = "..."]` lines**

Open `mur-core/src/store/vector/tests.rs`. In the `#[macro_export] macro_rules! vector_store_conformance` block, delete the lines:

```rust
        #[ignore = "enabled from P1.2 when upsert is real"]
```
and
```rust
        #[ignore = "enabled from P1.2 when delete_by_source is real"]
```

The surrounding `#[tokio::test]` and method bodies stay intact.

- [ ] **Step 2: Before the conformance tests run, the LanceDbStore factory used by the macro needs to call `ensure_sources_table` — otherwise search returns empty and upsert fails silently.**

In `mur-core/src/store/vector/lancedb.rs`, update `make_store_for_conformance` to ensure the table:

```rust
    async fn make_store_for_conformance() -> LanceDbStore {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
        // LanceDB tables are lazy — upsert calls ensure_sources_table internally,
        // but the delete_by_source conformance test runs before any upsert.
        store.ensure_sources_table().await.unwrap();
        store
        // NOTE: TempDir drops at end of function. In practice the conformance tests
        // don't need long-lived TempDirs because the LanceDB connection holds the
        // path open until the store handle itself is dropped.
    }
```

**WAIT — this is a bug pattern**: `tmp` drops at function end; if the test takes a Mutex or awaits elsewhere, the LanceDB directory vanishes mid-test. Fix by leaking the TempDir OR by returning `(LanceDbStore, TempDir)` — which requires changing the macro. Simpler: use `std::mem::forget(tmp);` to deliberately leak (conformance tests are rare).

Replace the factory body with:

```rust
    async fn make_store_for_conformance() -> LanceDbStore {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
        store.ensure_sources_table().await.unwrap();
        // Intentionally leak the TempDir: the LanceDB connection holds an open
        // handle and expects the directory to persist for the lifetime of the
        // `store` (which outlives this function). Conformance tests are few and
        // short-lived; leaked temp dirs get cleaned up by the OS at reboot.
        std::mem::forget(tmp);
        store
    }
```

- [ ] **Step 3: Run conformance suite**

```bash
cargo test -p mur-core store::vector::lancedb::tests::conformance_
```

Expected: three tests now all `ok` (no more ignored):
- `conformance_smoke_count_empty ... ok`
- `conformance_upsert_and_search ... ok`
- `conformance_delete_by_source_clears ... ok`

- [ ] **Step 4: Full workspace regression**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

Record counts.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/store/vector/tests.rs mur-core/src/store/vector/lancedb.rs
git commit -m "test(vector): un-ignore upsert/delete conformance tests now that impls are real"
```

---

## Task 7: Shared Markdown Chunker (heading-aware)

**Files:**
- Modify: `mur-core/Cargo.toml` (add `pulldown-cmark`)
- Create: `mur-core/src/sources/chunker/mod.rs`
- Create: `mur-core/src/sources/chunker/markdown.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Add pulldown-cmark dep**

Open `mur-core/Cargo.toml`. In `[dependencies]`, add:

```toml
pulldown-cmark = "0.12"
```

Keep alphabetical order where the file already has one, otherwise just append.

- [ ] **Step 2: Create `sources/chunker/mod.rs`**

```rust
//! Text chunking utilities shared across adapters.
//!
//! `markdown::chunk_markdown` is used by the Obsidian and (later) Joplin
//! adapters. Notion uses a block-aware chunker that will live as a sibling
//! module in P1.4 (`notion_blocks.rs`).

pub mod markdown;
```

- [ ] **Step 3: Create `sources/chunker/markdown.rs`**

```rust
//! Markdown heading-aware chunker.
//!
//! Splits a document into chunks by H1/H2/H3 boundaries and, within a chunk,
//! by paragraph boundaries if the byte budget is exceeded. Retains heading
//! path (list of current headings at ≤3 levels) for provenance.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

/// A chunk of markdown text with provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownChunk {
    /// Hierarchy of headings at the chunk's start (e.g. ["Design", "Error handling"]).
    pub heading_path: Vec<String>,
    /// Inclusive character offsets in the ORIGINAL body.
    pub char_range: (usize, usize),
    /// Plaintext content (markdown syntax stripped to embedding-friendly form).
    pub text: String,
}

/// Chunk a markdown body.
///
/// - `title` prepends the chunker's notion of a "document title" into the heading
///   path of every chunk (so searches over titles + sections work).
/// - `max_chars` is a soft byte budget: chunks exceeding it are split at the
///   nearest paragraph boundary.
pub fn chunk_markdown(title: &str, body: &str, max_chars: usize) -> Vec<MarkdownChunk> {
    let mut out: Vec<MarkdownChunk> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new(); // (level, heading text)
    let mut cur_buf = String::new();
    let mut cur_start: usize = 0;
    let mut in_heading: Option<HeadingLevel> = None;
    let mut heading_text_buf = String::new();

    let flush = |heading_stack: &Vec<(u8, String)>,
                 cur_buf: &mut String,
                 cur_start: &mut usize,
                 next_start: usize,
                 out: &mut Vec<MarkdownChunk>| {
        let text = std::mem::take(cur_buf).trim().to_string();
        if !text.is_empty() {
            let hp: Vec<String> = Some(title.to_string())
                .into_iter()
                .chain(heading_stack.iter().map(|(_, h)| h.clone()))
                .filter(|s| !s.is_empty())
                .collect();
            out.push(MarkdownChunk {
                heading_path: hp,
                char_range: (*cur_start, next_start),
                text,
            });
        }
        *cur_start = next_start;
    };

    // pulldown-cmark offset iterator yields (Event, byte-range) tuples.
    let offset_iter = Parser::new(body).into_offset_iter();

    for (event, range) in offset_iter {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                // Flush what we had before the heading
                let next_start = range.start;
                flush(&heading_stack, &mut cur_buf, &mut cur_start, next_start, &mut out);
                in_heading = Some(level);
                heading_text_buf.clear();
            }
            Event::End(TagEnd::Heading(level)) => {
                let text = std::mem::take(&mut heading_text_buf).trim().to_string();
                let depth = heading_level_to_u8(level);
                // Pop stack to the new depth - 1; then push.
                while let Some(&(d, _)) = heading_stack.last() {
                    if d >= depth {
                        heading_stack.pop();
                    } else {
                        break;
                    }
                }
                if !text.is_empty() {
                    heading_stack.push((depth, text));
                }
                in_heading = None;
                // Reset cursor for the NEXT chunk's starting offset.
                cur_start = range.end;
            }
            Event::Text(t) => {
                if in_heading.is_some() {
                    heading_text_buf.push_str(&t);
                } else {
                    cur_buf.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_heading.is_none() {
                    cur_buf.push('\n');
                }
            }
            Event::End(TagEnd::Paragraph) => {
                cur_buf.push_str("\n\n");
                // Soft split if over budget.
                if cur_buf.len() > max_chars {
                    flush(
                        &heading_stack,
                        &mut cur_buf,
                        &mut cur_start,
                        range.end,
                        &mut out,
                    );
                }
            }
            Event::Code(c) => {
                if in_heading.is_none() {
                    cur_buf.push_str(&format!("`{c}`"));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                cur_buf.push_str("\n```\n");
            }
            Event::End(TagEnd::CodeBlock) => {
                cur_buf.push_str("\n```\n");
            }
            _ => {}
        }
    }

    // Final flush.
    flush(
        &heading_stack,
        &mut cur_buf,
        &mut cur_start,
        body.len(),
        &mut out,
    );

    out
}

fn heading_level_to_u8(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_returns_no_chunks() {
        let chunks = chunk_markdown("T", "", 1000);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_paragraph_single_chunk() {
        let chunks = chunk_markdown("T", "Hello world.", 1000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, vec!["T".to_string()]);
        assert!(chunks[0].text.contains("Hello world"));
    }

    #[test]
    fn h1_h2_chunks_track_heading_path() {
        let body = "# Design\n\nintro para\n\n## Error handling\n\nsecond para\n";
        let chunks = chunk_markdown("Doc", body, 1000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading_path, vec!["Doc".to_string(), "Design".to_string()]);
        assert!(chunks[0].text.contains("intro para"));
        assert_eq!(
            chunks[1].heading_path,
            vec![
                "Doc".to_string(),
                "Design".to_string(),
                "Error handling".to_string()
            ]
        );
        assert!(chunks[1].text.contains("second para"));
    }

    #[test]
    fn oversized_chunk_splits_on_paragraph() {
        let big = "x".repeat(200);
        let body = format!("{big}\n\n{big}\n\n{big}");
        let chunks = chunk_markdown("T", &body, 150);
        // Each of the three paragraphs should land in its own chunk (each > 150 chars).
        assert!(chunks.len() >= 3);
    }

    #[test]
    fn sibling_h2_does_not_leak_previous_h2() {
        let body = "## A\n\npara A\n\n## B\n\npara B\n";
        let chunks = chunk_markdown("Doc", body, 1000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[0].heading_path,
            vec!["Doc".to_string(), "A".to_string()]
        );
        assert_eq!(
            chunks[1].heading_path,
            vec!["Doc".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn char_range_covers_body() {
        let body = "one\n\ntwo";
        let chunks = chunk_markdown("T", body, 1000);
        // Last char_range end should be body length.
        let last = chunks.last().unwrap();
        assert_eq!(last.char_range.1, body.len());
    }
}
```

- [ ] **Step 4: Declare modules**

Append to `mur-core/src/sources/mod.rs`:

```rust
pub mod chunker;
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p mur-core sources::chunker
```

Expected: 6 tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/sources/mod.rs mur-core/src/sources/chunker/
git commit -m "feat(sources): markdown heading-aware chunker (shared Obsidian/Joplin)"
```

---

## Task 8: ObsidianAdapter Skeleton + `list_documents`

**Files:**
- Create: `mur-core/src/sources/adapters/mod.rs`
- Create: `mur-core/src/sources/adapters/obsidian.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Create `sources/adapters/mod.rs`**

```rust
//! `KnowledgeSource` implementations for each supported external app.

pub mod obsidian;
```

- [ ] **Step 2: Declare in sources module**

Append to `mur-core/src/sources/mod.rs`:

```rust
pub mod adapters;
```

- [ ] **Step 3: Create `obsidian.rs` with struct, constructor, list_documents (TDD — tests first)**

Create `mur-core/src/sources/adapters/obsidian.rs`:

```rust
//! Obsidian vault adapter.
//!
//! Treats a local folder containing markdown files as a pull-index source.
//! Excludes `.obsidian/` (app state), `.trash/`, and any user-configured
//! folders via `SourceInstance.scope.exclude_folders`.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

use crate::sources::chunker::markdown as md;
use crate::sources::instance::SourceInstance;
use crate::sources::kind::SourceKind;
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};
use crate::sources::KnowledgeSource;

const EXCLUDED_SEGMENTS: &[&str] = &[".obsidian", ".trash"];
const CHUNK_MAX_CHARS: usize = 6000; // roughly 1500 tokens

/// Obsidian vault adapter.
pub struct ObsidianAdapter {
    id: String,
    vault_path: PathBuf,
    weight: f32,
    exclude_folders: Vec<String>,
}

impl ObsidianAdapter {
    /// Build from a `SourceInstance` (expects `type_name == "obsidian"` and the
    /// `scope.vault` value set to an absolute path).
    pub fn from_instance(instance: &SourceInstance) -> Result<Self> {
        if instance.type_name != "obsidian" {
            bail!(
                "expected type_name 'obsidian', got '{}'",
                instance.type_name
            );
        }
        let vault_val = instance
            .scope
            .get("vault")
            .context("source instance missing scope.vault")?;
        let vault_str: String = match vault_val {
            serde_yaml::Value::String(s) => s.clone(),
            _ => bail!("scope.vault must be a string"),
        };
        let vault_path = PathBuf::from(&vault_str);
        if !vault_path.is_dir() {
            bail!("vault path does not exist or is not a directory: {vault_str}");
        }
        // Warn — not fatal — if .obsidian isn't present.
        if !vault_path.join(".obsidian").exists() {
            tracing::warn!(
                "vault {} has no .obsidian/ subdir — proceeding anyway",
                vault_path.display()
            );
        }

        let exclude_folders: Vec<String> = instance
            .scope
            .get("exclude_folders")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            id: instance.id.clone(),
            vault_path,
            weight: instance.weight,
            exclude_folders,
        })
    }

    fn is_excluded(&self, rel: &Path) -> bool {
        for seg in rel.components() {
            if let Some(s) = seg.as_os_str().to_str() {
                if EXCLUDED_SEGMENTS.iter().any(|x| *x == s) {
                    return true;
                }
                if self.exclude_folders.iter().any(|e| e == s) {
                    return true;
                }
            }
        }
        false
    }
}

#[async_trait]
impl KnowledgeSource for ObsidianAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> SourceKind {
        SourceKind::PullIndex
    }

    fn weight(&self) -> f32 {
        self.weight
    }

    async fn list_documents(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        let threshold: Option<DateTime<Utc>> = cursor.and_then(|c| {
            if c.is_empty() {
                None
            } else {
                DateTime::parse_from_rfc3339(&c.0).ok().map(|dt| dt.with_timezone(&Utc))
            }
        });

        let mut docs: Vec<DocRef> = Vec::new();
        let mut max_ts: Option<DateTime<Utc>> = None;

        for entry in walkdir::WalkDir::new(&self.vault_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let rel = path
                .strip_prefix(&self.vault_path)
                .ok()
                .map(|r| r.to_path_buf())
                .unwrap_or_else(|| path.to_path_buf());
            if self.is_excluded(&rel) {
                continue;
            }
            let meta = entry.metadata().context("stat vault file")?;
            let modified = meta.modified().context("no mtime on vault file")?;
            let updated_at: DateTime<Utc> = modified.into();

            if let Some(t) = threshold
                && updated_at <= t
            {
                continue;
            }
            if max_ts.map_or(true, |m| updated_at > m) {
                max_ts = Some(updated_at);
            }
            let external_id = rel
                .to_str()
                .context("vault path is not valid UTF-8")?
                .to_string();
            let title = rel
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            docs.push(DocRef {
                external_id,
                title,
                updated_at,
            });
        }

        // Cursor semantics: advance to max(updated_at) of returned docs; if no
        // docs were returned, keep the previous cursor (preserves threshold).
        let cursor_out = match max_ts {
            Some(t) => SyncCursor(t.to_rfc3339()),
            None => SyncCursor(threshold.map(|t| t.to_rfc3339()).unwrap_or_default()),
        };

        Ok((docs, cursor_out))
    }

    async fn fetch(&self, _doc_ref: &DocRef) -> Result<Document> {
        anyhow::bail!("ObsidianAdapter::fetch arrives in Task 9")
    }

    fn chunk(&self, _doc: &Document) -> Result<Vec<Chunk>> {
        anyhow::bail!("ObsidianAdapter::chunk arrives in Task 10")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn make_vault() -> TempDir {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        fs::write(tmp.path().join("note-a.md"), "# A\n\ncontent a").unwrap();
        fs::create_dir_all(tmp.path().join("folder")).unwrap();
        fs::write(tmp.path().join("folder").join("note-b.md"), "# B\n\ncontent b").unwrap();
        fs::create_dir_all(tmp.path().join(".trash")).unwrap();
        fs::write(tmp.path().join(".trash").join("deleted.md"), "# D").unwrap();
        tmp
    }

    fn make_instance(id: &str, vault: &Path) -> SourceInstance {
        let mut scope = BTreeMap::new();
        scope.insert(
            "vault".into(),
            serde_yaml::Value::String(vault.to_string_lossy().to_string()),
        );
        SourceInstance {
            id: id.into(),
            type_name: "obsidian".into(),
            kind: SourceKind::PullIndex,
            enabled: true,
            weight: 1.0,
            scope,
            sync: crate::sources::instance::SyncState::default(),
            stats: crate::sources::instance::SourceStats::default(),
            keyring_entry: None,
        }
    }

    #[tokio::test]
    async fn from_instance_validates_vault_exists() {
        let inst = {
            let mut i = make_instance("obsidian-x", Path::new("/nonexistent/path/xyz"));
            i.scope.insert(
                "vault".into(),
                serde_yaml::Value::String("/nonexistent/path/xyz".into()),
            );
            i
        };
        assert!(ObsidianAdapter::from_instance(&inst).is_err());
    }

    #[tokio::test]
    async fn list_documents_finds_md_files_excluding_hidden() {
        let tmp = make_vault();
        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _cursor) = adapter.list_documents(None).await.unwrap();
        let mut ids: Vec<String> = docs.iter().map(|d| d.external_id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["folder/note-b.md".to_string(), "note-a.md".to_string()]
        );
    }

    #[tokio::test]
    async fn exclude_folder_is_honoured() {
        let tmp = make_vault();
        let mut inst = make_instance("obsidian-main", tmp.path());
        inst.scope.insert(
            "exclude_folders".into(),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("folder".into())]),
        );
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let ids: Vec<String> = docs.iter().map(|d| d.external_id.clone()).collect();
        assert_eq!(ids, vec!["note-a.md".to_string()]);
    }

    #[tokio::test]
    async fn cursor_filters_older_files() {
        let tmp = make_vault();
        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let future = chrono::Utc::now() + chrono::Duration::days(365);
        let (docs, _) = adapter
            .list_documents(Some(SyncCursor(future.to_rfc3339())))
            .await
            .unwrap();
        assert!(docs.is_empty());
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mur-core sources::adapters::obsidian
```

Expected: 4 tests pass (one validates error on bad vault, three test discovery).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/sources/
git commit -m "feat(sources): ObsidianAdapter skeleton + list_documents"
```

---

## Task 9: ObsidianAdapter::fetch (read file + frontmatter)

**Files:**
- Modify: `mur-core/src/sources/adapters/obsidian.rs`

- [ ] **Step 1: Failing test**

Append inside the `#[cfg(test)] mod tests` block of `obsidian.rs`:

```rust
    #[tokio::test]
    async fn fetch_reads_file_and_parses_frontmatter() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        fs::write(
            tmp.path().join("spec.md"),
            "---\ntags: [design, oauth]\nstatus: draft\n---\n\n# Auth spec\n\nbody.",
        )
        .unwrap();

        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let doc_ref = docs.iter().find(|d| d.external_id == "spec.md").unwrap();
        let doc = adapter.fetch(doc_ref).await.unwrap();
        assert_eq!(doc.source_id, "obsidian-main");
        assert_eq!(doc.external_id, "spec.md");
        assert!(doc.tags.contains(&"design".to_string()));
        assert!(doc.tags.contains(&"oauth".to_string()));
        // Body stripped of frontmatter
        match &doc.body {
            DocumentBody::Markdown(s) => {
                assert!(s.starts_with("# Auth spec"));
                assert!(!s.contains("---"));
            }
            _ => panic!("expected markdown body"),
        }
        // metadata retains arbitrary fields
        assert_eq!(doc.metadata.get("status").and_then(|v| v.as_str()), Some("draft"));
    }
```

- [ ] **Step 2: Run — expect FAIL (bail from Task 8 stub)**

```bash
cargo test -p mur-core sources::adapters::obsidian::tests::fetch_reads_file_and_parses_frontmatter
```

- [ ] **Step 3: Replace the `fetch` body**

Replace the `fetch` method's body with:

```rust
    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        let full_path = self.vault_path.join(&doc_ref.external_id);
        let raw = tokio::fs::read_to_string(&full_path)
            .await
            .with_context(|| format!("read vault file {}", full_path.display()))?;
        let (frontmatter, body_without_fm) = strip_frontmatter(&raw);

        let (tags, metadata) = frontmatter
            .as_ref()
            .map(parse_frontmatter)
            .unwrap_or_else(|| (Vec::new(), serde_json::Value::Object(Default::default())));

        let title = doc_ref
            .title
            .clone()
            .unwrap_or_else(|| doc_ref.external_id.clone());

        // Deep-link back to the vault via Obsidian URL scheme.
        let url = {
            let vault_name = self
                .vault_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("vault");
            Some(format!(
                "obsidian://open?vault={}&file={}",
                urlencoding::encode(vault_name),
                urlencoding::encode(&doc_ref.external_id)
            ))
        };

        Ok(Document {
            source_id: self.id.clone(),
            external_id: doc_ref.external_id.clone(),
            title,
            body: DocumentBody::Markdown(body_without_fm.to_string()),
            url,
            updated_at: doc_ref.updated_at,
            tags,
            metadata,
        })
    }
```

- [ ] **Step 4: Add helper functions at the bottom of `obsidian.rs` (outside any impl / mod):**

```rust
/// Strip the YAML frontmatter block if present. Returns `(Some(fm_text), body)`
/// or `(None, body)`. Frontmatter must start at character 0 and be delimited
/// by `---\n` ... `\n---\n` (or `\r\n` variants).
fn strip_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let mut lines = raw.split_inclusive('\n');
    let first = match lines.next() {
        Some(l) => l.trim_end_matches(['\r', '\n']),
        None => return (None, raw),
    };
    if first != "---" {
        return (None, raw);
    }
    // Find the closing "---" line.
    let mut acc_len = first.len() + 1; // include newline
    let mut end_at: Option<usize> = None;
    for line in lines {
        acc_len += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            end_at = Some(acc_len);
            break;
        }
    }
    match end_at {
        Some(idx) => {
            let fm = &raw[..idx];
            let body = raw[idx..].trim_start_matches('\n').trim_start_matches('\r');
            // Drop leading/trailing `---\n` lines from fm for cleaner parse
            let fm_inner = fm
                .trim_start_matches("---")
                .trim_start_matches(['\r', '\n'])
                .trim_end_matches("---\n")
                .trim_end_matches("---\r\n")
                .trim_end_matches("---");
            (Some(fm_inner), body)
        }
        None => (None, raw),
    }
}

/// Parse frontmatter YAML into (tags, metadata).
///
/// `tags` is the value of the `tags` key if it is a sequence of strings.
/// `metadata` is the full frontmatter as a JSON-compatible value, useful for
/// downstream `Document.metadata`.
fn parse_frontmatter(fm: &&str) -> (Vec<String>, serde_json::Value) {
    let parsed: serde_yaml::Value = serde_yaml::from_str(fm).unwrap_or(serde_yaml::Value::Null);
    let tags = parsed
        .get("tags")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let metadata = yaml_to_json(&parsed);
    (tags, metadata)
}

fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    use serde_yaml::Value as Y;
    match v {
        Y::Null => serde_json::Value::Null,
        Y::Bool(b) => serde_json::Value::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        Y::String(s) => serde_json::Value::String(s.clone()),
        Y::Sequence(s) => serde_json::Value::Array(s.iter().map(yaml_to_json).collect()),
        Y::Mapping(m) => {
            let mut obj = serde_json::Map::new();
            for (k, val) in m {
                if let Some(ks) = k.as_str() {
                    obj.insert(ks.to_string(), yaml_to_json(val));
                }
            }
            serde_json::Value::Object(obj)
        }
        Y::Tagged(t) => yaml_to_json(&t.value),
    }
}
```

- [ ] **Step 5: Ensure `urlencoding` dep exists or add it**

```bash
grep -n "^urlencoding" mur-core/Cargo.toml
```

If missing, add `urlencoding = "2"` to `[dependencies]`.

- [ ] **Step 6: Run tests**

```bash
cargo test -p mur-core sources::adapters::obsidian
```

Expected: all 5 tests pass (4 from Task 8 + `fetch_reads_file_and_parses_frontmatter`).

- [ ] **Step 7: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/sources/adapters/obsidian.rs
git commit -m "feat(sources/obsidian): fetch file with frontmatter stripping + tag extraction"
```

---

## Task 10: ObsidianAdapter::chunk (delegate to markdown chunker)

**Files:**
- Modify: `mur-core/src/sources/adapters/obsidian.rs`

- [ ] **Step 1: Failing test**

Append:

```rust
    #[tokio::test]
    async fn chunk_emits_multiple_chunks_with_heading_path() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        let body = "# H1\n\npara under h1.\n\n## H2\n\npara under h2.\n\n## H2-b\n\npara under h2-b.\n";
        fs::write(tmp.path().join("multi.md"), body).unwrap();

        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let doc_ref = docs.iter().find(|d| d.external_id == "multi.md").unwrap();
        let doc = adapter.fetch(doc_ref).await.unwrap();
        let chunks = adapter.chunk(&doc).unwrap();
        assert!(chunks.len() >= 3, "expected >=3 chunks, got {}", chunks.len());
        for c in &chunks {
            assert_eq!(c.source_id, "obsidian-main");
            assert_eq!(c.external_id, "multi.md");
            assert!(!c.chunk_id.is_empty());
            assert!(!c.text.is_empty());
        }
        // Ordinals are 0-indexed and strictly increasing
        let ords: Vec<usize> = chunks.iter().map(|c| c.ordinal).collect();
        assert_eq!(ords, (0..chunks.len()).collect::<Vec<_>>());
    }
```

- [ ] **Step 2: Run — expect FAIL (bail stub)**

```bash
cargo test -p mur-core sources::adapters::obsidian::tests::chunk_emits_multiple_chunks_with_heading_path
```

- [ ] **Step 3: Replace `chunk` body**

```rust
    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>> {
        let body = match &doc.body {
            DocumentBody::Markdown(s) | DocumentBody::PlainText(s) => s.clone(),
            DocumentBody::NotionBlocks(_) => bail!("obsidian adapter does not handle notion blocks"),
        };
        let raw_chunks = md::chunk_markdown(&doc.title, &body, CHUNK_MAX_CHARS);
        let mut out = Vec::with_capacity(raw_chunks.len());
        for (i, c) in raw_chunks.into_iter().enumerate() {
            out.push(Chunk::new(
                doc.source_id.clone(),
                doc.external_id.clone(),
                i,
                c.text,
                c.heading_path,
                c.char_range,
                doc.updated_at,
            ));
        }
        Ok(out)
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mur-core sources::adapters::obsidian
```

Expected: all 6 obsidian tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/sources/adapters/obsidian.rs
git commit -m "feat(sources/obsidian): chunk via shared markdown chunker"
```

---

## Task 11: Sync Orchestrator — Single-Source Sync Flow

**Files:**
- Create: `mur-core/src/sources/sync.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Create `sources/sync.rs` with tests**

Create `mur-core/src/sources/sync.rs`:

```rust
//! Sync orchestrator.
//!
//! Drives one adapter through list → fetch → chunk → embed → upsert, updates
//! the `SourceInstance` yaml cursor/stats, and detects deletions via a
//! set-diff against the vector store. P1.2 is single-source / sequential /
//! manual — `--watch` and cross-source parallelism arrive in P1.4.

use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::Arc;

use crate::sources::instance::{SourceInstance, SourceInstanceStore, SyncError};
use crate::sources::types::SyncCursor;
use crate::sources::KnowledgeSource;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::vector::{EmbeddedChunk, VectorStore};

/// High-level summary returned by `sync_source`.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub docs_synced: usize,
    pub chunks_emitted: usize,
    pub docs_deleted: usize,
    pub errors: Vec<String>,
}

/// Run one full sync cycle for a single source.
pub async fn sync_source(
    adapter: &dyn KnowledgeSource,
    instance: &mut SourceInstance,
    instance_store: &SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    embedding_cfg: &EmbeddingConfig,
    full: bool,
) -> Result<SyncReport> {
    let source_id = adapter.id().to_string();
    let cursor_in = if full {
        None
    } else {
        instance
            .sync
            .last_cursor
            .clone()
            .map(SyncCursor)
            .filter(|c| !c.is_empty())
    };

    tracing::info!(source_id = %source_id, full = full, "sync: start");

    // Step 1: list documents
    let (doc_refs, new_cursor) = adapter
        .list_documents(cursor_in)
        .await
        .context("adapter.list_documents")?;

    let mut report = SyncReport::default();
    let mut successful_external_ids_from_list: std::collections::HashSet<String> =
        doc_refs.iter().map(|d| d.external_id.clone()).collect();

    for doc_ref in &doc_refs {
        match fetch_chunk_embed_upsert(adapter, doc_ref, &*vector_store, embedding_cfg).await {
            Ok(n_chunks) => {
                report.docs_synced += 1;
                report.chunks_emitted += n_chunks;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!(
                    source_id = %source_id,
                    doc = %doc_ref.external_id,
                    error = %msg,
                    "sync: doc error"
                );
                report.errors.push(msg.clone());
                successful_external_ids_from_list.remove(&doc_ref.external_id);
                instance.sync.push_error(SyncError {
                    at: Utc::now(),
                    doc: doc_ref.external_id.clone(),
                    msg,
                });
            }
        }
    }

    // Step 2: Deletion detection. If `full=true`, compare against the full
    // adapter state; if incremental, we ONLY know what changed. For P1.2,
    // deletion detection runs in `full` mode — incremental does not attempt it
    // (may leave orphan chunks until next full sync or explicit `remove`).
    if full {
        let indexed = vector_store
            .list_external_ids(&source_id)
            .await
            .context("list_external_ids")?;
        let current: std::collections::HashSet<String> =
            doc_refs.iter().map(|d| d.external_id.clone()).collect();
        let deleted: Vec<String> = indexed.into_iter().filter(|id| !current.contains(id)).collect();
        if !deleted.is_empty() {
            vector_store
                .delete_by_external_ids(&source_id, &deleted)
                .await
                .context("delete_by_external_ids")?;
            report.docs_deleted = deleted.len();
        }
    }

    // Step 3: Update instance yaml
    instance.sync.last_cursor = if new_cursor.is_empty() {
        None
    } else {
        Some(new_cursor.0)
    };
    instance.sync.last_sync_at = Some(Utc::now());
    instance.sync.last_error = report.errors.last().cloned();
    instance.stats.doc_count = report.docs_synced as u64;
    instance.stats.chunk_count = report.chunks_emitted as u64;

    instance_store
        .save(instance)
        .context("persist SourceInstance yaml")?;

    tracing::info!(
        source_id = %source_id,
        docs = report.docs_synced,
        chunks = report.chunks_emitted,
        deleted = report.docs_deleted,
        errors = report.errors.len(),
        "sync: complete"
    );

    Ok(report)
}

async fn fetch_chunk_embed_upsert(
    adapter: &dyn KnowledgeSource,
    doc_ref: &crate::sources::types::DocRef,
    vector_store: &dyn VectorStore,
    embedding_cfg: &EmbeddingConfig,
) -> Result<usize> {
    let doc = adapter.fetch(doc_ref).await.context("adapter.fetch")?;
    let chunks = adapter.chunk(&doc).context("adapter.chunk")?;
    if chunks.is_empty() {
        return Ok(0);
    }
    // Delete-by-external_id before upserting (handles the case where the same
    // document's chunk set changed — old chunk_ids no longer valid).
    vector_store
        .delete_by_external_ids(&doc.source_id, &[doc.external_id.clone()])
        .await
        .context("delete old chunks for doc")?;
    let mut embedded: Vec<EmbeddedChunk> = Vec::with_capacity(chunks.len());
    for c in chunks {
        let vec = embed(&c.text, embedding_cfg)
            .await
            .with_context(|| format!("embed chunk of doc {}", doc.external_id))?;
        embedded.push(EmbeddedChunk {
            chunk_id: c.chunk_id,
            source_id: c.source_id,
            external_id: c.external_id,
            ordinal: c.ordinal,
            text: c.text,
            heading_path: c.heading_path,
            char_range: c.char_range,
            updated_at: c.updated_at,
            embedding: vec,
        });
    }
    let n = embedded.len();
    vector_store
        .upsert(&embedded)
        .await
        .context("vector_store.upsert")?;
    Ok(n)
}
```

- [ ] **Step 2: Declare in sources module**

Append to `mur-core/src/sources/mod.rs`:

```rust
pub mod sync;
```

- [ ] **Step 3: Compile check**

```bash
cargo check -p mur-core 2>&1 | tail -15
```

Expected: clean. This task has no test of its own — the E2E test in Task 17 exercises it.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/mod.rs mur-core/src/sources/sync.rs
git commit -m "feat(sources): sync orchestrator with per-doc isolation and full-mode deletion diff"
```

---

## Task 12: CLI Handlers — `source add obsidian`, `source list`, `source status`

**Files:**
- Modify: `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Replace the `source_cmd.rs` file with a working dispatcher for three verbs**

Open `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.1/mur-core/src/cmd/source_cmd.rs`. Keep the existing enum definitions (`SourceCommand`, `AddKind`) but REPLACE the `handle` function and ADD the three verb implementations.

Replace the file's content from `pub async fn handle(cmd: SourceCommand) -> Result<()>` onward (inclusive) with:

```rust
pub async fn handle(cmd: SourceCommand) -> Result<()> {
    match cmd {
        SourceCommand::Add { kind } => match kind {
            AddKind::Obsidian {
                instance,
                vault,
                exclude_folder,
            } => add_obsidian(instance, vault, exclude_folder).await,
            AddKind::Notion { .. } => bail!("`mur source add notion` arrives in P1.4"),
            AddKind::Joplin { .. } => bail!("`mur source add joplin` arrives in P1.4"),
        },
        SourceCommand::List { json, verbose } => list(json, verbose).await,
        SourceCommand::Remove { id, keep_index } => remove(&id, keep_index).await,
        SourceCommand::Sync { id, full, watch } => {
            if watch {
                bail!("`mur source sync --watch` arrives in P1.4");
            }
            sync(id.as_deref(), full).await
        }
        SourceCommand::Status { id } => status(id.as_deref()).await,
        SourceCommand::Weight { id, value } => set_weight(&id, value).await,
        SourceCommand::Test { id } => test_source(&id).await,
        SourceCommand::Reindex { .. } => bail!("`mur source reindex` arrives in P1.3"),
        SourceCommand::InstallSchedule => bail!("`mur source install-schedule` arrives in P1.4"),
        SourceCommand::Disable { id } => set_enabled(&id, false).await,
        SourceCommand::Enable { id } => set_enabled(&id, true).await,
    }
}
```

- [ ] **Step 2: Add the `add_obsidian` function at the bottom of the same file**

```rust
async fn add_obsidian(
    instance: Option<String>,
    vault: std::path::PathBuf,
    exclude_folder: Vec<String>,
) -> Result<()> {
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SyncState, SourceStats};
    use crate::sources::kind::SourceKind;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    // Derive id: user-supplied instance OR auto (first "obsidian", then "obsidian:randXXXX")
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("obsidian:{tag}"),
        _ => {
            let existing: Vec<String> = store.list()?.into_iter().map(|i| i.id).collect();
            if !existing.iter().any(|id| id == "obsidian") {
                "obsidian".to_string()
            } else {
                // find a free obsidian:<rand>
                let mut rng = rand::random::<u16>();
                loop {
                    let candidate = format!("obsidian:{rng:04x}");
                    if !existing.contains(&candidate) {
                        break candidate;
                    }
                    rng = rng.wrapping_add(1);
                }
            }
        }
    };

    let abs_vault = std::fs::canonicalize(&vault)
        .with_context(|| format!("resolve vault path {}", vault.display()))?;
    if !abs_vault.is_dir() {
        bail!("vault path is not a directory: {}", abs_vault.display());
    }

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    scope.insert(
        "vault".into(),
        serde_yaml::Value::String(abs_vault.to_string_lossy().to_string()),
    );
    if !exclude_folder.is_empty() {
        scope.insert(
            "exclude_folders".into(),
            serde_yaml::Value::Sequence(
                exclude_folder.into_iter().map(serde_yaml::Value::String).collect(),
            ),
        );
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "obsidian".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: None,
    };
    store.save(&inst)?;
    println!("✅ Connected vault {} as `{}`", abs_vault.display(), id);
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}
```

- [ ] **Step 3: Add `list` and `status` functions**

```rust
async fn list(json: bool, verbose: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let items = store.list()?;
    if json {
        let j = serde_json::to_string_pretty(&items)?;
        println!("{j}");
        return Ok(());
    }
    if items.is_empty() {
        println!("(no sources — use `mur source add obsidian --vault <path>`)");
        return Ok(());
    }
    println!(
        "{:<22} {:<10} {:<8} {:>7} {:>7} {:<24}",
        "ID", "TYPE", "STATUS", "DOCS", "WEIGHT", "LAST SYNC"
    );
    for inst in &items {
        let status = if !inst.enabled { "off" } else { "ok" };
        let last = inst
            .sync
            .last_sync_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into());
        println!(
            "{:<22} {:<10} {:<8} {:>7} {:>7.2} {:<24}",
            inst.id,
            inst.type_name,
            status,
            inst.stats.doc_count,
            inst.weight,
            last
        );
        if verbose {
            println!("    scope: {:?}", inst.scope);
            if let Some(err) = &inst.sync.last_error {
                println!("    last_error: {err}");
            }
        }
    }
    Ok(())
}

async fn status(id: Option<&str>) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let items = match id {
        Some(i) => vec![store.load(i)?],
        None => store.list()?,
    };
    if items.is_empty() {
        println!("(no sources)");
        return Ok(());
    }
    for inst in items {
        println!("─── {} ({}) ───", inst.id, inst.type_name);
        println!("  enabled     : {}", inst.enabled);
        println!("  weight      : {:.2}", inst.weight);
        println!("  scope       : {:?}", inst.scope);
        println!(
            "  last_sync_at: {}",
            inst.sync
                .last_sync_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".into())
        );
        println!(
            "  last_cursor : {}",
            inst.sync.last_cursor.unwrap_or_else(|| "none".into())
        );
        println!("  docs        : {}", inst.stats.doc_count);
        println!("  chunks      : {}", inst.stats.chunk_count);
        if let Some(err) = &inst.sync.last_error {
            println!("  last_error  : {err}");
        }
        if !inst.sync.errors_tail.is_empty() {
            println!("  errors_tail : {} entries", inst.sync.errors_tail.len());
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Update imports at the top of `source_cmd.rs`**

Ensure these are imported at the top:

```rust
use anyhow::{Context, Result, bail};
use clap::Subcommand;
```

(Add `Context` if not already there.)

- [ ] **Step 5: Add `rand` dep if not present**

```bash
grep -n "^rand" mur-core/Cargo.toml
```

If missing, add `rand = "0.8"` to `[dependencies]`. (If the workspace already provides `rand`, prefer `rand = { workspace = true }`.)

- [ ] **Step 6: Compile**

```bash
cargo check --workspace 2>&1 | tail -10
```

- [ ] **Step 7: Smoke test the three commands against a temp vault**

```bash
# Create a temp vault
T=$(mktemp -d)
mkdir -p "$T/.obsidian"
echo "# Hello" > "$T/hello.md"

# Add it (this writes ~/.mur/sources/obsidian.yaml — delete after if you don't want it persisted)
cargo run -- source add obsidian --vault "$T"

# List
cargo run -- source list

# Status
cargo run -- source status obsidian

# Cleanup (optional — comment out if you want state to persist)
rm -f ~/.mur/sources/obsidian.yaml
rm -rf "$T"
```

Expected: `source add` prints "Connected vault …"; `source list` shows `obsidian` row; `source status` shows details with `docs: 0` (no sync yet).

- [ ] **Step 8: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): source add obsidian + source list + source status real handlers"
```

---

## Task 13: CLI Handlers — `source sync`, `source remove`, `source test`

**Files:**
- Modify: `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Append the three handler functions**

At the bottom of `source_cmd.rs`, add:

```rust
async fn sync(id: Option<&str>, full: bool) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::sync::sync_source;
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vector_store = get_vector_store(&cfg, &index_path).await?;

    let store = SourceInstanceStore::default_store()?;
    let targets: Vec<crate::sources::instance::SourceInstance> = match id {
        Some(i) => vec![store.load(i)?],
        None => store.list()?.into_iter().filter(|inst| inst.enabled).collect(),
    };
    if targets.is_empty() {
        println!("(no enabled sources to sync)");
        return Ok(());
    }

    for mut inst in targets {
        if inst.type_name != "obsidian" {
            println!("⏭  {}: adapter `{}` arrives in a later sub-milestone", inst.id, inst.type_name);
            continue;
        }
        let adapter = ObsidianAdapter::from_instance(&inst)?;
        println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
        let report = sync_source(
            &adapter,
            &mut inst,
            &store,
            vector_store.clone(),
            &emb_cfg,
            full,
        )
        .await?;
        println!(
            "  synced {} docs ({} chunks), deleted {}, {} errors",
            report.docs_synced,
            report.chunks_emitted,
            report.docs_deleted,
            report.errors.len()
        );
        for e in report.errors.iter().take(3) {
            println!("  ! {e}");
        }
    }
    Ok(())
}

async fn remove(id: &str, keep_index: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    use crate::store::vector::factory::get_vector_store;

    let store = SourceInstanceStore::default_store()?;
    // Fail if it doesn't exist — user should know
    let _ = store.load(id).with_context(|| format!("source `{id}` not found"))?;

    if !keep_index {
        let cfg = crate::store::config::load_config()?;
        let index_path = dirs::home_dir()
            .context("no home dir")?
            .join(".mur")
            .join("index");
        let vs = get_vector_store(&cfg, &index_path).await?;
        vs.delete_by_source(id).await.context("delete source chunks")?;
        println!("🗑  removed indexed chunks for {id}");
    }
    store.delete(id)?;
    println!("🗑  removed yaml for {id}");
    Ok(())
}

async fn test_source(id: &str) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::store::embedding::{EmbeddingConfig, embed};
    use std::time::Instant;

    let store = SourceInstanceStore::default_store()?;
    let inst = store.load(id)?;
    if inst.type_name != "obsidian" {
        bail!("test only supports obsidian in P1.2; got `{}`", inst.type_name);
    }
    let adapter = ObsidianAdapter::from_instance(&inst)?;

    let t0 = Instant::now();
    let (docs, _cursor) = adapter.list_documents(None).await?;
    println!("→ list_documents: {} docs in {:?}", docs.len(), t0.elapsed());
    if docs.is_empty() {
        println!("   (no documents — nothing to test)");
        return Ok(());
    }
    let doc_ref = &docs[0];
    println!("→ sampling first doc: {}", doc_ref.external_id);
    let t0 = Instant::now();
    let doc = adapter.fetch(doc_ref).await?;
    println!("  fetch: {} chars in {:?}", doc.body.as_plain_text().len(), t0.elapsed());

    let t0 = Instant::now();
    let chunks = adapter.chunk(&doc)?;
    println!("  chunk: {} chunks in {:?}", chunks.len(), t0.elapsed());

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let sample = chunks.first().map(|c| c.text.clone()).unwrap_or_default();
    let t0 = Instant::now();
    let v = embed(&sample, &emb_cfg).await?;
    println!("  embed: {} dims in {:?}", v.len(), t0.elapsed());

    println!("✅ adapter working");
    Ok(())
}
```

- [ ] **Step 2: Compile**

```bash
cargo check --workspace 2>&1 | tail -10
```

- [ ] **Step 3: Smoke test**

```bash
T=$(mktemp -d)
mkdir -p "$T/.obsidian"
echo -e "# One\n\npara one\n\n## Sub\n\npara sub" > "$T/a.md"
echo "# Two" > "$T/b.md"

cargo run -- source add obsidian --vault "$T"
cargo run -- source test obsidian                    # requires ollama or api key
cargo run -- source sync obsidian --full             # requires embedding provider configured
cargo run -- source list                             # docs should be > 0

cargo run -- source remove obsidian                  # cleanup
rm -rf "$T"
```

(If embedding provider is NOT configured locally, `source test` / `source sync` will fail — document this as "requires a working embedding provider" in the task report rather than a plan bug. E2E test in Task 17 uses a deterministic mock embedder to bypass this.)

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): source sync / remove / test real handlers"
```

---

## Task 14: CLI Handlers — `source weight`, `source enable`, `source disable`

**Files:**
- Modify: `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Append the two handler functions**

```rust
async fn set_weight(id: &str, value: f32) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    if !(0.0..=2.0).contains(&value) {
        bail!("weight must be in [0.0, 2.0], got {value}");
    }
    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;
    inst.weight = value;
    store.save(&inst)?;
    println!("✏️  {id} weight set to {value:.2}");
    Ok(())
}

async fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;
    inst.enabled = enabled;
    store.save(&inst)?;
    println!("✏️  {id} {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}
```

- [ ] **Step 2: Compile**

```bash
cargo check --workspace 2>&1 | tail -5
```

- [ ] **Step 3: Smoke test**

```bash
T=$(mktemp -d) && mkdir -p "$T/.obsidian"
cargo run -- source add obsidian --vault "$T"
cargo run -- source weight obsidian 0.5
cargo run -- source status obsidian | grep weight    # expect 0.50
cargo run -- source disable obsidian
cargo run -- source status obsidian | grep enabled   # expect false
cargo run -- source enable obsidian
cargo run -- source remove obsidian
rm -rf "$T"
```

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): source weight / enable / disable handlers"
```

---

## Task 15: Add `mur source search` Verb (Minimal Sources-only Search)

P1.2's "visible value" deliverable. A new verb under `source` that queries the sources table directly. No interaction with the existing `mur search` patterns path. P1.3 will merge both under `mur search`.

**Files:**
- Modify: `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Add `Search` variant to `SourceCommand` enum**

Inside the existing `pub enum SourceCommand { ... }`, after `Test { id: String },` add:

```rust
    /// Search indexed source chunks (minimal, sources-only — for P1.3 see `mur search`).
    Search {
        query: String,
        #[arg(long, short = 'k', default_value_t = 5)]
        limit: usize,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        json: bool,
    },
```

- [ ] **Step 2: Dispatch in `handle`**

Inside the `match cmd` in `handle`, add a new arm (keeping ordering consistent):

```rust
        SourceCommand::Search {
            query,
            limit,
            source,
            json,
        } => search(&query, limit, source.as_deref(), json).await,
```

- [ ] **Step 3: Implement the `search` handler**

At the bottom of `source_cmd.rs`:

```rust
async fn search(query: &str, limit: usize, source: Option<&str>, json: bool) -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::{SearchFilter, factory::get_vector_store};

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vs = get_vector_store(&cfg, &index_path).await?;

    let qvec = embed(query, &emb_cfg).await.context("embed query")?;

    let filter = SearchFilter {
        source_ids: source.map(|s| vec![s.to_string()]),
        since: None,
    };
    let hits = vs.search(&qvec, limit, &filter).await?;

    if json {
        let j = serde_json::to_string_pretty(&hits.iter().map(|h| serde_json::json!({
            "chunk_id": h.chunk_id,
            "source_id": h.source_id,
            "external_id": h.external_id,
            "score": h.score,
            "text": h.text,
            "heading_path": h.heading_path,
            "updated_at": h.updated_at.to_rfc3339(),
        })).collect::<Vec<_>>())?;
        println!("{j}");
        return Ok(());
    }
    if hits.is_empty() {
        println!("(no hits)");
        return Ok(());
    }
    for h in &hits {
        let hp = if h.heading_path.is_empty() {
            String::new()
        } else {
            format!(" § {}", h.heading_path.join(" / "))
        };
        println!(
            "[{:.3}] {} / {}{}",
            h.score, h.source_id, h.external_id, hp
        );
        // First 180 chars of text
        let preview: String = h.text.chars().take(180).collect();
        println!("       {}", preview);
    }
    Ok(())
}
```

- [ ] **Step 4: The `Hit` struct has no `Serialize` derive — serialize manually via `json!{}` (done above)**

Verify the `use crate::store::vector::...` covers `SearchFilter` and that `Hit` is reachable (it is, via `search` return type).

- [ ] **Step 5: Compile + smoke test**

```bash
cargo check --workspace 2>&1 | tail -5

T=$(mktemp -d) && mkdir -p "$T/.obsidian"
echo -e "# OAuth flow\n\nPKCE + localhost callback." > "$T/oauth.md"
echo "# Unrelated" > "$T/x.md"
cargo run -- source add obsidian --vault "$T"
cargo run -- source sync obsidian --full
cargo run -- source search "oauth pkce" -k 3

cargo run -- source remove obsidian
rm -rf "$T"
```

Expected: the `oauth.md` chunk appears in the top hits with a score > 0.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): source search — minimal sources-only query (P1.3 unifies)"
```

---

## Task 16: End-to-End Integration Test

**Files:**
- Create: `mur-core/tests/obsidian_e2e.rs`

- [ ] **Step 1: Decide on embedding for E2E**

We can't depend on a running Ollama / OpenAI key during `cargo test`. Two options:

(a) **Inject a deterministic in-process embedder** — would require adding a test hook to `sync_source` (e.g. generic over an embed-fn). Large refactor.

(b) **Drive adapter + chunker + LanceDB directly**, bypassing `sync_source` (and therefore the real embed step), injecting precomputed fake embeddings.

Go with (b) — it's a true end-to-end test of the library pieces that matter, without taking on a flag-infection refactor.

- [ ] **Step 2: Write `mur-core/tests/obsidian_e2e.rs`**

```rust
//! End-to-end: ObsidianAdapter + chunker + LanceDbStore.
//!
//! Does not exercise the embedding step (uses zero vectors). Real embeddings
//! are covered by manual smoke tests and the adapter unit tests.

use chrono::Utc;
use mur_core::sources::adapters::obsidian::ObsidianAdapter;
use mur_core::sources::instance::{SourceInstance, SourceStats, SyncState};
use mur_core::sources::kind::SourceKind;
use mur_core::sources::KnowledgeSource;
use mur_core::store::vector::{EmbeddedChunk, LanceDbStore, SearchFilter, VectorStore};
use std::collections::BTreeMap;
use std::fs;
use tempfile::TempDir;

fn make_instance(id: &str, vault: &std::path::Path) -> SourceInstance {
    let mut scope = BTreeMap::new();
    scope.insert(
        "vault".into(),
        serde_yaml::Value::String(vault.to_string_lossy().to_string()),
    );
    SourceInstance {
        id: id.into(),
        type_name: "obsidian".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: None,
    }
}

const DIM: i32 = 8;

fn zeros() -> Vec<f32> {
    vec![0.0_f32; DIM as usize]
}
fn ones() -> Vec<f32> {
    vec![1.0_f32; DIM as usize]
}

#[tokio::test]
async fn obsidian_end_to_end_sync_then_search() {
    let vault = TempDir::new().unwrap();
    fs::create_dir_all(vault.path().join(".obsidian")).unwrap();
    fs::write(
        vault.path().join("design.md"),
        "---\ntags: [design]\n---\n\n# Auth design\n\nJWT 15min access + 7d refresh.",
    )
    .unwrap();
    fs::write(
        vault.path().join("scratch.md"),
        "# scratch\n\nnothing interesting",
    )
    .unwrap();

    let inst = make_instance("obsidian:e2e", vault.path());
    let adapter = ObsidianAdapter::from_instance(&inst).unwrap();

    // List + fetch + chunk
    let (refs, _cursor) = adapter.list_documents(None).await.unwrap();
    assert_eq!(refs.len(), 2);

    let index = TempDir::new().unwrap();
    let store = LanceDbStore::open(index.path(), DIM).await.unwrap();
    store.ensure_sources_table().await.unwrap();

    let mut all_chunks: Vec<EmbeddedChunk> = Vec::new();
    for r in &refs {
        let doc = adapter.fetch(r).await.unwrap();
        for c in adapter.chunk(&doc).unwrap() {
            // Deterministic "embedding": 1.0s if external_id contains "design", else 0.0s.
            let embed = if c.external_id.contains("design") {
                ones()
            } else {
                zeros()
            };
            all_chunks.push(EmbeddedChunk {
                chunk_id: c.chunk_id,
                source_id: c.source_id,
                external_id: c.external_id,
                ordinal: c.ordinal,
                text: c.text,
                heading_path: c.heading_path,
                char_range: c.char_range,
                updated_at: c.updated_at,
                embedding: embed,
            });
        }
    }
    store.upsert(&all_chunks).await.unwrap();

    // Search with the "ones" vector — design.md chunks should come first.
    let hits = store.search(&ones(), 5, &SearchFilter::default()).await.unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].external_id, "design.md");

    // list_external_ids / count
    let ids = store.list_external_ids("obsidian:e2e").await.unwrap();
    assert!(ids.contains(&"design.md".to_string()));
    assert!(ids.contains(&"scratch.md".to_string()));

    let c = store.count(Some("obsidian:e2e")).await.unwrap();
    assert!(c >= 2);

    // Delete one file, re-sync via delete_by_external_ids
    fs::remove_file(vault.path().join("scratch.md")).unwrap();
    let (refs_after, _) = adapter.list_documents(None).await.unwrap();
    let current: std::collections::HashSet<String> =
        refs_after.iter().map(|r| r.external_id.clone()).collect();
    let indexed = store.list_external_ids("obsidian:e2e").await.unwrap();
    let deleted: Vec<String> = indexed.into_iter().filter(|id| !current.contains(id)).collect();
    assert_eq!(deleted, vec!["scratch.md".to_string()]);
    store
        .delete_by_external_ids("obsidian:e2e", &deleted)
        .await
        .unwrap();
    let after = store.list_external_ids("obsidian:e2e").await.unwrap();
    assert!(!after.contains(&"scratch.md".to_string()));
    assert!(after.contains(&"design.md".to_string()));

    // delete_by_source clears everything
    store.delete_by_source("obsidian:e2e").await.unwrap();
    let empty = store.list_external_ids("obsidian:e2e").await.unwrap();
    assert!(empty.is_empty());

    // Leak the LanceDB TempDir to avoid the handle-still-open race
    std::mem::forget(index);
    std::mem::forget(vault);
}
```

- [ ] **Step 3: Run the E2E test**

```bash
cargo test --test obsidian_e2e 2>&1 | tail -20
```

Expected: single test passes.

- [ ] **Step 4: Commit**

```bash
git add mur-core/tests/obsidian_e2e.rs
git commit -m "test(obsidian): end-to-end sync + search + delete integration test"
```

---

## Task 17: Final Verification

- [ ] **Step 1: Full workspace tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

Expected: all `ok`, 0 failed. The `obsidian_e2e` adds 1 new test binary entry. Compared to the P1.1 baseline (1246 passing), P1.2 adds roughly:
- Task 1 open_or_create_sources_table_is_idempotent: +1 lib test
- Task 2 roundtrip: +1 lib test
- Task 4 list/count: +1 lib test
- Task 5 delete: +1 lib test
- Task 6 un-ignored two conformance tests: +2 (from ignored → passing) on EACH of lib+bin harnesses = +4
- Task 7 chunker: +6 lib tests
- Task 8/9/10 obsidian: +6 lib tests (4 + 1 + 1)
- Task 16 e2e: +1 integration test

Expected delta: roughly **+20 passing tests** (exact count depends on harness propagation — only count trend, not exact number).

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-features -- -D warnings 2>&1 | tail -20
```

Fix any warnings we introduced. Pre-existing warnings unrelated to P1.2 are acceptable but flag them in the final report.

- [ ] **Step 3: fmt**

```bash
cargo fmt --check
```

If diffs, `cargo fmt` and commit as `style:` follow-up.

- [ ] **Step 4: Feature matrix**

```bash
cargo build --workspace 2>&1 | tail -3
cargo build --workspace --no-default-features --features "cli server" 2>&1 | tail -3
```

Both must succeed. The feature-off build should NOT include any `sources` code. If `source_cmd.rs` imports from `sources/*` without feature-gating, you'll see errors — fix by gating `source_cmd` itself behind `#[cfg(feature = "sources")]` (it already is from P1.1 Task 12, but the `sources` module declaration in `lib.rs` is NOT gated — that's fine because the module compiles regardless; it only matters that the CLI and factory paths used by the bin don't require it).

If the feature-off build fails because the bin's `source_cmd` reaches into `sources::*` that requires transitive symbols, the fix is to also gate the lib's `pub mod sources;` behind `#[cfg(feature = "sources")]`. Use this ONLY if required.

- [ ] **Step 5: Full CLI smoke on real machine**

(Optional but recommended.) Against a small real vault of 10-20 notes and a working embedding provider:

```bash
cargo run --release -- source add obsidian --vault ~/some-notes
cargo run --release -- source sync obsidian --full
cargo run --release -- source search "whatever you expect to hit" -k 5
cargo run --release -- source status
cargo run --release -- source remove obsidian
```

- [ ] **Step 6: CLAUDE.md update**

Open `CLAUDE.md`. Find the "Sources pipeline (P1.1 foundation in place; adapters arrive P1.2+)" paragraph added in P1.1, and REPLACE the status tag:

```markdown
**Sources pipeline (P1.2 — Obsidian adapter shipped; Notion/Joplin arrive P1.4):**
```

Keep the rest of the paragraph unchanged.

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): mark P1.2 Obsidian adapter shipped"
```

- [ ] **Step 7: Summary commit log**

```bash
git log --oneline feat/sources-p1.1..HEAD  # or whatever the P1.2 base branch is
```

Expected: ~15 commits from P1.2.

---

## Done Criteria (P1.2)

- [ ] LanceDB sources table created on demand (`ensure_sources_table`).
- [ ] `LanceDbStore::upsert`, `search`, `list_external_ids`, `delete_by_external_ids`, `delete_by_source`, `count` are real — not stubs. `rebuild_index` stays stub (P1.3).
- [ ] Conformance tests `conformance_upsert_and_search` + `conformance_delete_by_source_clears` pass (no longer `#[ignore]`).
- [ ] `sources::chunker::markdown::chunk_markdown` emits heading-aware chunks; paragraph-boundary split on oversized.
- [ ] `ObsidianAdapter` implements `KnowledgeSource` end-to-end: `list_documents` (walkdir + exclude + cursor), `fetch` (read file + strip frontmatter + tags), `chunk` (delegate to markdown chunker).
- [ ] `sources::sync::sync_source` performs list → fetch → chunk → embed → upsert, full-mode deletion diff, per-doc error isolation, persists cursor + stats to yaml.
- [ ] CLI verbs working for real: `source add obsidian`, `source list`, `source status`, `source sync [id] [--full]`, `source remove [id] [--keep-index]`, `source test`, `source weight`, `source enable`, `source disable`, `source search`.
- [ ] CLI verbs still stubbed: `add notion/joplin`, `sync --watch`, `reindex`, `install-schedule` (all return clear "arrives in P1.x" errors).
- [ ] E2E integration test covers list → fetch → chunk → upsert → search → delete flow.
- [ ] Clippy clean, fmt clean, feature matrix green.
- [ ] CLAUDE.md updated.

**Out of scope for P1.2 (deferred):**
- `sync --watch` + file watcher
- Unified `mur search` (patterns + sources merged) — P1.3
- tantivy BM25 — P1.3
- inject formatter Notes section — P1.3
- Qdrant backend — P1.3
- Notion / Joplin adapters — P1.4
- `install-schedule` — P1.4
- Token truncation for oversized source chunks at retrieval time — P1.3
- **Inline `#tag` extraction** in Obsidian — P1.2 only reads YAML-frontmatter `tags:`. Inline `#tag` parsing (`[A-Za-z_][A-Za-z0-9_/-]*`) is deferred; tags are metadata-only in P1.2 and don't drive retrieval.
- Incremental-mode deletion detection — P1.2 only detects deletes in `--full` sync. Incremental syncs may leave orphan chunks until the next full run. Acceptable given the typical Obsidian workflow (weekly full sync).
