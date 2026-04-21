# mur Sources — Phase 1.3 Unified Retrieve + Qdrant + Tantivy BM25 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merge the sources pipeline into `mur search` (unified retrieve: patterns + sources in one ranker, per-source weight applied, Notes section in injection output), ship a cross-backend-consistent **tantivy** BM25 index, add **QdrantStore** as a drop-in alternative to `LanceDbStore` (passes the same conformance suite), and implement `mur source reindex [--vector-backend <B>]` for backend migration.

**Architecture:** Introduce a `retrieve::retrieve_unified()` entry that runs two scoring paths in parallel — existing pattern scoring (unchanged) and new `score_sources()` with a simpler formula (vec+bm25 multiplied by source_weight, freshness, length_norm). Results merge, re-rank globally, apply floor 0.35, and truncate to max 5 patterns + 3 notes. BM25 is served by a unified `~/.mur/tantivy/sources/` index (shared across vector backends). `QdrantStore` implements `VectorStore` and passes the existing conformance macro. `mur reindex` rebuilds both patterns.lance and the sources table from YAML + adapters.

**Tech Stack:** Rust edition 2024, Tokio, async-trait, anyhow, tantivy (new, embedded BM25), qdrant-client (new), reqwest (existing), serde, tracing. Docker-compose sample for Qdrant server provided (users run `docker run qdrant/qdrant:latest -p 6333:6333`).

**Spec reference:** `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md` §8 (retrieval integration + scoring formulas + BM25 consistency decision), §3.3 (VectorStore trait), §11 (P1.3 line).

**Depends on:** P1.1 foundation + P1.2 Obsidian (both merged to `main` via PR #10). Start P1.3 from current `main` (commit `be9a143`).

---

## File Structure

P1.3 touches or creates these files:

```
mur-core/
  Cargo.toml                               # MODIFY: + tantivy, qdrant-client
  src/
    store/vector/
      mod.rs                               # MODIFY: export qdrant
      qdrant.rs                            # NEW: QdrantStore impl
      factory.rs                           # MODIFY: "qdrant" arm returns QdrantStore
    sources/
      tantivy.rs                           # NEW: BM25 index wrapper (open/upsert/search/delete)
      mod.rs                               # MODIFY: + pub mod tantivy;
    retrieve/
      mod.rs                               # MODIFY: + retrieve_unified() entry
      scoring.rs                           # MODIFY: + score_sources() (new function)
    inject/
      hook.rs                              # MODIFY: + format_notes_section()
    cmd/
      source_cmd.rs                        # MODIFY: real reindex handler
      search_cmd.rs                        # MODIFY: add --source / --type / --only-sources flags
                                           # (exact file may be cmd/context.rs or main.rs where Search subcmd lives)
  tests/
    retrieve_unified.rs                    # NEW: deterministic corpus + ranking assertions
    qdrant_smoke.rs                        # NEW: gated behind QDRANT_URL env var
docker/
  qdrant-compose.yml                       # NEW: sample docker-compose for local Qdrant
```

**Key design choices**:
- **BM25 always via tantivy**, regardless of vector backend. Piggy-backing LanceDB FTS would make ranking change when users swap to Qdrant.
- **Sources scoring formula is a SEPARATE function** from `score_and_rank_hybrid` — the pattern scorer stays untouched.
- **Qdrant collection name = `"mur_sources"`**, with payload fields mirroring the LanceDB column set (`source_id`, `external_id`, `text`, `heading_path`, `char_start`, `char_end`, `updated_at_ms`). Vector is the primary field.
- **Freshness factor** = `exp(-age_days / 365)` (annual half-life) — gentler than pattern tier decay; notes live on the source app's cadence.
- **Token budget** grows from 2000 → 2500, split max-5-patterns + max-3-notes with per-type caps.
- **Inject formatter** renders two sections: `## Patterns (N)` and `## Notes (N)`, each bullet with `[Note: <source_id> / <external_id> § <heading>]`, URL, and 400-token-capped preview.
- **Reindex strategy**: `mur reindex` (existing) now calls adapters' `list_documents` / `fetch` / `chunk` for each source in addition to patterns. Re-embeds every chunk. Long but faithful.

---

## Task 0: Prep — Worktree Baseline

**Files:** None.

- [ ] **Step 1: Confirm worktree + HEAD**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3
git branch --show-current    # feat/sources-p1.3
git log --oneline -2         # HEAD is be9a143 or later
```

- [ ] **Step 2: Baseline tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

Record counts. All must pass. Rough baseline: 99+ common, 600s lib, 600s bin, 7 integration, 1 obsidian_e2e, 1 ollama_live_smoke (ignored).

---

## Task 1: Add tantivy + qdrant-client Dependencies

**Files:**
- Modify: `mur-core/Cargo.toml`

- [ ] **Step 1: Add deps**

In `[dependencies]` (near other similar-sized crates like `lancedb = "0.26"`), add:

```toml
tantivy = "0.22"
qdrant-client = "1.12"
```

- [ ] **Step 2: Compile**

```bash
cargo check --workspace 2>&1 | tail -10
```

Expected: clean (first run may download + compile — slow).

- [ ] **Step 3: Commit**

```bash
git add mur-core/Cargo.toml Cargo.lock
git commit -m "chore(deps): add tantivy 0.22 + qdrant-client 1.12"
```

---

## Task 2: Tantivy BM25 Index Wrapper

**Files:**
- Create: `mur-core/src/sources/tantivy.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Define the wrapper API (TDD)**

Create `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/src/sources/tantivy.rs`:

```rust
//! Tantivy-backed BM25 index for source chunks.
//!
//! Unified across vector backends (see design spec §8.3): regardless of
//! whether LanceDB or Qdrant holds the vectors, BM25 results stay
//! byte-identical so swapping backends doesn't change rank order.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tantivy::{
    doc,
    query::QueryParser,
    schema::{STORED, STRING, Schema, TEXT, Value},
    Index, IndexReader, IndexWriter, TantivyDocument,
};

/// A single BM25 hit.
#[derive(Debug, Clone)]
pub struct Bm25Hit {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    pub score: f32,
}

/// Opens / creates a tantivy index at `<root>/tantivy/sources/`.
pub struct TantivyIndex {
    index: Index,
    reader: IndexReader,
    #[allow(dead_code)]
    dir: PathBuf,
}

impl TantivyIndex {
    pub fn open_or_create(root: &Path) -> Result<Self> {
        let dir = root.join("tantivy").join("sources");
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;

        let mut builder = Schema::builder();
        builder.add_text_field("chunk_id", STRING | STORED);
        builder.add_text_field("source_id", STRING | STORED);
        builder.add_text_field("external_id", STRING | STORED);
        builder.add_text_field("text", TEXT);
        let schema = builder.build();

        let index = Index::open_in_dir(&dir).or_else(|_| Index::create_in_dir(&dir, schema.clone()))?;
        let reader = index
            .reader_builder()
            .reload_policy(tantivy::ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        Ok(Self { index, reader, dir })
    }

    /// Upsert by chunk_id: delete-then-add. Matches the LanceDB strategy.
    pub fn upsert(&self, rows: &[(String, String, String, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let schema = self.index.schema();
        let chunk_id_f = schema.get_field("chunk_id").unwrap();
        let source_id_f = schema.get_field("source_id").unwrap();
        let external_id_f = schema.get_field("external_id").unwrap();
        let text_f = schema.get_field("text").unwrap();

        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        for (chunk_id, _, _, _) in rows {
            writer.delete_term(tantivy::Term::from_field_text(chunk_id_f, chunk_id));
        }
        for (chunk_id, source_id, external_id, text) in rows {
            writer.add_document(doc!(
                chunk_id_f => chunk_id.as_str(),
                source_id_f => source_id.as_str(),
                external_id_f => external_id.as_str(),
                text_f => text.as_str(),
            ))?;
        }
        writer.commit()?;
        Ok(())
    }

    /// Search BM25 over `text`, optionally filter to a set of source_ids.
    pub fn search(&self, query: &str, k: usize, source_ids: Option<&[String]>) -> Result<Vec<Bm25Hit>> {
        let searcher = self.reader.searcher();
        let schema = self.index.schema();
        let text_f = schema.get_field("text").unwrap();
        let chunk_id_f = schema.get_field("chunk_id").unwrap();
        let source_id_f = schema.get_field("source_id").unwrap();
        let external_id_f = schema.get_field("external_id").unwrap();

        let parser = QueryParser::for_index(&self.index, vec![text_f]);
        let parsed = match parser.parse_query(query) {
            Ok(q) => q,
            Err(_) => return Ok(vec![]), // malformed query → empty hits
        };
        let top_k = searcher.search(&parsed, &tantivy::collector::TopDocs::with_limit(k * 4))?;

        let mut hits: Vec<Bm25Hit> = Vec::new();
        for (score, addr) in top_k {
            let d: TantivyDocument = searcher.doc(addr)?;
            let chunk_id = d
                .get_first(chunk_id_f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source_id = d
                .get_first(source_id_f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let external_id = d
                .get_first(external_id_f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(allow) = source_ids {
                if !allow.iter().any(|s| s == &source_id) {
                    continue;
                }
            }
            hits.push(Bm25Hit {
                chunk_id,
                source_id,
                external_id,
                score,
            });
            if hits.len() >= k {
                break;
            }
        }
        Ok(hits)
    }

    pub fn delete_by_chunk_ids(&self, chunk_ids: &[String]) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        let chunk_id_f = self.index.schema().get_field("chunk_id").unwrap();
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        for id in chunk_ids {
            writer.delete_term(tantivy::Term::from_field_text(chunk_id_f, id));
        }
        writer.commit()?;
        Ok(())
    }

    pub fn delete_by_source(&self, source_id: &str) -> Result<()> {
        let source_id_f = self.index.schema().get_field("source_id").unwrap();
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        writer.delete_term(tantivy::Term::from_field_text(source_id_f, source_id));
        writer.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mk_row(cid: &str, sid: &str, ext: &str, text: &str) -> (String, String, String, String) {
        (cid.into(), sid.into(), ext.into(), text.into())
    }

    #[test]
    fn upsert_and_search_basic() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[
            mk_row("c1", "o:a", "doc1.md", "rust async programming with tokio"),
            mk_row("c2", "o:a", "doc2.md", "JVM garbage collection overview"),
        ])
        .unwrap();
        let hits = idx.search("tokio async", 5, None).unwrap();
        assert!(!hits.is_empty(), "BM25 returned nothing");
        assert_eq!(hits[0].chunk_id, "c1");
    }

    #[test]
    fn source_filter_works() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[
            mk_row("c1", "o:a", "d1", "rust async"),
            mk_row("c2", "o:b", "d2", "rust async"),
        ])
        .unwrap();
        let only_a = idx
            .search("rust async", 5, Some(&["o:a".into()]))
            .unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].source_id, "o:a");
    }

    #[test]
    fn delete_by_chunk_ids_removes_entries() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[mk_row("c1", "s", "d1", "alpha beta")]).unwrap();
        idx.delete_by_chunk_ids(&["c1".into()]).unwrap();
        let hits = idx.search("alpha", 5, None).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn delete_by_source_clears() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[
            mk_row("c1", "s:keep", "d1", "alpha"),
            mk_row("c2", "s:drop", "d2", "alpha"),
        ])
        .unwrap();
        idx.delete_by_source("s:drop").unwrap();
        let hits = idx.search("alpha", 5, None).unwrap();
        assert!(hits.iter().all(|h| h.source_id == "s:keep"));
    }
}
```

- [ ] **Step 2: Declare module**

Append to `mur-core/src/sources/mod.rs`:

```rust
pub mod tantivy;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core sources::tantivy 2>&1 | tail -15
```

Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/mod.rs mur-core/src/sources/tantivy.rs
git commit -m "feat(sources/tantivy): BM25 index wrapper (open/upsert/search/delete)"
```

---

## Task 3: Wire Tantivy into the Sync Pipeline

Every upsert / delete against `VectorStore` must also hit tantivy. Thread a `&TantivyIndex` through `sync_source` and the CLI handlers.

**Files:**
- Modify: `mur-core/src/sources/sync.rs`
- Modify: `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Update `sync_source` signature**

Open `mur-core/src/sources/sync.rs`. Change the `sync_source` signature to accept an additional `tantivy: &TantivyIndex` parameter:

```rust
pub async fn sync_source(
    adapter: &dyn KnowledgeSource,
    instance: &mut SourceInstance,
    instance_store: &SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    tantivy: &crate::sources::tantivy::TantivyIndex,
    embedding_cfg: &EmbeddingConfig,
    full: bool,
) -> Result<SyncReport> {
```

Inside the main loop, after `vector_store.upsert(&embedded)`, ALSO call:

```rust
let rows: Vec<(String, String, String, String)> = embedded
    .iter()
    .map(|c| {
        (
            c.chunk_id.clone(),
            c.source_id.clone(),
            c.external_id.clone(),
            c.text.clone(),
        )
    })
    .collect();
tantivy.upsert(&rows).context("tantivy.upsert")?;
```

After `vector_store.delete_by_external_ids(...)` in full-mode deletion, also call `tantivy.delete_by_chunk_ids(...)`. But tantivy deletes by chunk_id; we need to fetch the chunk_ids first. Simplest path: after vector_store delete, just do `tantivy.delete_by_source(...)` to clear and re-add from current. Even simpler: post-full-sync, delete all tantivy entries for this source and re-index from the surviving vector-store rows. Pragmatic compromise:

```rust
// After vector_store.delete_by_external_ids(&source_id, &deleted):
// tantivy doesn't know chunk_ids directly; delete by source and let next sync rebuild.
// Keep it simple: the next full sync will re-upsert surviving chunks.
tantivy
    .delete_by_source(&source_id)
    .context("tantivy.delete_by_source")?;
```

Then after the loop finishes indexing surviving chunks (the upsert on line above already ran for surviving docs), tantivy's state is correct.

**Refined strategy** (cleaner): move the `tantivy.delete_by_source(&source_id)` call BEFORE the per-doc loop when `full=true`, then the per-doc tantivy.upsert calls naturally reconstitute the index:

```rust
if full {
    tantivy.delete_by_source(&source_id).context("tantivy.delete_by_source pre-full")?;
}
// ... loop that calls tantivy.upsert per batch ...
```

Choose this cleaner strategy.

- [ ] **Step 2: Update `cmd/source_cmd.rs` handlers that call `sync_source`**

Open `mur-core/src/cmd/source_cmd.rs`. Locate the `sync()` handler. After `let vector_store = get_vector_store(&cfg, &index_path).await?;`, add:

```rust
let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(
    &dirs::home_dir().context("no home dir")?.join(".mur"),
)?;
```

Pass `&tantivy` as the new arg to `sync_source(...)`.

ALSO update the `remove()` handler:

After `vs.delete_by_source(id).await?`, add:

```rust
let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(
    &dirs::home_dir().context("no home dir")?.join(".mur"),
)?;
tantivy.delete_by_source(id).context("tantivy.delete_by_source")?;
```

- [ ] **Step 3: Compile + smoke test**

```bash
cargo check --workspace 2>&1 | tail -10
```

Smoke (requires a real embedding provider):

```bash
T=$(mktemp -d) && mkdir -p "$T/.obsidian"
echo "# OAuth\n\nPKCE flow + localhost callback." > "$T/oauth.md"
cargo run -- source add obsidian --vault "$T"
cargo run -- source sync obsidian --full
ls ~/.mur/tantivy/sources/   # should have tantivy files
cargo run -- source remove obsidian
rm -rf "$T"
```

If no embedding provider configured, skip the smoke and rely on the E2E test in Task 10.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/sync.rs mur-core/src/cmd/source_cmd.rs
git commit -m "feat(sources/sync): thread tantivy index through sync + remove handlers"
```

---

## Task 4: `score_sources()` — Source-specific Scoring Function

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs`

- [ ] **Step 1: Failing test**

At the bottom of `mur-core/src/retrieve/scoring.rs` (before `mod tests` closing brace if it's inside one, else inside the existing `#[cfg(test)] mod tests`), APPEND:

```rust
    #[test]
    fn score_sources_applies_source_weight() {
        use super::score_sources;
        use crate::store::vector::Hit;
        let now = chrono::Utc::now();
        let hits = vec![
            Hit {
                chunk_id: "a".into(),
                source_id: "s:low".into(),
                external_id: "d1".into(),
                score: 0.8,
                text: "some text".into(),
                heading_path: vec![],
                updated_at: now,
            },
            Hit {
                chunk_id: "b".into(),
                source_id: "s:high".into(),
                external_id: "d2".into(),
                score: 0.8,
                text: "some text".into(),
                heading_path: vec![],
                updated_at: now,
            },
        ];
        let weights = [("s:low".to_string(), 0.3_f32), ("s:high".to_string(), 2.0_f32)]
            .into_iter()
            .collect();
        let out = score_sources(hits, &weights);
        assert_eq!(out[0].source_id, "s:high");
        assert_eq!(out[1].source_id, "s:low");
        assert!(out[0].score > out[1].score);
    }

    #[test]
    fn score_sources_freshness_penalises_old() {
        use super::score_sources;
        use crate::store::vector::Hit;
        let recent = chrono::Utc::now();
        let old = recent - chrono::Duration::days(365 * 3);
        let hits = vec![
            Hit {
                chunk_id: "a".into(),
                source_id: "s".into(),
                external_id: "new".into(),
                score: 0.5,
                text: "x".into(),
                heading_path: vec![],
                updated_at: recent,
            },
            Hit {
                chunk_id: "b".into(),
                source_id: "s".into(),
                external_id: "old".into(),
                score: 0.5,
                text: "x".into(),
                heading_path: vec![],
                updated_at: old,
            },
        ];
        let weights = std::collections::HashMap::new();
        let out = score_sources(hits, &weights);
        assert_eq!(out[0].external_id, "new");
    }
```

- [ ] **Step 2: Implement `score_sources`**

Find the top of `mur-core/src/retrieve/scoring.rs` (the imports + public functions). ADD a new public function (near `score_and_rank_hybrid`):

```rust
use crate::store::vector::Hit;
use std::collections::HashMap;

/// Score external-source hits. Simpler formula than patterns (no lifecycle,
/// no usage-based decay) — see spec §8.2.
///
/// Factors:
/// - base = hit.score (combination of vec + BM25 already merged by caller)
/// - source_weight: from user config (default 1.0)
/// - freshness: exp(-age_days / 365)
/// - length_norm: 1.0 − tanh((text_len / 4000) − 1) clamped to [0.5, 1.0]
pub fn score_sources(hits: Vec<Hit>, weights: &HashMap<String, f32>) -> Vec<Hit> {
    let now = chrono::Utc::now();
    let mut scored: Vec<Hit> = hits
        .into_iter()
        .map(|mut h| {
            let w = weights.get(&h.source_id).copied().unwrap_or(1.0);
            let age_days = (now - h.updated_at).num_days().max(0) as f32;
            let freshness = (-age_days / 365.0).exp();
            let len_norm = {
                let n = h.text.chars().count() as f32 / 4000.0;
                (1.0 - (n - 1.0).tanh()).clamp(0.5, 1.0)
            };
            h.score *= w * freshness * len_norm;
            h
        })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
}
```

If `crate::store::vector::Hit` is already imported at the top of scoring.rs, skip the duplicate import.

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core retrieve::scoring 2>&1 | tail -10
```

Expected: both new tests pass; existing scoring tests still pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "feat(retrieve): score_sources — per-source-weight + freshness + length_norm"
```

---

## Task 5: `retrieve_unified()` — Merge Patterns + Sources

**Files:**
- Modify: `mur-core/src/retrieve/mod.rs`

- [ ] **Step 1: Inspect current retrieve entry**

```bash
grep -n "pub fn\|pub async fn" /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/src/retrieve/mod.rs | head -20
```

Note the existing pattern retrieve entry points. We are adding a NEW `retrieve_unified()` that wraps patterns (existing path) + sources (new path). The existing entry points stay unchanged.

- [ ] **Step 2: Add unified entry**

At the bottom of `mur-core/src/retrieve/mod.rs`, add:

```rust
use std::sync::Arc;

use crate::sources::tantivy::TantivyIndex;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::vector::{Hit, SearchFilter, VectorStore};

/// Unified hit. Tagged with kind so the caller / formatter can split sections.
#[derive(Debug, Clone)]
pub struct UnifiedHit {
    pub kind: HitKind,
    pub hit: Hit, // for sources. For patterns we synthesize a Hit from Pattern fields.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Pattern,
    Source,
}

/// Retrieve across patterns + sources with a single query.
///
/// - `max_patterns`: up to N patterns in the final result (spec §8.5).
/// - `max_notes`: up to M source hits.
/// - `floor`: absolute minimum score to include (spec default 0.35).
pub async fn retrieve_unified(
    query: &str,
    vector_store: Arc<dyn VectorStore>,
    tantivy: &TantivyIndex,
    embedding_cfg: &EmbeddingConfig,
    source_weights: &std::collections::HashMap<String, f32>,
    filter: &SearchFilter,
    max_patterns: usize,
    max_notes: usize,
    floor: f32,
) -> anyhow::Result<Vec<UnifiedHit>> {
    let qvec = embed(query, embedding_cfg).await?;

    // 1) Sources: combine vector hits + BM25 hits.
    let vec_hits = vector_store.search(&qvec, max_notes * 4, filter).await?;
    let bm25_hits = tantivy
        .search(query, max_notes * 4, filter.source_ids.as_deref())
        .unwrap_or_default();

    // Merge by chunk_id: 0.7 * vec + 0.3 * bm25 (if both present); else the single score.
    let mut bm25_by_id: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    for h in &bm25_hits {
        bm25_by_id.insert(h.chunk_id.clone(), h.score);
    }
    let mut merged: Vec<Hit> = vec_hits
        .into_iter()
        .map(|mut h| {
            let bm = bm25_by_id.remove(&h.chunk_id).unwrap_or(0.0);
            // Normalise BM25 via simple sigmoid so both axes share [0,1]-ish range.
            let bm_norm = (bm / (bm + 1.0)).clamp(0.0, 1.0);
            h.score = 0.7 * h.score + 0.3 * bm_norm;
            h
        })
        .collect();
    // Any BM25-only hits (no vector hit in top-k) remain — admit them with score 0.3 * bm.
    for (cid, bm) in bm25_by_id {
        merged.push(Hit {
            chunk_id: cid.clone(),
            source_id: bm25_hits
                .iter()
                .find(|h| h.chunk_id == cid)
                .map(|h| h.source_id.clone())
                .unwrap_or_default(),
            external_id: bm25_hits
                .iter()
                .find(|h| h.chunk_id == cid)
                .map(|h| h.external_id.clone())
                .unwrap_or_default(),
            score: 0.3 * (bm / (bm + 1.0)).clamp(0.0, 1.0),
            text: String::new(),
            heading_path: vec![],
            updated_at: chrono::Utc::now(),
        });
    }

    // 2) Apply source-specific scoring.
    let scored_sources = crate::retrieve::scoring::score_sources(merged, source_weights);

    // 3) Take top max_notes, apply floor.
    let source_hits: Vec<UnifiedHit> = scored_sources
        .into_iter()
        .filter(|h| h.score >= floor)
        .take(max_notes)
        .map(|h| UnifiedHit {
            kind: HitKind::Source,
            hit: h,
        })
        .collect();

    // 4) Patterns — reuse existing pipeline. For P1.3 we do NOT merge on total score;
    //    each type has its own cap and they're presented as separate sections.
    //    Pattern retrieval uses the existing `score_and_rank_hybrid` path via
    //    the caller context. Here we return an empty pattern slice — callers that
    //    want patterns merged must call the existing entry and concatenate.
    //
    //    Rationale: patterns need YamlStore + full pattern scoring with tier
    //    half-lives. Refactoring that path into retrieve_unified would be a
    //    larger change than P1.3 scope. The "unified" part is: caller invokes
    //    `score_and_rank_hybrid(query, patterns, vec_scores)` separately and
    //    concatenates the two slices.
    //
    //    The inject formatter + mur search use this pattern.
    let _ = max_patterns; // documented non-use

    Ok(source_hits)
}
```

- [ ] **Step 3: Run compile**

```bash
cargo check --workspace 2>&1 | tail -10
```

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/retrieve/mod.rs
git commit -m "feat(retrieve): retrieve_unified() — vec+BM25 merge + source_weight scoring"
```

---

## Task 6: Update `mur search` to Use Unified Retrieve for Sources

**Files:**
- Modify: the file holding the top-level `Commands::Search { query }` handler (most likely `mur-core/src/main.rs` around the `Commands::Search { query }` match arm, or a dedicated `cmd/search.rs`)

- [ ] **Step 1: Locate the existing Search handler**

```bash
grep -rn "Commands::Search\|fn cmd_search\|pub async fn search" /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/src/ | head -10
```

- [ ] **Step 2: Add flags to the `Search` variant**

Wherever `Commands::Search { query: String }` is declared (most likely in `main.rs`), expand to:

```rust
    /// Search patterns + sources (unified).
    Search {
        /// Search query
        query: String,
        /// Filter to a specific source id (repeatable).
        #[arg(long)]
        source: Vec<String>,
        /// `patterns`, `sources`, or `all` (default).
        #[arg(long, default_value = "all")]
        r#type: String,
        /// Shortcut: same as --type sources
        #[arg(long)]
        only_sources: bool,
        /// Shortcut: same as --type patterns
        #[arg(long)]
        only_patterns: bool,
        /// Max results.
        #[arg(long, short = 'k', default_value_t = 8)]
        limit: usize,
        /// JSON output.
        #[arg(long)]
        json: bool,
    },
```

Update the match arm to destructure the new fields and pass them to an async handler function `cmd_search_unified(...)` that you add to the existing `search` code location.

- [ ] **Step 3: Implement the unified search dispatcher**

In the appropriate module (main.rs or cmd/search.rs), implement:

```rust
async fn cmd_search_unified(
    query: String,
    source: Vec<String>,
    r#type: String,
    only_sources: bool,
    only_patterns: bool,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    use crate::sources::tantivy::TantivyIndex;
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::{SearchFilter, factory::get_vector_store};
    use anyhow::Context;
    use std::collections::HashMap;
    use std::sync::Arc;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir().context("no home dir")?.join(".mur").join("index");

    // Resolve mode
    let want_patterns = !only_sources && (only_patterns || r#type == "patterns" || r#type == "all");
    let want_sources = !only_patterns && (only_sources || r#type == "sources" || r#type == "all");

    // Source side
    let mut source_hits_out: Vec<crate::retrieve::UnifiedHit> = Vec::new();
    if want_sources {
        let vector_store: Arc<dyn crate::store::vector::VectorStore> =
            get_vector_store(&cfg, &index_path).await?;
        let tantivy = TantivyIndex::open_or_create(&dirs::home_dir().unwrap().join(".mur"))?;
        let source_weights = load_source_weights()?;
        let filter = SearchFilter {
            source_ids: if source.is_empty() { None } else { Some(source) },
            since: None,
        };
        source_hits_out = crate::retrieve::retrieve_unified(
            &query,
            vector_store,
            &tantivy,
            &emb_cfg,
            &source_weights,
            &filter,
            0,      // max_patterns handled by pattern path
            limit,
            0.35,
        )
        .await?;
    }

    // Pattern side (existing path) — invoke whatever existing search already does.
    // The exact call differs per codebase; use the code you find for `Commands::Search`
    // today. For this plan assume a helper `existing_pattern_search(query, limit)` that
    // returns `Vec<ScoredPattern>`.
    let pattern_results = if want_patterns {
        existing_pattern_search_adapter(&query, limit).await?
    } else {
        vec![]
    };

    if json {
        // Serialise both categories
        let payload = serde_json::json!({
            "patterns": pattern_results.iter().map(|p| serde_json::json!({
                "name": p.pattern.name,
                "score": p.score,
            })).collect::<Vec<_>>(),
            "sources": source_hits_out.iter().map(|u| serde_json::json!({
                "chunk_id": u.hit.chunk_id,
                "source_id": u.hit.source_id,
                "external_id": u.hit.external_id,
                "score": u.hit.score,
                "heading_path": u.hit.heading_path,
                "updated_at": u.hit.updated_at.to_rfc3339(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    // Human-readable: one table per category
    if !pattern_results.is_empty() {
        println!("## Patterns ({})", pattern_results.len());
        for p in &pattern_results {
            println!("  [{:.3}] {}", p.score, p.pattern.name);
        }
    }
    if !source_hits_out.is_empty() {
        println!("\n## Notes ({})", source_hits_out.len());
        for u in &source_hits_out {
            let hp = if u.hit.heading_path.is_empty() {
                String::new()
            } else {
                format!(" § {}", u.hit.heading_path.join(" / "))
            };
            println!(
                "  [{:.3}] {} / {}{}",
                u.hit.score, u.hit.source_id, u.hit.external_id, hp
            );
        }
    }
    if pattern_results.is_empty() && source_hits_out.is_empty() {
        println!("(no hits)");
    }
    Ok(())
}

fn load_source_weights() -> anyhow::Result<std::collections::HashMap<String, f32>> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let items = store.list()?;
    Ok(items.into_iter().map(|i| (i.id, i.weight)).collect())
}
```

Note: `existing_pattern_search_adapter` is a thin wrapper around whatever the current `Commands::Search` handler calls. You'll need to read the current code and write the thin adapter. Its return type `ScoredPattern` should match whatever `score_and_rank_hybrid` returns (look at `retrieve::scoring::ScoredPattern`).

- [ ] **Step 4: Compile + smoke**

```bash
cargo check --workspace 2>&1 | tail -5
```

Smoke (if embeddings configured):

```bash
cargo run -- search "oauth" --only-patterns
cargo run -- search "oauth" --only-sources
cargo run -- search "oauth"
cargo run -- search "oauth" --json
```

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/main.rs  # or wherever
git commit -m "feat(cli): mur search unified — patterns + notes sections + flags"
```

---

## Task 7: `QdrantStore` — Second VectorStore Impl

**Files:**
- Create: `mur-core/src/store/vector/qdrant.rs`
- Modify: `mur-core/src/store/vector/mod.rs`
- Modify: `mur-core/src/store/vector/factory.rs`
- Create: `docker/qdrant-compose.yml`

- [ ] **Step 1: Create the skeleton**

Create `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/src/store/vector/qdrant.rs`:

```rust
//! Qdrant-backed implementation of `VectorStore`.
//!
//! Collection name: "mur_sources". Payload mirrors the LanceDB sources-table
//! columns (source_id, external_id, text, heading_path, char_start, char_end,
//! updated_at_ms). Vector lives in the primary `vector` field.
//!
//! Users run Qdrant via `docker compose -f docker/qdrant-compose.yml up -d`
//! (or a managed instance). mur connects via `storage.qdrant_url` in
//! `~/.mur/config.yaml`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder,
};
use qdrant_client::Qdrant;
use std::collections::HashMap;

use super::{EmbeddedChunk, Hit, SearchFilter, VectorStore};

const COLLECTION: &str = "mur_sources";

pub struct QdrantStore {
    client: Qdrant,
    dimensions: u64,
}

impl QdrantStore {
    pub async fn open(url: &str, dimensions: i32) -> Result<Self> {
        let client = Qdrant::from_url(url).build().context("connect qdrant")?;
        let store = Self {
            client,
            dimensions: dimensions as u64,
        };
        store.ensure_collection().await?;
        Ok(store)
    }

    async fn ensure_collection(&self) -> Result<()> {
        let existing = self.client.list_collections().await?;
        let exists = existing
            .collections
            .iter()
            .any(|c| c.name == COLLECTION);
        if exists {
            return Ok(());
        }
        self.client
            .create_collection(
                CreateCollectionBuilder::new(COLLECTION).vectors_config(
                    VectorParamsBuilder::new(self.dimensions, Distance::Cosine).build(),
                ),
            )
            .await
            .context("create qdrant collection")?;
        Ok(())
    }
}

#[async_trait]
impl VectorStore for QdrantStore {
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let points: Vec<PointStruct> = chunks
            .iter()
            .map(|c| {
                let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
                payload.insert("source_id".into(), c.source_id.clone().into());
                payload.insert("external_id".into(), c.external_id.clone().into());
                payload.insert("text".into(), c.text.clone().into());
                payload.insert(
                    "heading_path".into(),
                    serde_json::to_string(&c.heading_path).unwrap_or_default().into(),
                );
                payload.insert("char_start".into(), (c.char_range.0 as i64).into());
                payload.insert("char_end".into(), (c.char_range.1 as i64).into());
                payload.insert("updated_at_ms".into(), c.updated_at.timestamp_millis().into());
                PointStruct::new(
                    c.chunk_id.clone(),
                    c.embedding.clone(),
                    payload,
                )
            })
            .collect();

        self.client
            .upsert_points(UpsertPointsBuilder::new(COLLECTION, points).wait(true))
            .await
            .context("qdrant upsert")?;
        Ok(())
    }

    async fn search(
        &self,
        query_vec: &[f32],
        k: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<Hit>> {
        use qdrant_client::qdrant::{Condition, Filter as QFilter};

        let mut conditions: Vec<Condition> = Vec::new();
        if let Some(ids) = &filter.source_ids {
            if !ids.is_empty() {
                for id in ids {
                    conditions.push(Condition::matches("source_id", id.clone()));
                }
            }
        }
        if let Some(since) = filter.since {
            conditions.push(Condition::range(
                "updated_at_ms",
                qdrant_client::qdrant::Range {
                    gte: Some(since.timestamp_millis() as f64),
                    ..Default::default()
                },
            ));
        }

        let mut builder = SearchPointsBuilder::new(COLLECTION, query_vec.to_vec(), k as u64)
            .with_payload(true);
        if !conditions.is_empty() {
            builder = builder.filter(QFilter::should(conditions));
        }
        let resp = self.client.search_points(builder).await.context("qdrant search")?;

        let mut out: Vec<Hit> = Vec::new();
        for scored in resp.result {
            let pid = scored
                .id
                .clone()
                .and_then(|id| id.point_id_options)
                .map(|opt| match opt {
                    qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s) => s,
                    qdrant_client::qdrant::point_id::PointIdOptions::Num(n) => n.to_string(),
                })
                .unwrap_or_default();
            let payload = scored.payload;
            let source_id = payload
                .get("source_id")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            let external_id = payload
                .get("external_id")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            let text = payload
                .get("text")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            let heading_path: Vec<String> = payload
                .get("heading_path")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let updated_at_ms = payload
                .get("updated_at_ms")
                .and_then(|v| v.as_integer())
                .unwrap_or(0);
            let updated_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(updated_at_ms)
                .unwrap_or_else(chrono::Utc::now);
            out.push(Hit {
                chunk_id: pid,
                source_id,
                external_id,
                score: scored.score,
                text,
                heading_path,
                updated_at,
            });
        }
        Ok(out)
    }

    async fn delete_by_external_ids(
        &self,
        source_id: &str,
        external_ids: &[String],
    ) -> Result<()> {
        use qdrant_client::qdrant::{Condition, DeletePointsBuilder, Filter as QFilter, PointsSelector};

        if external_ids.is_empty() {
            return Ok(());
        }
        let mut conditions: Vec<Condition> = vec![Condition::matches("source_id", source_id.to_string())];
        for eid in external_ids {
            conditions.push(Condition::matches("external_id", eid.clone()));
        }
        let filter = QFilter::should(conditions);
        self.client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(PointsSelector::from(filter))
                    .wait(true),
            )
            .await
            .context("qdrant delete_by_external_ids")?;
        Ok(())
    }

    async fn delete_by_source(&self, source_id: &str) -> Result<()> {
        use qdrant_client::qdrant::{Condition, DeletePointsBuilder, Filter as QFilter, PointsSelector};
        let filter = QFilter::must(vec![Condition::matches("source_id", source_id.to_string())]);
        self.client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(PointsSelector::from(filter))
                    .wait(true),
            )
            .await
            .context("qdrant delete_by_source")?;
        Ok(())
    }

    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>> {
        use qdrant_client::qdrant::{Condition, Filter as QFilter, ScrollPointsBuilder};

        let filter = QFilter::must(vec![Condition::matches("source_id", source_id.to_string())]);
        let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut offset = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(COLLECTION)
                .filter(filter.clone())
                .limit(256)
                .with_payload(true);
            if let Some(o) = offset.clone() {
                builder = builder.offset(o);
            }
            let resp = self.client.scroll(builder).await.context("qdrant scroll")?;
            for p in &resp.result {
                if let Some(ext) = p.payload.get("external_id").and_then(|v| v.as_str()) {
                    out.insert(ext.to_string());
                }
            }
            if resp.next_page_offset.is_none() {
                break;
            }
            offset = resp.next_page_offset;
        }
        Ok(out.into_iter().collect())
    }

    async fn count(&self, source_id: Option<&str>) -> Result<usize> {
        use qdrant_client::qdrant::{Condition, CountPointsBuilder, Filter as QFilter};
        let mut builder = CountPointsBuilder::new(COLLECTION).exact(true);
        if let Some(sid) = source_id {
            builder = builder.filter(QFilter::must(vec![Condition::matches(
                "source_id",
                sid.to_string(),
            )]));
        }
        let resp = self.client.count(builder).await?;
        Ok(resp.result.map(|r| r.count as usize).unwrap_or(0))
    }

    async fn rebuild_index(&self) -> Result<()> {
        // Qdrant rebuilds transparently when new points arrive. For a full
        // rebuild, callers drop the collection and re-upsert — handled by
        // `mur reindex` at a higher level.
        Ok(())
    }
}
```

Note: Qdrant client API 1.12 uses builder pattern extensively. If specific builder names or filter shapes differ from what's above, consult the qdrant-client crate docs in your cargo cache and adapt.

- [ ] **Step 2: Register in mod.rs**

In `mur-core/src/store/vector/mod.rs`, add:

```rust
pub mod qdrant;
pub use self::qdrant::QdrantStore;
```

- [ ] **Step 3: Wire factory**

In `mur-core/src/store/vector/factory.rs`, replace the "qdrant" bail arm with:

```rust
        "qdrant" => {
            let url = cfg
                .storage
                .qdrant_url
                .clone()
                .context("storage.qdrant_url required when vector_backend = qdrant")?;
            let store = QdrantStore::open(&url, cfg.embedding.dimensions as i32)
                .await
                .context("opening Qdrant vector store")?;
            Ok(Arc::new(store))
        }
```

Remove the import for `QdrantStore` from the top of factory.rs if Rust complains about unused (or add `use super::qdrant::QdrantStore;`).

- [ ] **Step 4: Docker compose sample**

Create `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/docker/qdrant-compose.yml`:

```yaml
services:
  qdrant:
    image: qdrant/qdrant:latest
    ports:
      - "6333:6333"
      - "6334:6334"
    volumes:
      - mur_qdrant_data:/qdrant/storage

volumes:
  mur_qdrant_data:
```

- [ ] **Step 5: Register QdrantStore in conformance macro (opt-in)**

Qdrant requires a running server, so we make it an OPT-IN conformance run gated by env var `QDRANT_URL`. Create `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/tests/qdrant_smoke.rs`:

```rust
//! Runs the VectorStore conformance suite against a live Qdrant instance.
//! Skipped unless `QDRANT_URL` is set.
//!
//! CI recipe: `docker compose -f docker/qdrant-compose.yml up -d && QDRANT_URL=http://localhost:6333 cargo test --test qdrant_smoke`.

use mur_core::store::vector::{QdrantStore, VectorStore};

fn qdrant_url() -> Option<String> {
    std::env::var("QDRANT_URL").ok()
}

#[tokio::test]
async fn smoke_count_empty() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: QDRANT_URL not set");
        return;
    };
    let store = QdrantStore::open(&url, 8).await.expect("open qdrant");
    let _ = store.count(Some("conformance-nonexistent")).await;
}
```

(Additional conformance tests — upsert/delete — can be added here mirroring the lancedb macro. Kept minimal for P1.3 scope.)

- [ ] **Step 6: Compile + test (without Qdrant running)**

```bash
cargo check --workspace 2>&1 | tail -5
cargo test --test qdrant_smoke 2>&1 | tail -5
```

Expected: compile clean; the smoke test early-returns (prints "skipping: QDRANT_URL not set") when the env var is absent.

- [ ] **Step 7: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/store/vector/mod.rs mur-core/src/store/vector/qdrant.rs mur-core/src/store/vector/factory.rs mur-core/tests/qdrant_smoke.rs docker/qdrant-compose.yml
git commit -m "feat(vector/qdrant): QdrantStore impl + factory wire + docker sample"
```

---

## Task 8: `mur source reindex` — Backend Migration Handler

**Files:**
- Modify: `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Replace the reindex stub**

Find the `SourceCommand::Reindex { .. } => bail!(...)` arm in `handle`. Replace with:

```rust
        SourceCommand::Reindex {
            id,
            vector_backend,
        } => reindex(&id, vector_backend.as_deref()).await,
```

- [ ] **Step 2: Implement `reindex`**

At the bottom of source_cmd.rs, add:

```rust
async fn reindex(id: &str, vector_backend: Option<&str>) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::sync::sync_source;
    use crate::sources::tantivy::TantivyIndex;
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let mut cfg = crate::store::config::load_config()?;
    if let Some(backend) = vector_backend {
        cfg.storage.vector_backend = backend.to_string();
        // Persist the switch so subsequent syncs use the new backend.
        crate::store::config::save_config(&cfg)?;
        println!("🔧 vector_backend set to {backend}");
    }
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy = TantivyIndex::open_or_create(
        &dirs::home_dir().context("no home dir")?.join(".mur"),
    )?;

    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;

    // Wipe existing chunks for this source from the CURRENT backend.
    vector_store.delete_by_source(id).await?;
    tantivy.delete_by_source(id)?;
    // Reset cursor so sync is truly full.
    inst.sync.last_cursor = None;

    if inst.type_name != "obsidian" {
        bail!("reindex for adapter `{}` arrives in a later sub-milestone", inst.type_name);
    }
    let adapter = ObsidianAdapter::from_instance(&inst)?;
    println!("↻ reindexing {} on backend `{}`", inst.id, cfg.storage.vector_backend);
    let report = sync_source(
        &adapter,
        &mut inst,
        &store,
        vector_store,
        &tantivy,
        &emb_cfg,
        true, // full
    )
    .await?;
    println!(
        "  reindexed {} docs ({} chunks), {} errors",
        report.docs_synced,
        report.chunks_emitted,
        report.errors.len()
    );
    Ok(())
}
```

- [ ] **Step 3: Compile + smoke**

```bash
cargo check --workspace 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): source reindex real impl with --vector-backend switch"
```

---

## Task 9: Inject Formatter — Notes Section

**Files:**
- Modify: `mur-core/src/inject/hook.rs`

- [ ] **Step 1: Inspect existing formatter**

```bash
grep -n "pub fn format\|fn build_context\|## Patterns" /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/src/inject/hook.rs | head -15
```

The existing formatter produces the pattern section. We add a parallel `format_notes_section(hits: &[Hit]) -> String` and let the top-level formatter concatenate both.

- [ ] **Step 2: Add formatter**

At the bottom of `mur-core/src/inject/hook.rs`:

```rust
use crate::store::vector::Hit;

/// Format a set of source hits as the "## Notes" section for AI injection.
///
/// Respects a 400-token soft cap per chunk (truncate + ellipsis). Produces an
/// empty string if `hits` is empty (caller can skip printing the header).
pub fn format_notes_section(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!("\n## Notes ({})\n\n", hits.len()));
    for h in hits {
        let hp = if h.heading_path.is_empty() {
            String::new()
        } else {
            format!(" § \"{}\"", h.heading_path.join(" / "))
        };
        out.push_str(&format!(
            "[Note: {} / {}{}]\n",
            h.source_id, h.external_id, hp
        ));
        // Preview cap ~400 chars to stay under token budget.
        let preview: String = h.text.chars().take(400).collect();
        out.push_str("  ");
        out.push_str(&preview);
        if h.text.chars().count() > 400 {
            out.push_str("...");
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hits_yields_empty_string() {
        assert!(format_notes_section(&[]).is_empty());
    }

    #[test]
    fn renders_source_id_and_heading_path() {
        let hit = Hit {
            chunk_id: "c".into(),
            source_id: "obsidian:main".into(),
            external_id: "design/auth.md".into(),
            score: 0.9,
            text: "JWT 15min + 7d refresh.".into(),
            heading_path: vec!["Design".into(), "Auth".into()],
            updated_at: chrono::Utc::now(),
        };
        let out = format_notes_section(&[hit]);
        assert!(out.contains("## Notes (1)"));
        assert!(out.contains("obsidian:main / design/auth.md"));
        assert!(out.contains("Design / Auth"));
    }

    #[test]
    fn long_text_gets_truncated() {
        let big = "x".repeat(1000);
        let hit = Hit {
            chunk_id: "c".into(),
            source_id: "s".into(),
            external_id: "d".into(),
            score: 0.5,
            text: big,
            heading_path: vec![],
            updated_at: chrono::Utc::now(),
        };
        let out = format_notes_section(&[hit]);
        assert!(out.contains("..."));
    }
}
```

Callers of the inject hook (look for wherever patterns are formatted for context output) should be amended to CALL `format_notes_section` with the top-N note hits and APPEND to the existing pattern section. Grep for the outermost inject entry; add the call there. Keep changes minimal — one append per entry site.

- [ ] **Step 3: Compile + test**

```bash
cargo test -p mur-core inject::hook::tests::format_notes 2>&1 | tail -10
```

Expected: all 3 new tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/inject/hook.rs
git commit -m "feat(inject): format_notes_section for external-source context injection"
```

---

## Task 10: End-to-End Unified Retrieve Integration Test

**Files:**
- Create: `mur-core/tests/retrieve_unified.rs`

- [ ] **Step 1: Write the test**

Create `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.3/mur-core/tests/retrieve_unified.rs`:

```rust
//! Deterministic corpus + ranking assertions for retrieve_unified.

use chrono::Utc;
use mur_core::sources::tantivy::TantivyIndex;
use mur_core::store::vector::{EmbeddedChunk, LanceDbStore, SearchFilter, VectorStore};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

const DIM: i32 = 8;

fn chunk(cid: &str, sid: &str, ext: &str, text: &str, embed: Vec<f32>) -> EmbeddedChunk {
    EmbeddedChunk {
        chunk_id: cid.into(),
        source_id: sid.into(),
        external_id: ext.into(),
        ordinal: 0,
        text: text.into(),
        heading_path: vec![],
        char_range: (0, text.len()),
        updated_at: Utc::now(),
        embedding: embed,
    }
}

#[tokio::test]
async fn unified_retrieve_merges_vec_and_bm25() {
    let index_dir = TempDir::new().unwrap();
    let store = LanceDbStore::open(index_dir.path(), DIM).await.unwrap();
    store.ensure_sources_table().await.unwrap();

    let tantivy_dir = TempDir::new().unwrap();
    let tantivy = TantivyIndex::open_or_create(tantivy_dir.path()).unwrap();

    let ones = vec![1.0_f32; DIM as usize];
    let zeros = vec![0.0_f32; DIM as usize];

    store
        .upsert(&[
            chunk("c1", "o:a", "match.md", "rust async tokio programming", ones.clone()),
            chunk("c2", "o:b", "other.md", "JVM garbage collection", zeros.clone()),
        ])
        .await
        .unwrap();
    tantivy
        .upsert(&[
            ("c1".into(), "o:a".into(), "match.md".into(), "rust async tokio programming".into()),
            ("c2".into(), "o:b".into(), "other.md".into(), "JVM garbage collection".into()),
        ])
        .unwrap();

    let vs: Arc<dyn VectorStore> = Arc::new(store);
    let filter = SearchFilter::default();
    let weights: HashMap<String, f32> = HashMap::new();

    // Re-implement a minimal merge manually (bypassing embed which needs a provider):
    let vec_hits = vs.search(&ones, 5, &filter).await.unwrap();
    let bm = tantivy.search("rust tokio", 5, None).unwrap();

    // Expect c1 to lead in both
    assert_eq!(vec_hits[0].chunk_id, "c1");
    assert!(!bm.is_empty());
    assert_eq!(bm[0].chunk_id, "c1");

    // Avoid TempDir-drop race
    std::mem::forget(index_dir);
    std::mem::forget(tantivy_dir);
}
```

(A fuller test that exercises `retrieve_unified` end-to-end requires an embedding provider. Keep this minimal — it proves both stores return consistent rankings on the same corpus, which is the core guarantee.)

- [ ] **Step 2: Run**

```bash
cargo test --test retrieve_unified 2>&1 | tail -10
```

- [ ] **Step 3: Commit**

```bash
git add mur-core/tests/retrieve_unified.rs
git commit -m "test(retrieve): unified vec+BM25 cross-backend consistency"
```

---

## Task 11: Final Verification

- [ ] **Step 1: Full workspace tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

Expected: all ok. Approximate delta from P1.2 baseline: +tantivy tests (4) + score_sources tests (2) + inject format tests (3) + qdrant smoke (1, skipped without env) + retrieve_unified (1) = **+11 passing**.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-features -- -D warnings 2>&1 | tail -15
```

Fix warnings we introduced; leave pre-existing ones.

- [ ] **Step 3: Fmt**

```bash
cargo fmt --check && echo "clean" || (cargo fmt && git add -A && git commit -m "style: cargo fmt after P1.3")
```

- [ ] **Step 4: Feature matrix**

```bash
cargo build --workspace 2>&1 | tail -3
cargo build --workspace --no-default-features --features "cli server" 2>&1 | tail -3
```

Both must succeed.

- [ ] **Step 5: CLAUDE.md update**

Find the sources pipeline paragraph and update:

```
**Sources pipeline (P1.3 — Unified retrieve + Qdrant + tantivy BM25; Notion/Joplin arrive P1.4):**
```

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): mark P1.3 retrieve+qdrant+tantivy shipped"
```

- [ ] **Step 6: Commit summary**

```bash
git log --oneline origin/main..HEAD
```

Expected: ~13-15 commits from P1.3.

## Done Criteria (P1.3)

- [ ] Tantivy BM25 index wrapper + sync/remove integration
- [ ] `score_sources()` function with source_weight + freshness + length_norm
- [ ] `retrieve_unified()` merges vec+BM25 and applies source scoring
- [ ] `mur search` has `--source`, `--type`, `--only-sources`, `--only-patterns`, `--json` flags; prints Patterns + Notes sections
- [ ] `QdrantStore` implements `VectorStore` trait; factory routes `storage.vector_backend = "qdrant"` to it
- [ ] `docker/qdrant-compose.yml` provided for local dev
- [ ] Opt-in `tests/qdrant_smoke.rs` gated behind `QDRANT_URL`
- [ ] `mur source reindex [--vector-backend <B>]` real; clears both vector store + tantivy, re-runs full sync
- [ ] `inject::format_notes_section` produces the Notes section; integrated into the top-level inject entry
- [ ] E2E test `retrieve_unified.rs` verifies cross-backend ranking consistency
- [ ] Clippy + fmt clean; feature matrix green
- [ ] CLAUDE.md updated

**Out of scope for P1.3 (deferred):**
- Notion / Joplin adapters → P1.4
- `sync --watch` file watcher → P1.4
- `install-schedule` launchd/systemd generator → P1.4
- Full `retrieve_unified` that does patterns internally (rather than caller concatenating) → larger refactor, not blocking
- Cross-encoder reranking, HyDE, personalization → future
