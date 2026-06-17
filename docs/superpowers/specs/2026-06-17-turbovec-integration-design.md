# turbovec Integration Design

**Date:** 2026-06-17
**Branch:** feat/turbovec-vector-backend

## Goals

Replace LanceDB's vector ANN role with turbovec for both the sources corpus and the skill/pattern corpus, reducing memory usage ~8× (4-bit quantization) and improving search latency on Apple Silicon via SIMD acceleration. LanceDB remains available as the default; turbovec is opt-in via config + Cargo feature.

## Background

mur has two separate vector search paths:

1. **Sources path** — `Arc<dyn VectorStore>` trait, used by `retrieve_unified` for Obsidian/Notion/Joplin chunks.
2. **Skill/pattern path** — `LanceDbStore` used directly (not via trait) in `server/context.rs`, `cmd/reindex.rs`, `cmd/workflow.rs`, `server/workflows.rs`.

turbovec is a published Rust crate (`cargo add turbovec`) implementing TurboQuant (ICLR 2026): SIMD-accelerated ANN with 8× memory compression at 4-bit. It provides `IdMapIndex` (stable u64 IDs, O(1) deletion, filtered search via allowlist) and `TurboQuantIndex` (simpler, no stable IDs). The allowlist feature runs filter logic inside the SIMD kernel — no post-hoc pruning overhead.

rusqlite (bundled) is already in mur-core, so no new heavy dependency is needed for the metadata sidecar.

## Architecture

### New Types

**`TurboVecStore`** — implements the existing `VectorStore` trait (sources path).
- `IdMapIndex` (behind `Arc<Mutex<…>>`) for ANN
- SQLite connection (behind `Arc<Mutex<…>>`, WAL mode) for chunk metadata
- On-disk: `{data_dir}/tv_sources.tvim` + `{data_dir}/tv_sources.db`

**`TurboSkillIndex`** — implements a new `SkillIndex` trait (skill/pattern path).
- `IdMapIndex` for ANN
- In-memory `Vec<SkillEntry>` sidecar (name, description, item_type, tier, importance)
- On-disk: `{data_dir}/tv_skills.tvim` + `{data_dir}/tv_skills_meta.json`

### New Trait

```rust
// mur-core/src/store/vector/skill_index.rs
#[async_trait]
pub trait SkillIndex: Send + Sync {
    async fn build_unified_index(
        &self,
        patterns: &[(Pattern, Vec<f32>)],
        workflows: &[(Workflow, Vec<f32>)],
    ) -> Result<()>;

    async fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
        item_type: Option<&str>,
    ) -> Result<Vec<SearchResult>>;
}
```

`LanceDbStore` gains a `SkillIndex` impl (zero behavior change). `TurboSkillIndex` is the new impl.

### Factory

```rust
// factory.rs additions
pub async fn get_skill_index(cfg: &Config, index_dir: &Path) -> Result<Arc<dyn SkillIndex>>;
```

Dispatches on `cfg.storage.vector_backend`. Existing `get_vector_store` gains a `"turbovec"` arm alongside `"lancedb"` and `"qdrant"`.

## Data Flow

### Sources — `TurboVecStore::upsert(chunks)`

1. Open SQLite transaction.
2. For each chunk: `DELETE FROM chunks WHERE chunk_id = ?` then `INSERT` → get `rowid` (u64 ID).
3. Call `index.remove(old_id)` for deleted rows; `index.add_with_ids(&vecs, &ids)` for new rows.
4. Commit transaction; write `index.write("tv_sources.tvim.tmp")` then `fs::rename` (atomic).

### Sources — `TurboVecStore::search(query_vec, k, filter)`

1. If `filter.source_ids` or `filter.since` is set: `SELECT id FROM chunks WHERE source_id IN (…) AND updated_at_ms >= ?` → `Vec<u64>` allowlist.
2. `index.search(query_vec, k, allowlist_or_none)` → `(scores, ids)`.
3. `SELECT * FROM chunks WHERE id IN (…)` → build `Vec<Hit>` sorted by score.

When no filter applies, allowlist is `None` and turbovec searches all vectors.

### Skills — `TurboSkillIndex::build_unified_index(patterns, workflows)`

1. Construct a fresh `IdMapIndex::new(dim, bit_width)` (replaces old index wholesale — simpler than incremental removal, safe because build is always a full rebuild).
2. Assign sequential u64 IDs (0..n).
3. `index.add_with_ids(&all_vecs, &ids)`.
4. Populate sidecar Vec with `SkillEntry` per item.
5. Swap new index into `Arc<Mutex<…>>`; write `.tvim.tmp` → rename; write `tv_skills_meta.json`.

### Skills — `TurboSkillIndex::search(query_vec, limit, item_type)`

1. `index.search(query_vec, limit * 4)` → `(scores, ids)` (4× over-fetch; skills corpus ≤ 500 items).
2. Filter results by `item_type` if Some; take first `limit`.
3. Map ids → sidecar Vec → `Vec<SearchResult>`.

No SQL needed for skills — corpus is small, item_type post-filter on Vec is negligible.

## SQLite Schema (sources)

```sql
CREATE TABLE IF NOT EXISTS chunks (
    id          INTEGER PRIMARY KEY,   -- turbovec u64 ID (rowid)
    chunk_id    TEXT    NOT NULL UNIQUE,
    source_id   TEXT    NOT NULL,
    external_id TEXT    NOT NULL,
    ordinal     INTEGER NOT NULL,
    text        TEXT    NOT NULL,
    heading_path TEXT   NOT NULL,      -- JSON-encoded string[]
    char_start  INTEGER NOT NULL,
    char_end    INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    vector      BLOB    NOT NULL       -- raw little-endian f32 bytes for index rebuild
);
CREATE INDEX IF NOT EXISTS idx_source    ON chunks(source_id);
CREATE INDEX IF NOT EXISTS idx_source_ext ON chunks(source_id, external_id);
PRAGMA journal_mode = WAL;
```

The `vector BLOB` column enables index rebuild from SQLite alone when `.tvim` is missing (crash recovery, migration).

## Error Handling

**Missing `.tvim` at startup** — if SQLite DB exists, rebuild `IdMapIndex` by reading all rows and calling `add_with_ids`. Logs `tracing::info!("rebuilding turbovec index from SQLite")`. If neither file exists, starts empty (first run or fresh install).

**`.tvim` write atomicity** — write to `*.tvim.tmp` then `fs::rename` (atomic on POSIX/macOS). Same for `tv_skills_meta.json`.

**Concurrent access** — `Arc<Mutex<IdMapIndex>>` + `Arc<Mutex<rusqlite::Connection>>`. SQLite WAL mode allows concurrent reads without blocking writes. Mutex held only during the index update + file write, not during the SQLite query phase.

**`mur internals reindex`** — `TurboSkillIndex::rebuild_index()` clears state and calls `build_unified_index`. `TurboVecStore::rebuild_index()` drops the SQLite table and `.tvim`, re-upserts from YAML source of truth.

## Configuration

```yaml
# ~/.mur/config.yaml
storage:
  vector_backend: turbovec   # "lancedb" (default) | "turbovec" | "qdrant"
  turbovec:
    bit_width: 4             # 2 (16× compression) or 4 (8× compression, better recall)
```

New `TurboVecConfig` sub-struct in `mur-common/src/config.rs` under `StorageConfig`.

## Cargo Feature

```toml
# mur-core/Cargo.toml
turbovec = { version = "0.9", optional = true }

[features]
turbovec = ["dep:turbovec"]
```

Factory arms for `"turbovec"` are `#[cfg(feature = "turbovec")]`. Attempting `vector_backend = "turbovec"` without the feature gives the same helpful error as the existing Qdrant gate. Default builds are unaffected — no compile cost.

## Files Changed

### New
| File | Purpose |
|------|---------|
| `mur-core/src/store/vector/turbo.rs` | `TurboVecStore` (VectorStore impl) |
| `mur-core/src/store/vector/turbo_skill.rs` | `TurboSkillIndex` + `SkillEntry` |
| `mur-core/src/store/vector/skill_index.rs` | `SkillIndex` trait |

### Modified
| File | Change |
|------|--------|
| `mur-core/Cargo.toml` | Add `turbovec` optional dep + feature |
| `mur-core/src/store/vector/mod.rs` | Add `pub mod turbo`, `turbo_skill`, `skill_index` |
| `mur-core/src/store/vector/factory.rs` | Add `get_skill_index()` fn; `"turbovec"` arms in both fns |
| `mur-core/src/store/vector/lancedb.rs` | Add `impl SkillIndex for LanceDbStore`; move `SearchResult` to `skill_index.rs` (re-export from lancedb for compat) |
| `mur-core/src/server/context.rs` | Use `get_skill_index()` instead of `LanceDbStore as VectorStore` |
| `mur-core/src/cmd/reindex.rs` | Same substitution |
| `mur-core/src/cmd/workflow.rs` | Same substitution |
| `mur-core/src/server/workflows.rs` | Same substitution |
| `mur-common/src/config.rs` | Add `TurboVecConfig`, `turbovec` field in `StorageConfig` |

## Testing

**`VectorStore` conformance suite** — `turbo.rs` adds `make_store_for_conformance()` fixture + `vector_store_conformance!` macro invocation. Covers upsert, search, delete, list, count with synthetic embeddings.

**`TurboSkillIndex` unit tests** (in `turbo_skill.rs`):
- `build_and_search_roundtrip` — 3 patterns + 1 workflow, top result is correct
- `item_type_filter` — `Some("workflow")` returns only workflows
- `rebuild_clears_old` — rebuild with different items, old items absent
- `persist_and_reload` — write + reconstruct, search still correct

**Factory tests** — `factory_returns_turbovec_when_configured` (behind `#[cfg(feature = "turbovec")]`), matching existing lancedb/qdrant factory test pattern.

All tests use synthetic `Vec<f32>` — no real embedding calls.

## Compression Numbers (reference)

At 1536-dim (OpenAI text-embedding-3-small):

| Corpus | Items | float32 | 4-bit turbovec |
|--------|-------|---------|----------------|
| Skills | 200 | ~1.2 MB | ~150 KB |
| Sources | 10,000 | ~59 MB | ~7.5 MB |
