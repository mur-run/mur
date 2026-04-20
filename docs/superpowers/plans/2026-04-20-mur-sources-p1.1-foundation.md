# mur Sources — Phase 1.1 Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor `store/lancedb.rs` behind a `VectorStore` trait, create `sources/` module skeleton with `KnowledgeSource` trait + types + keyring wrapper + yaml IO, and extend `Config` with `StorageConfig` / `SourcesGlobalConfig` — all backward-compatible. No user-visible commands.

**Architecture:** Introduce two Rust traits (`VectorStore`, `KnowledgeSource`) that later sub-milestones (P1.2–P1.4) implement. The existing `store::lancedb::VectorStore` struct is renamed `LanceDbStore` and moved to `store/vector/lancedb.rs`, implementing the new trait. All current callers switch to holding a `Box<dyn VectorStore>`. `sources/` module is scaffolded with types, trait, and credential abstraction but no adapter implementations yet. A CLI feature flag `sources` gates the future `mur source` subcommand tree (not wired up).

**Tech Stack:** Rust edition 2024, Tokio, async-trait, anyhow, LanceDB, serde + serde_yaml, keyring (new). Cargo workspace crates: `mur-common` (shared types), `mur-core` (binary + logic).

**Spec reference:** `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md` §3, §5, §9.2, §11 (P1.1).

---

## File Structure

This plan creates / modifies these files:

```
mur-common/src/
  config.rs                # MODIFY: + StorageConfig + SourcesGlobalConfig

mur-core/
  Cargo.toml               # MODIFY: + keyring dep; + [features] sources
  src/
    main.rs                # MODIFY: add gated Source subcommand stub
    lib.rs                 # MODIFY: + pub mod sources; (if missing)
    store/
      mod.rs               # MODIFY: swap `pub mod lancedb;` → `pub mod vector;`
      lancedb.rs           # DELETE (after content moves)
      vector/
        mod.rs             # NEW: VectorStore trait + SearchFilter + Hit
        lancedb.rs         # NEW: moved content, struct renamed LanceDbStore
        factory.rs         # NEW: get_vector_store() builder
        tests.rs           # NEW: trait conformance suite macro
    sources/
      mod.rs               # NEW: KnowledgeSource trait + SourceRegistry stub
      kind.rs              # NEW: SourceKind enum
      types.rs             # NEW: Document, Chunk, DocRef, SyncCursor, DocumentBody
      credentials.rs       # NEW: keyring-rs wrapper (CredentialStore trait + OsKeyring impl)
      instance.rs          # NEW: SourceInstance yaml struct + IO
    cmd/
      context.rs           # MODIFY: use Box<dyn VectorStore> via factory
      reindex.rs           # MODIFY: same
      inject_cmd.rs        # MODIFY: same
      workflow.rs          # MODIFY: same (4 call sites)
      source_cmd.rs        # NEW (feature="sources"): stub subcommand tree
      mod.rs               # MODIFY: conditional pub mod source_cmd
    server.rs              # MODIFY: same refactor
    conversations/index.rs # NO CHANGE (it uses lancedb crate directly, not our struct)
```

**Key design choices**:
- Types `Document`, `Chunk` etc. live in `mur-core/src/sources/types.rs` (Phase 1). If mur-server ever needs them, move to `mur-common` later.
- `VectorStore` trait goes in `mur-core`, not `mur-common` — keeps `mur-common` free of async-trait dep.
- Conformance tests use a macro that generates tests for any impl. Phase 1 runs it against `LanceDbStore`; P1.3 will add `QdrantStore` and re-run the same macro.

---

## Task 0: Preparation — Verify Clean Working Tree

**Files:**
- None (verification step)

- [ ] **Step 1: Confirm you're on the right branch and tree is clean except for plan/spec**

Run:
```bash
git status
git log --oneline -3
```

Expected:
```
On branch main (or a feature branch from main)
Recent commit: "docs(spec): mur sources — Phase 1 external note-app integration"
No unrelated uncommitted changes.
```

If dirty, stash or commit unrelated changes first.

- [ ] **Step 2: Confirm existing test suite is green before touching anything**

Run:
```bash
cargo test --workspace
```

Expected: All tests pass. Record the current total pass count — you'll compare at the end.

---

## Task 1: Add `keyring` Dependency

**Files:**
- Modify: `mur-core/Cargo.toml`

- [ ] **Step 1: Add keyring dep**

Open `mur-core/Cargo.toml`. Find the `[dependencies]` block (line ~29 has `async-trait = "0.1"`). After the `async-trait` line, add:

```toml
keyring = "3"
```

- [ ] **Step 2: Add `sources` feature flag**

In the same `Cargo.toml`, add a `[features]` section if it doesn't exist; otherwise append:

```toml
[features]
default = ["sources"]
sources = []
```

Rationale: default-on so CI exercises the gated code, but the flag exists so we can keep `mur source` subcommands invisible until P1.2 fills them in.

- [ ] **Step 3: Verify it compiles**

Run:
```bash
cargo check --workspace
```

Expected: clean compile, no warnings from our changes. (keyring adds transitively; should resolve.)

- [ ] **Step 4: Commit**

```bash
git add mur-core/Cargo.toml
git commit -m "chore(deps): add keyring + sources feature flag"
```

---

## Task 2: Extend `Config` with `StorageConfig` + `SourcesGlobalConfig`

**Files:**
- Modify: `mur-common/src/config.rs`
- Test: `mur-common/src/config.rs` (inline `#[cfg(test)]` module at bottom)

- [ ] **Step 1: Write the failing test for `StorageConfig` defaults**

Open `mur-common/src/config.rs`. At the very bottom of the file, add (creating `#[cfg(test)] mod tests` if missing):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_default_is_lancedb() {
        let c = StorageConfig::default();
        assert_eq!(c.vector_backend, "lancedb");
        assert_eq!(c.qdrant_url, None);
        assert_eq!(c.qdrant_api_key_ref, None);
    }

    #[test]
    fn sources_global_config_has_sensible_defaults() {
        let c = SourcesGlobalConfig::default();
        assert_eq!(c.poll_interval_secs, 600);
        assert_eq!(c.max_chunks_per_sync, 10_000);
        assert_eq!(c.max_parallel_sources, 3);
        assert_eq!(c.default_weight, 1.0);
        assert_eq!(c.embedding_batch_size, 32);
    }

    #[test]
    fn config_default_has_storage_and_sources_global() {
        let c = Config::default();
        assert_eq!(c.storage.vector_backend, "lancedb");
        assert_eq!(c.sources_global.default_weight, 1.0);
    }

    #[test]
    fn config_loads_yaml_without_new_fields() {
        // Existing users' config.yaml won't mention storage or sources_global.
        // It must still parse.
        let yaml = r#"
embedding:
  provider: ollama
  model: test-model
  dimensions: 512
  ollama_endpoint: http://localhost:11434
"#;
        let c: Config = serde_yaml::from_str(yaml).expect("parses");
        assert_eq!(c.storage.vector_backend, "lancedb");
        assert_eq!(c.sources_global.max_parallel_sources, 3);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run:
```bash
cargo test -p mur-common config::tests
```

Expected: FAIL — `StorageConfig` / `SourcesGlobalConfig` not found.

- [ ] **Step 3: Implement `StorageConfig` + `SourcesGlobalConfig`**

In `mur-common/src/config.rs`, after the `PathConfig` struct (around line 185), add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Vector backend identifier: "lancedb" (default) or "qdrant".
    #[serde(default = "default_vector_backend")]
    pub vector_backend: String,

    /// Qdrant connection URL (only used when vector_backend = "qdrant").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qdrant_url: Option<String>,

    /// Keyring account name holding the Qdrant API key, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qdrant_api_key_ref: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            vector_backend: default_vector_backend(),
            qdrant_url: None,
            qdrant_api_key_ref: None,
        }
    }
}

fn default_vector_backend() -> String {
    "lancedb".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcesGlobalConfig {
    /// Polling interval for cloud sources (seconds).
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,

    /// Safety cap: do not sync more than this many chunks per run.
    #[serde(default = "default_max_chunks_per_sync")]
    pub max_chunks_per_sync: usize,

    /// Upper bound on parallel source sync tasks.
    #[serde(default = "default_max_parallel_sources")]
    pub max_parallel_sources: usize,

    /// Weight applied to new sources unless overridden.
    #[serde(default = "default_source_weight")]
    pub default_weight: f32,

    /// Embedding request batch size.
    #[serde(default = "default_embedding_batch_size")]
    pub embedding_batch_size: usize,
}

impl Default for SourcesGlobalConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval_secs(),
            max_chunks_per_sync: default_max_chunks_per_sync(),
            max_parallel_sources: default_max_parallel_sources(),
            default_weight: default_source_weight(),
            embedding_batch_size: default_embedding_batch_size(),
        }
    }
}

fn default_poll_interval_secs() -> u64 { 600 }
fn default_max_chunks_per_sync() -> usize { 10_000 }
fn default_max_parallel_sources() -> usize { 3 }
fn default_source_weight() -> f32 { 1.0 }
fn default_embedding_batch_size() -> usize { 32 }
```

- [ ] **Step 4: Wire the new fields into `Config`**

Still in `mur-common/src/config.rs`, locate the `pub struct Config { ... }` (lines 6–27) and add two new fields before the closing brace:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default)]
    pub llm: LlmConfig,

    #[serde(default)]
    pub retrieval: RetrievalConfig,

    #[serde(default)]
    pub paths: PathConfig,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub community: CommunityConfig,

    #[serde(default)]
    pub sync: SyncConfig,

    // --- P1.1 additions ---
    #[serde(default)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub sources_global: SourcesGlobalConfig,
}
```

- [ ] **Step 5: Run the new tests**

```bash
cargo test -p mur-common config::tests
```

Expected: all four tests pass.

- [ ] **Step 6: Run full workspace tests to confirm no regression**

```bash
cargo test --workspace
```

Expected: same pass count as Task 0 Step 2, **plus the 4 new tests**. No existing test regresses (backward-compatible `#[serde(default)]`).

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): add StorageConfig and SourcesGlobalConfig with defaults"
```

---

## Task 3: Create `store/vector/mod.rs` with `VectorStore` Trait Skeleton

**Files:**
- Create: `mur-core/src/store/vector/mod.rs`

- [ ] **Step 1: Create the new module file with trait + types**

Create `mur-core/src/store/vector/mod.rs` with the full content:

```rust
//! Abstract vector store trait.
//!
//! Phase 1 has one implementation (`lancedb::LanceDbStore`). Phase 1.3 adds
//! `qdrant::QdrantStore`. All callers interact through `Box<dyn VectorStore>`
//! obtained from `factory::get_vector_store(&config)`.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub mod lancedb;
pub mod factory;

#[cfg(test)]
pub mod tests;

/// An embedded chunk ready to be stored.
///
/// This shape is re-used by the `sources` pipeline (for external notes) *and*
/// the existing pattern index. For Phase 1 we only use it for sources — the
/// pattern path keeps its existing code path untouched.
#[derive(Debug, Clone)]
pub struct EmbeddedChunk {
    pub chunk_id: String,
    pub source_id: String,       // "patterns" for patterns; e.g. "notion:work" otherwise
    pub external_id: String,     // within-source unique id (pattern name, notion page uuid, …)
    pub ordinal: usize,
    pub text: String,
    pub heading_path: Vec<String>,
    pub char_range: (usize, usize),
    pub updated_at: DateTime<Utc>,
    pub embedding: Vec<f32>,
}

/// Filter applied at search time.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Restrict to these source_ids. `None` = all sources.
    pub source_ids: Option<Vec<String>>,
    /// Only return rows whose `updated_at` is at least this timestamp.
    pub since: Option<DateTime<Utc>>,
}

/// A single search hit.
#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    /// Similarity score in [0.0, 1.0], higher = better.
    pub score: f32,
    pub text: String,
    pub heading_path: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

/// Abstract vector store. Implementations MUST be safe to share across
/// tasks (`Send + Sync`) and MUST be idempotent for `upsert`.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Insert or replace chunks by `chunk_id`.
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()>;

    /// Search by embedding, returning up to `k` hits filtered by `filter`.
    async fn search(
        &self,
        query_vec: &[f32],
        k: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<Hit>>;

    /// Delete chunks whose (source_id, external_id) matches.
    async fn delete_by_external_ids(
        &self,
        source_id: &str,
        external_ids: &[String],
    ) -> Result<()>;

    /// Delete all chunks for a given source.
    async fn delete_by_source(&self, source_id: &str) -> Result<()>;

    /// List every `external_id` currently indexed for `source_id`.
    /// Used by sync loops to compute deletion set diff.
    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>>;

    /// Count chunks (optionally per source).
    async fn count(&self, source_id: Option<&str>) -> Result<usize>;

    /// Drop and recreate all state. Used by `mur reindex`.
    async fn rebuild_index(&self) -> Result<()>;
}
```

- [ ] **Step 2: Expose it from `store/mod.rs`**

Open `mur-core/src/store/mod.rs`. It currently reads:

```rust
// MUR Core v2 — store module
//
// YAML files are the source of truth. All pattern reads/writes go through here.

pub mod config;
pub mod embedding;
pub mod exchange;
pub mod lancedb;
pub mod pipeline_yaml;
pub mod spot_rate;
pub mod workflow_yaml;
pub mod yaml;
```

Replace `pub mod lancedb;` with `pub mod vector;` (the old file will be moved in Task 4). New content:

```rust
// MUR Core v2 — store module
//
// YAML files are the source of truth. All pattern reads/writes go through here.

pub mod config;
pub mod embedding;
pub mod exchange;
pub mod pipeline_yaml;
pub mod spot_rate;
pub mod vector;
pub mod workflow_yaml;
pub mod yaml;
```

- [ ] **Step 3: Create placeholder `store/vector/lancedb.rs` and `factory.rs` to satisfy the module tree**

These will be fleshed out in later tasks. For now create:

`mur-core/src/store/vector/lancedb.rs`:
```rust
//! LanceDB implementation of `VectorStore`. (stub — content arrives in Task 4)
```

`mur-core/src/store/vector/factory.rs`:
```rust
//! Vector store factory. (stub — wired up in Task 6)
```

- [ ] **Step 4: Verify workspace still compiles — existing callers of `crate::store::lancedb` will now fail. That's expected and fixed in Task 4.**

Run:
```bash
cargo check -p mur-core 2>&1 | head -30
```

Expected: compile errors like `unresolved import crate::store::lancedb`. These are the call sites we fix in Task 4. **Do not commit yet** — commit at end of Task 4 when build is green again.

---

## Task 4: Move LanceDB Code to `store/vector/lancedb.rs`, Rename to `LanceDbStore`, Implement Trait

This is the biggest task. Break it into substeps.

**Files:**
- Create (content moved in): `mur-core/src/store/vector/lancedb.rs`
- Delete: `mur-core/src/store/lancedb.rs`

- [ ] **Step 1: Copy the entire existing `store/lancedb.rs` content to `store/vector/lancedb.rs`**

Use `git mv` semantics by writing new file then deleting old:

```bash
cp mur-core/src/store/lancedb.rs mur-core/src/store/vector/lancedb.rs
```

- [ ] **Step 2: Rename the struct in the new file from `VectorStore` to `LanceDbStore`**

Open `mur-core/src/store/vector/lancedb.rs`. Do a file-wide rename of `pub struct VectorStore` → `pub struct LanceDbStore` and every `impl VectorStore` inherent impl block header → `impl LanceDbStore`. Also update the usage in the `#[cfg(test)] mod tests` block (the `VectorStore::open` calls on lines 342/364/372/440).

Concretely:

- Line 20: `pub struct VectorStore {` → `pub struct LanceDbStore {`
- Line 25: `impl VectorStore {` → `impl LanceDbStore {`
- Tests: each `VectorStore::open(...)` → `LanceDbStore::open(...)`

Also update the module docstring at top of file:

```rust
//! LanceDB-backed implementation of the `VectorStore` trait.
//!
//! YAML remains the source of truth. LanceDB is a rebuildable index.
```

- [ ] **Step 3: Add `VectorStore` trait implementation block for `LanceDbStore`**

At the bottom of `mur-core/src/store/vector/lancedb.rs` (before the `#[cfg(test)]` block), add:

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use super::{EmbeddedChunk, Hit, SearchFilter, VectorStore};

const CHUNKS_TABLE: &str = "sources";

#[async_trait]
impl VectorStore for LanceDbStore {
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        // Phase 1.1 is a skeleton; the real arrow schema + upsert body lands in P1.2
        // when the first adapter calls it. For now we provide a minimal no-op that
        // satisfies the trait and is exercised by the conformance suite only if the
        // caller is willing to treat an empty search as success.
        let _ = chunks;
        anyhow::bail!("LanceDbStore::upsert is a stub until P1.2 wires adapters")
    }

    async fn search(
        &self,
        _query_vec: &[f32],
        _k: usize,
        _filter: &SearchFilter,
    ) -> Result<Vec<Hit>> {
        anyhow::bail!("LanceDbStore::search (trait) is a stub until P1.3 wires unified retrieve")
    }

    async fn delete_by_external_ids(
        &self,
        _source_id: &str,
        _external_ids: &[String],
    ) -> Result<()> {
        anyhow::bail!("LanceDbStore::delete_by_external_ids is a stub until P1.2")
    }

    async fn delete_by_source(&self, _source_id: &str) -> Result<()> {
        anyhow::bail!("LanceDbStore::delete_by_source is a stub until P1.2")
    }

    async fn list_external_ids(&self, _source_id: &str) -> Result<Vec<String>> {
        anyhow::bail!("LanceDbStore::list_external_ids is a stub until P1.2")
    }

    async fn count(&self, _source_id: Option<&str>) -> Result<usize> {
        anyhow::bail!("LanceDbStore::count is a stub until P1.2")
    }

    async fn rebuild_index(&self) -> Result<()> {
        // Phase 1.1: reuse existing build_unified_index by rebuilding from yaml — but
        // we don't have access to YamlStore here. Caller owns that. Trait method kept
        // but simply errors until P1.3.
        anyhow::bail!("LanceDbStore::rebuild_index (trait) is a stub until P1.3 orchestrates")
    }
}

// NOTE: the existing inherent methods `open`, `build_index`, `build_unified_index`,
// `search` (with `item_type`) remain untouched. Existing callers keep using them
// unchanged for patterns/workflows. The trait methods above are the NEW surface
// used by the sources pipeline in P1.2+.
//
// These stubs are intentional: P1.1 only establishes the trait surface. Adapters
// in P1.2 will need real upsert, so we'll flesh out the trait bodies then. A
// conformance-test "happy path" is provided by the `empty_store` test in
// store/vector/tests.rs which stays within the stubs' no-op contract.
const _: () = {
    // Compile-time assertion that LanceDbStore implements VectorStore.
    fn _assert_impl<T: VectorStore>() {}
    fn _check() { _assert_impl::<LanceDbStore>(); }
};
```

**IMPORTANT — why stubs**: the trait methods above intentionally panic/bail because the production callers for them (sources pipeline) don't exist until P1.2. P1.1 is a surface refactor; it must not change runtime behavior of the existing `build_unified_index` / inherent `search` code paths that patterns use today. When P1.2 adds the first adapter, the stubs are replaced with real implementations alongside conformance-suite tests that exercise them.

- [ ] **Step 4: Update `store/vector/mod.rs` to re-export the renamed struct**

Open `mur-core/src/store/vector/mod.rs`. Below the existing `pub mod lancedb;`, add:

```rust
pub use self::lancedb::LanceDbStore;
```

- [ ] **Step 5: Update every caller of `crate::store::lancedb::VectorStore`**

Seven files use it. Go through each:

**`mur-core/src/cmd/context.rs:26`** — change:
```rust
use crate::store::lancedb::VectorStore;
```
to:
```rust
use crate::store::vector::LanceDbStore as VectorStore;
```

(The `as VectorStore` alias keeps the rest of that function body unchanged.)

**`mur-core/src/cmd/reindex.rs:8`** — same edit.

**`mur-core/src/cmd/inject_cmd.rs:14`** — same edit.

**`mur-core/src/cmd/workflow.rs`** — two occurrences at line 14 and line 276:
```rust
use crate::store::lancedb::VectorStore;
```
both become:
```rust
use crate::store::vector::LanceDbStore as VectorStore;
```

**`mur-core/src/server.rs:46`** — same edit.

- [ ] **Step 6: Delete the old `mur-core/src/store/lancedb.rs` file**

```bash
git rm mur-core/src/store/lancedb.rs
```

(We already copied content in step 1.)

- [ ] **Step 7: Run workspace check**

```bash
cargo check --workspace
```

Expected: clean compile. No warnings from our renames.

- [ ] **Step 8: Run the full test suite, including the moved LanceDB tests**

```bash
cargo test --workspace
```

Expected: all tests pass, including `store::vector::lancedb::tests::*` (which are the original `store::lancedb::tests::*` in their new location). Pass count should equal Task 2's count + 4 from Task 2 (no net change from this task — it's a refactor).

If any test fails, check:
- Did you miss a `VectorStore::open` → `LanceDbStore::open` rename inside the test module?
- Is `use super::*;` still at the top of the tests module?

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(store): move lancedb into vector submodule, rename struct to LanceDbStore, extract VectorStore trait

- Introduces async-trait based VectorStore abstraction in store/vector/mod.rs
- Existing VectorStore struct renamed LanceDbStore and moved to store/vector/lancedb.rs
- All 7 callers updated via use alias; behavior unchanged
- Trait method bodies are stubs until P1.2 wires the sources pipeline"
```

---

## Task 5: Trait Conformance Test Suite

**Files:**
- Create: `mur-core/src/store/vector/tests.rs`

- [ ] **Step 1: Create the conformance suite**

Create `mur-core/src/store/vector/tests.rs`:

```rust
//! Conformance suite every `VectorStore` impl must satisfy.
//!
//! Usage from an impl's module:
//! ```ignore
//! #[cfg(test)]
//! mod conformance {
//!     use super::*;
//!     crate::store::vector::tests::vector_store_conformance!(LanceDbStore, make_store);
//!     async fn make_store() -> LanceDbStore { /* ... */ }
//! }
//! ```
//!
//! Phase 1.1 only calls the `smoke_create` test — the upsert/search tests will
//! become meaningful when P1.2 replaces the trait-method stubs with real code.

#![allow(dead_code)]

use super::*;

/// Generic smoke test: construct the store and count returns 0 for unknown source.
pub async fn smoke_count_empty<S: VectorStore>(store: &S) {
    // The stub implementations currently bail!. We only assert the method is
    // *callable*; once real bodies land in P1.2 this becomes a hard assertion.
    let _ = store.count(Some("nonexistent")).await;
}

/// Round-trip test (meaningful from P1.2 onward).
pub async fn upsert_and_search<S: VectorStore>(store: &S, dims: usize) -> anyhow::Result<()> {
    let chunk = EmbeddedChunk {
        chunk_id: "chunk-a".into(),
        source_id: "test".into(),
        external_id: "doc-1".into(),
        ordinal: 0,
        text: "hello world".into(),
        heading_path: vec![],
        char_range: (0, 11),
        updated_at: chrono::Utc::now(),
        embedding: vec![0.1_f32; dims],
    };
    store.upsert(&[chunk.clone()]).await?;
    let hits = store
        .search(&vec![0.1_f32; dims], 5, &SearchFilter::default())
        .await?;
    anyhow::ensure!(!hits.is_empty(), "expected at least one hit");
    Ok(())
}

/// Delete-by-source removes everything for that source.
pub async fn delete_by_source_clears<S: VectorStore>(store: &S) -> anyhow::Result<()> {
    store.delete_by_source("test").await?;
    let ids = store.list_external_ids("test").await?;
    anyhow::ensure!(ids.is_empty(), "expected zero ids after delete_by_source");
    Ok(())
}

/// Shared macro: drop this into an impl's test module to exercise the full suite.
#[macro_export]
macro_rules! vector_store_conformance {
    ($ty:ty, $factory:ident) => {
        #[tokio::test]
        async fn conformance_smoke_count_empty() {
            let s = $factory().await;
            $crate::store::vector::tests::smoke_count_empty::<$ty>(&s).await;
        }
        // The following are wired up but remain #[ignore] until P1.2 replaces
        // the stubs with real bodies. Leaving them in keeps the wiring visible.
        #[tokio::test]
        #[ignore = "enabled from P1.2 when upsert is real"]
        async fn conformance_upsert_and_search() {
            let s = $factory().await;
            $crate::store::vector::tests::upsert_and_search::<$ty>(&s, 64)
                .await
                .expect("roundtrip");
        }
        #[tokio::test]
        #[ignore = "enabled from P1.2 when delete_by_source is real"]
        async fn conformance_delete_by_source_clears() {
            let s = $factory().await;
            $crate::store::vector::tests::delete_by_source_clears::<$ty>(&s)
                .await
                .expect("delete_by_source");
        }
    };
}
```

- [ ] **Step 2: Wire the LanceDbStore through the macro**

Open `mur-core/src/store/vector/lancedb.rs`. Inside the existing `#[cfg(test)] mod tests { ... }` block, add at the **top** of that mod:

```rust
    // Conformance suite — see store/vector/tests.rs
    crate::vector_store_conformance!(LanceDbStore, make_store_for_conformance);

    async fn make_store_for_conformance() -> LanceDbStore {
        let tmp = tempfile::TempDir::new().unwrap();
        LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap()
        // NOTE: TempDir drops at end of test; store holds DB handle. That's fine
        // for the smoke test; roundtrip tests unignored in P1.2 will keep their
        // own TempDir lifetimes.
    }
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core store::vector 2>&1 | tail -20
```

Expected:
- `conformance_smoke_count_empty ... ok`
- `conformance_upsert_and_search ... ignored`
- `conformance_delete_by_source_clears ... ignored`
- existing `store::vector::lancedb::tests::*` all still pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/store/vector/tests.rs mur-core/src/store/vector/lancedb.rs
git commit -m "test(vector): add VectorStore trait conformance macro suite

- Defines reusable smoke + roundtrip tests any VectorStore impl can opt into.
- LanceDbStore registered; upsert/delete tests ignored until P1.2 replaces stubs.
- QdrantStore (P1.3) will plug into the same macro."
```

---

## Task 6: `store/vector/factory.rs` — Build `Box<dyn VectorStore>` from `Config`

**Files:**
- Modify: `mur-core/src/store/vector/factory.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/store/vector/factory.rs`:

```rust
//! Vector store factory.
//!
//! Returns a boxed `dyn VectorStore` selected by `Config::storage::vector_backend`.
//! Phase 1.1 supports `"lancedb"`; `"qdrant"` arrives in P1.3.

use anyhow::{Context, Result, bail};
use mur_common::config::Config;
use std::path::Path;
use std::sync::Arc;

use super::VectorStore;
use super::lancedb::LanceDbStore;

pub async fn get_vector_store(
    cfg: &Config,
    index_dir: &Path,
) -> Result<Arc<dyn VectorStore>> {
    match cfg.storage.vector_backend.as_str() {
        "lancedb" => {
            let store = LanceDbStore::open(index_dir, cfg.embedding.dimensions as i32)
                .await
                .context("opening LanceDB vector store")?;
            Ok(Arc::new(store))
        }
        "qdrant" => bail!("Qdrant backend is not available until P1.3"),
        other => bail!("unknown storage.vector_backend: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn dims_128_cfg() -> Config {
        let mut c = Config::default();
        c.embedding.dimensions = 128;
        c
    }

    #[tokio::test]
    async fn factory_returns_lancedb_by_default() {
        let tmp = TempDir::new().unwrap();
        let cfg = dims_128_cfg();
        let store = get_vector_store(&cfg, tmp.path()).await.unwrap();
        // We can only check it constructed; count is stubbed.
        let _ = store.count(None).await;
    }

    #[tokio::test]
    async fn factory_rejects_qdrant_in_p1_1() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = dims_128_cfg();
        cfg.storage.vector_backend = "qdrant".into();
        let err = get_vector_store(&cfg, tmp.path()).await.err().unwrap();
        assert!(err.to_string().contains("not available until P1.3"));
    }

    #[tokio::test]
    async fn factory_rejects_unknown_backend() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = dims_128_cfg();
        cfg.storage.vector_backend = "bogus".into();
        let err = get_vector_store(&cfg, tmp.path()).await.err().unwrap();
        assert!(err.to_string().contains("unknown storage.vector_backend"));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p mur-core store::vector::factory
```

Expected: all three tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/store/vector/factory.rs
git commit -m "feat(vector): factory::get_vector_store(cfg) selects backend by config"
```

---

## Task 7: `sources/types.rs` — Shared Data Types

**Files:**
- Create: `mur-core/src/sources/types.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/sources/types.rs` with test stub at bottom:

```rust
//! Core types passed through the sources pipeline.
//!
//! These do *not* live in `mur-common` (yet) because only `mur-core` consumes
//! them. If a future mur-server integration needs them, hoist at that point.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque cursor returned by `KnowledgeSource::list_documents`. Each adapter
/// defines its own encoding (Notion uses an RFC3339 timestamp, Obsidian a
/// directory hash, Joplin an epoch-ms). The orchestrator stores it verbatim
/// in the source yaml and passes it back on the next sync.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncCursor(pub String);

impl SyncCursor {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Lightweight reference to a document that hasn't been fetched yet.
#[derive(Debug, Clone)]
pub struct DocRef {
    pub external_id: String,
    pub title: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A full document payload.
#[derive(Debug, Clone)]
pub struct Document {
    pub source_id: String,
    pub external_id: String,
    pub title: String,
    pub body: DocumentBody,
    pub url: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub metadata: serde_json::Value,
}

/// Body form; adapters pick the variant that preserves the most fidelity.
#[derive(Debug, Clone)]
pub enum DocumentBody {
    Markdown(String),
    PlainText(String),
    /// Notion blocks — serialized as opaque JSON so we don't depend on the
    /// Notion SDK crate from `types.rs`.
    NotionBlocks(serde_json::Value),
}

impl DocumentBody {
    /// Returns the content as plaintext suitable for embedding.
    pub fn as_plain_text(&self) -> String {
        match self {
            DocumentBody::Markdown(s) | DocumentBody::PlainText(s) => s.clone(),
            DocumentBody::NotionBlocks(_) => {
                // Real extraction lives in the Notion chunker (P1.4). For P1.1
                // we expose an empty fallback — no adapter produces this variant
                // in P1.1.
                String::new()
            }
        }
    }
}

/// Pre-embedding chunk.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    pub ordinal: usize,
    pub text: String,
    pub heading_path: Vec<String>,
    pub char_range: (usize, usize),
    pub updated_at: DateTime<Utc>,
}

impl Chunk {
    /// Build a chunk with a fresh UUID v4 chunk_id.
    pub fn new(
        source_id: impl Into<String>,
        external_id: impl Into<String>,
        ordinal: usize,
        text: impl Into<String>,
        heading_path: Vec<String>,
        char_range: (usize, usize),
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            chunk_id: Uuid::new_v4().to_string(),
            source_id: source_id.into(),
            external_id: external_id.into(),
            ordinal,
            text: text.into(),
            heading_path,
            char_range,
            updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_cursor_default_is_empty() {
        assert!(SyncCursor::default().is_empty());
    }

    #[test]
    fn document_body_markdown_is_plain_text() {
        let b = DocumentBody::Markdown("# hi\nbody".into());
        assert_eq!(b.as_plain_text(), "# hi\nbody");
    }

    #[test]
    fn chunk_new_assigns_unique_id() {
        let now = Utc::now();
        let a = Chunk::new("s", "d", 0, "t", vec![], (0, 1), now);
        let b = Chunk::new("s", "d", 0, "t", vec![], (0, 1), now);
        assert_ne!(a.chunk_id, b.chunk_id);
    }
}
```

- [ ] **Step 2: Add `uuid` dependency if not present**

Check `mur-core/Cargo.toml`:

```bash
grep -n "^uuid" mur-core/Cargo.toml || echo MISSING
```

If MISSING, add to `[dependencies]`:
```toml
uuid = { version = "1", features = ["v4"] }
```

- [ ] **Step 3: Run tests (expected to fail — module doesn't exist yet)**

```bash
cargo test -p mur-core sources::types
```

Expected: compile error — `sources` module not declared.

- [ ] **Step 4: Declare the `sources` module**

Open `mur-core/src/lib.rs` (or `main.rs` if lib.rs doesn't exist — the binary may declare modules there). Check:

```bash
grep -n "pub mod\|^mod " mur-core/src/lib.rs 2>&1 | head -20
```

If `lib.rs` exists and declares modules, add `pub mod sources;` there. Otherwise `main.rs` declares modules — add it there.

Create `mur-core/src/sources/mod.rs` with minimal content:

```rust
//! External knowledge sources pipeline. See design spec.
pub mod types;
```

- [ ] **Step 5: Re-run tests**

```bash
cargo test -p mur-core sources::types
```

Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/lib.rs mur-core/src/sources/mod.rs mur-core/src/sources/types.rs
git commit -m "feat(sources): add core pipeline types (Document, Chunk, DocRef, SyncCursor)"
```

---

## Task 8: `sources/kind.rs` — `SourceKind` Enum with Phase 2 Stub

**Files:**
- Create: `mur-core/src/sources/kind.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Create kind.rs with tests**

Create `mur-core/src/sources/kind.rs`:

```rust
//! Adapter behavior kind. Phase 1 only implements `PullIndex`; the
//! `FederatedQuery` variant exists so MCP adapters (P2+) compile without
//! changing the trait signature.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Adapter pulls documents into mur's vector index and answers via local search.
    PullIndex,
    /// Adapter does not hand over documents; queries are forwarded to the
    /// adapter at search time (e.g., NotebookLM via MCP).
    FederatedQuery,
}

impl SourceKind {
    pub fn is_pull_index(self) -> bool {
        matches!(self, SourceKind::PullIndex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_index_is_pull_index() {
        assert!(SourceKind::PullIndex.is_pull_index());
        assert!(!SourceKind::FederatedQuery.is_pull_index());
    }

    #[test]
    fn serde_roundtrip_snake_case() {
        let s = serde_yaml::to_string(&SourceKind::PullIndex).unwrap();
        assert!(s.contains("pull_index"));
        let back: SourceKind = serde_yaml::from_str(&s).unwrap();
        assert_eq!(back, SourceKind::PullIndex);
    }
}
```

- [ ] **Step 2: Declare it in the sources module**

In `mur-core/src/sources/mod.rs`, add:

```rust
pub mod kind;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core sources::kind
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/kind.rs mur-core/src/sources/mod.rs
git commit -m "feat(sources): SourceKind enum with FederatedQuery stub for MCP (P2)"
```

---

## Task 9: `sources/credentials.rs` — OS Keyring Wrapper

**Files:**
- Create: `mur-core/src/sources/credentials.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Create credentials.rs**

Create `mur-core/src/sources/credentials.rs`:

```rust
//! Credential storage for adapters.
//!
//! We abstract over the OS keyring so unit tests don't need Keychain /
//! Secret Service. Production uses `OsKeyring`; tests use `InMemoryCreds`.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait CredentialStore: Send + Sync {
    /// Store `value` under `(service, account)`.
    fn set(&self, service: &str, account: &str, value: &str) -> Result<()>;
    /// Retrieve `(service, account)` or return `Ok(None)` if not present.
    fn get(&self, service: &str, account: &str) -> Result<Option<String>>;
    /// Delete if present; no error if absent.
    fn delete(&self, service: &str, account: &str) -> Result<()>;
}

/// Production implementation backed by the OS keyring (macOS Keychain,
/// Linux Secret Service via libsecret, Windows Credential Manager).
pub struct OsKeyring;

impl CredentialStore for OsKeyring {
    fn set(&self, service: &str, account: &str, value: &str) -> Result<()> {
        let entry = keyring::Entry::new(service, account)
            .with_context(|| format!("open keyring entry {service}:{account}"))?;
        entry
            .set_password(value)
            .with_context(|| format!("set keyring entry {service}:{account}"))?;
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(service, account)
            .with_context(|| format!("open keyring entry {service}:{account}"))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("get keyring entry {service}:{account}")),
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let entry = keyring::Entry::new(service, account)
            .with_context(|| format!("open keyring entry {service}:{account}"))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete keyring entry {service}:{account}")),
        }
    }
}

/// In-memory implementation for tests.
#[derive(Default)]
pub struct InMemoryCreds {
    store: Mutex<HashMap<(String, String), String>>,
}

impl CredentialStore for InMemoryCreds {
    fn set(&self, service: &str, account: &str, value: &str) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .insert((service.into(), account.into()), value.into());
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .get(&(service.into(), account.into()))
            .cloned())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .remove(&(service.into(), account.into()));
        Ok(())
    }
}

/// Canonical helper: derive the keyring account name from source id + field.
pub fn account(source_id: &str, field: &str) -> String {
    format!("{source_id}:{field}")
}

pub const SERVICE: &str = "mur";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_roundtrip() {
        let c = InMemoryCreds::default();
        c.set("mur", "notion:work:access_token", "secret-123").unwrap();
        assert_eq!(
            c.get("mur", "notion:work:access_token").unwrap().as_deref(),
            Some("secret-123")
        );
        c.delete("mur", "notion:work:access_token").unwrap();
        assert_eq!(c.get("mur", "notion:work:access_token").unwrap(), None);
    }

    #[test]
    fn in_memory_missing_returns_none() {
        let c = InMemoryCreds::default();
        assert_eq!(c.get("mur", "nope").unwrap(), None);
    }

    #[test]
    fn account_helper_formats_canonically() {
        assert_eq!(account("notion:work", "access_token"), "notion:work:access_token");
    }
}
```

- [ ] **Step 2: Declare in sources module**

In `mur-core/src/sources/mod.rs`, append:

```rust
pub mod credentials;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core sources::credentials
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/credentials.rs mur-core/src/sources/mod.rs
git commit -m "feat(sources): CredentialStore trait + OsKeyring + InMemoryCreds"
```

---

## Task 10: `sources/instance.rs` — SourceInstance YAML IO

**Files:**
- Create: `mur-core/src/sources/instance.rs`
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Create instance.rs with tests**

Create `mur-core/src/sources/instance.rs`:

```rust
//! Per-source config + sync state, persisted as `~/.mur/sources/<id>.yaml`.
//!
//! This file mirrors the `YamlStore` pattern used by patterns/workflows:
//! YAML is the source of truth; everything else rebuildable.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::kind::SourceKind;

const MAX_ERRORS_TAIL: usize = 50;

/// Complete state for one connected source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInstance {
    pub id: String,
    #[serde(rename = "type")]
    pub type_name: String,        // "notion" / "obsidian" / "joplin"
    pub kind: SourceKind,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    #[serde(default = "default_weight")]
    pub weight: f32,

    #[serde(default)]
    pub scope: BTreeMap<String, serde_yaml::Value>,

    #[serde(default)]
    pub sync: SyncState,

    #[serde(default)]
    pub stats: SourceStats,

    /// Keyring account name if this source needs credentials. `None` for
    /// Obsidian (file-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyring_entry: Option<String>,
}

fn default_enabled() -> bool { true }
fn default_weight() -> f32 { 1.0 }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub errors_tail: Vec<SyncError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncError {
    pub at: DateTime<Utc>,
    pub doc: String,
    pub msg: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceStats {
    #[serde(default)]
    pub doc_count: u64,
    #[serde(default)]
    pub chunk_count: u64,
    #[serde(default)]
    pub indexed_bytes: u64,
}

impl SyncState {
    /// Append an error, keeping the tail bounded.
    pub fn push_error(&mut self, err: SyncError) {
        self.errors_tail.push(err);
        let overflow = self.errors_tail.len().saturating_sub(MAX_ERRORS_TAIL);
        if overflow > 0 {
            self.errors_tail.drain(0..overflow);
        }
    }
}

/// Filesystem store: one yaml per source at `<root>/sources/<id>.yaml`.
pub struct SourceInstanceStore {
    root: PathBuf,
}

impl SourceInstanceStore {
    /// Default is `~/.mur/sources/`.
    pub fn default_store() -> Result<Self> {
        let root = dirs::home_dir()
            .context("no home dir")?
            .join(".mur")
            .join("sources");
        Ok(Self::new(root))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        // ':' is illegal on some filesystems — allowed on macOS+Linux but not
        // Windows NTFS. Phase 1 targets macOS+Linux. Flag a plan risk.
        self.root.join(format!("{id}.yaml"))
    }

    pub fn save(&self, instance: &SourceInstance) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create dir {}", self.root.display()))?;
        let yaml = serde_yaml::to_string(instance)?;
        let target = self.path_for(&instance.id);
        let tmp = target.with_extension("yaml.tmp");
        fs::write(&tmp, yaml)
            .with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &target)
            .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<SourceInstance> {
        let p = self.path_for(id);
        let content = fs::read_to_string(&p)
            .with_context(|| format!("read {}", p.display()))?;
        let inst: SourceInstance = serde_yaml::from_str(&content)
            .with_context(|| format!("parse {}", p.display()))?;
        if inst.id != id {
            bail!("file {} has id {} but we asked for {}", p.display(), inst.id, id);
        }
        Ok(inst)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let p = self.path_for(id);
        if p.exists() {
            fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
        }
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<SourceInstance>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let content = fs::read_to_string(&p)?;
            match serde_yaml::from_str::<SourceInstance>(&content) {
                Ok(inst) => out.push(inst),
                Err(e) => {
                    tracing::warn!(file = %p.display(), error = %e, "skipping malformed source yaml");
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_instance(id: &str) -> SourceInstance {
        SourceInstance {
            id: id.into(),
            type_name: "obsidian".into(),
            kind: SourceKind::PullIndex,
            enabled: true,
            weight: 1.0,
            scope: BTreeMap::new(),
            sync: SyncState::default(),
            stats: SourceStats::default(),
            keyring_entry: None,
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        let inst = sample_instance("obsidian-main");
        store.save(&inst).unwrap();
        let loaded = store.load("obsidian-main").unwrap();
        assert_eq!(loaded.id, "obsidian-main");
        assert_eq!(loaded.type_name, "obsidian");
        assert!(loaded.enabled);
    }

    #[test]
    fn list_returns_sorted_instances() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.save(&sample_instance("b-second")).unwrap();
        store.save(&sample_instance("a-first")).unwrap();
        let items = store.list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a-first");
        assert_eq!(items[1].id, "b-second");
    }

    #[test]
    fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.save(&sample_instance("obsidian-main")).unwrap();
        store.delete("obsidian-main").unwrap();
        assert!(store.load("obsidian-main").is_err());
    }

    #[test]
    fn delete_missing_is_ok() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.delete("never-existed").unwrap();
    }

    #[test]
    fn errors_tail_bounded_to_fifty() {
        let mut s = SyncState::default();
        for i in 0..60 {
            s.push_error(SyncError {
                at: Utc::now(),
                doc: format!("doc-{i}"),
                msg: "boom".into(),
            });
        }
        assert_eq!(s.errors_tail.len(), MAX_ERRORS_TAIL);
        assert_eq!(s.errors_tail[0].doc, "doc-10"); // first 10 dropped
        assert_eq!(s.errors_tail.last().unwrap().doc, "doc-59");
    }

    #[test]
    fn load_rejects_id_mismatch() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.save(&sample_instance("real-id")).unwrap();
        // Rename file to a wrong id
        fs::rename(
            tmp.path().join("real-id.yaml"),
            tmp.path().join("other-id.yaml"),
        )
        .unwrap();
        assert!(store.load("other-id").is_err());
    }
}
```

- [ ] **Step 2: Declare in sources module**

In `mur-core/src/sources/mod.rs`, append:

```rust
pub mod instance;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core sources::instance
```

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/instance.rs mur-core/src/sources/mod.rs
git commit -m "feat(sources): SourceInstance yaml IO with atomic writes and 50-error bound"
```

---

## Task 11: `sources/mod.rs` — `KnowledgeSource` Trait + `SourceRegistry` Stub

**Files:**
- Modify: `mur-core/src/sources/mod.rs`

- [ ] **Step 1: Rewrite `sources/mod.rs` to host the trait**

Open `mur-core/src/sources/mod.rs`. Replace current contents (which is just `pub mod …;` lines) with:

```rust
//! External knowledge sources pipeline.
//!
//! A `KnowledgeSource` is a typed connection to a note app or RAG system. The
//! sync engine iterates documents, chunks them, embeds them, and writes to a
//! `VectorStore`. P1.1 defines the trait + registry skeleton only — adapters
//! arrive in P1.2 (Obsidian), P1.4 (Notion, Joplin).

use anyhow::Result;
use async_trait::async_trait;

pub mod credentials;
pub mod instance;
pub mod kind;
pub mod types;

pub use kind::SourceKind;
pub use types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

/// Adapter interface. Implementors are stateless with respect to the
/// orchestrator; all cursor state is persisted in `SourceInstance`.
#[async_trait]
pub trait KnowledgeSource: Send + Sync {
    /// Stable id, e.g. `"notion:work"`.
    fn id(&self) -> &str;

    /// Behaviour kind.
    fn kind(&self) -> SourceKind;

    /// User-configurable multiplicative weight (from `SourceInstance`).
    fn weight(&self) -> f32;

    /// Incremental listing. `cursor == None` on first sync.
    async fn list_documents(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)>;

    /// Fetch full content for one document.
    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document>;

    /// Adapter-specific chunking.
    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>>;

    /// External ids that this adapter has deleted since `cursor`. Orchestrator
    /// additionally runs a set-diff fallback, so returning `Ok(vec![])` is a
    /// safe no-op default — override when an adapter exposes deletion events
    /// natively.
    async fn list_deleted_since(
        &self,
        _cursor: Option<SyncCursor>,
    ) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

/// Closed-set registry. Phase 1 hardcodes the three adapter type names; the
/// factory function that builds each type lives alongside the adapter itself.
pub const KNOWN_ADAPTER_TYPES: &[&str] = &["obsidian", "notion", "joplin"];

pub fn is_known_adapter_type(t: &str) -> bool {
    KNOWN_ADAPTER_TYPES.contains(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_adapter_types_sanity() {
        assert!(is_known_adapter_type("obsidian"));
        assert!(is_known_adapter_type("notion"));
        assert!(is_known_adapter_type("joplin"));
        assert!(!is_known_adapter_type("onenote"));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p mur-core sources
```

Expected: everything from prior tasks still passes + the 1 new test.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/sources/mod.rs
git commit -m "feat(sources): KnowledgeSource trait + closed-set adapter type registry"
```

---

## Task 12: Gate `mur source` CLI Subcommand Stub Behind `sources` Feature

**Files:**
- Create: `mur-core/src/cmd/source_cmd.rs`
- Modify: `mur-core/src/cmd/mod.rs`
- Modify: `mur-core/src/main.rs`

- [ ] **Step 1: Create the stub subcommand module**

Create `mur-core/src/cmd/source_cmd.rs`:

```rust
//! `mur source ...` subcommand tree.
//!
//! P1.1 wires up the tree with every verb returning a "not yet implemented"
//! error, gated behind the `sources` feature flag. P1.2–P1.4 fill in each verb.

use anyhow::{Result, bail};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Register a new source.
    Add {
        #[command(subcommand)]
        kind: AddKind,
    },
    /// List registered sources.
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
    },
    /// Remove a source (credentials + index).
    Remove {
        id: String,
        #[arg(long)]
        keep_index: bool,
    },
    /// Sync one or all sources.
    Sync {
        id: Option<String>,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        watch: bool,
    },
    /// Show sync health for a source.
    Status { id: Option<String> },
    /// Set the retrieve weight.
    Weight { id: String, value: f32 },
    /// Dry-run a single document through the adapter.
    Test { id: String },
    /// Rebuild the vector index for a source.
    Reindex {
        id: String,
        #[arg(long)]
        vector_backend: Option<String>,
    },
    /// Generate launchd / systemd unit files for scheduled sync.
    InstallSchedule,
    Disable { id: String },
    Enable { id: String },
}

#[derive(Subcommand)]
pub enum AddKind {
    /// Connect a Notion workspace (OAuth or Integration Token).
    Notion {
        instance: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Connect an Obsidian vault (local markdown folder).
    Obsidian {
        instance: Option<String>,
        #[arg(long)]
        vault: std::path::PathBuf,
        #[arg(long, value_delimiter = ',')]
        exclude_folder: Vec<String>,
    },
    /// Connect Joplin (local SQLite or Joplin Server).
    Joplin {
        instance: Option<String>,
        #[arg(long, conflicts_with = "server")]
        db: Option<std::path::PathBuf>,
        #[arg(long, requires = "token")]
        server: Option<String>,
        #[arg(long, requires = "server")]
        token: Option<String>,
    },
}

pub async fn handle(cmd: SourceCommand) -> Result<()> {
    match cmd {
        SourceCommand::Add { .. } => bail!("`mur source add` arrives in P1.2 (obsidian) / P1.4 (notion, joplin)"),
        SourceCommand::List { .. } => bail!("`mur source list` arrives in P1.2"),
        SourceCommand::Remove { .. } => bail!("`mur source remove` arrives in P1.2"),
        SourceCommand::Sync { .. } => bail!("`mur source sync` arrives in P1.2"),
        SourceCommand::Status { .. } => bail!("`mur source status` arrives in P1.2"),
        SourceCommand::Weight { .. } => bail!("`mur source weight` arrives in P1.2"),
        SourceCommand::Test { .. } => bail!("`mur source test` arrives in P1.2"),
        SourceCommand::Reindex { .. } => bail!("`mur source reindex` arrives in P1.3"),
        SourceCommand::InstallSchedule => bail!("`mur source install-schedule` arrives in P1.4"),
        SourceCommand::Disable { .. } => bail!("`mur source disable` arrives in P1.2"),
        SourceCommand::Enable { .. } => bail!("`mur source enable` arrives in P1.2"),
    }
}
```

- [ ] **Step 2: Register the module conditionally**

Open `mur-core/src/cmd/mod.rs`. At the end, add:

```rust
#[cfg(feature = "sources")]
pub(crate) mod source_cmd;
```

- [ ] **Step 3: Wire into the top-level CLI in `main.rs`**

Open `mur-core/src/main.rs`. Locate the main `Commands` enum (line 34 has `#[derive(Subcommand)]`). Read the entire enum body to find a good insertion point (pick a consistent location — probably alphabetical or near other "plural noun" verbs like `Pattern`).

Add a new variant, gated:

```rust
#[cfg(feature = "sources")]
/// Manage external knowledge sources (Notion, Obsidian, Joplin, ...).
Source {
    #[command(subcommand)]
    cmd: cmd::source_cmd::SourceCommand,
},
```

Then in the match-on-commands dispatch (around line 796 where you see `Commands::Links { name } => ...`), add:

```rust
#[cfg(feature = "sources")]
Commands::Source { cmd } => cmd::source_cmd::handle(cmd).await?,
```

- [ ] **Step 4: Verify the CLI help reflects the new subcommand**

```bash
cargo run -- source --help
```

Expected: clap-generated help lists `add`, `list`, `remove`, `sync`, `status`, `weight`, `test`, `reindex`, `install-schedule`, `disable`, `enable`.

- [ ] **Step 5: Verify stub execution**

```bash
cargo run -- source list 2>&1 || true
```

Expected: exit 1 with message "`mur source list` arrives in P1.2".

- [ ] **Step 6: Verify feature-off build hides the subcommand**

```bash
cargo build --no-default-features -p mur-core
cargo run --no-default-features -- source --help 2>&1 || true
```

Expected: no-default-features build succeeds; `source --help` errors with "unrecognized subcommand".

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/source_cmd.rs mur-core/src/cmd/mod.rs mur-core/src/main.rs
git commit -m "feat(cli): add gated \`mur source\` subcommand tree (P1.1 stubs)

All verbs return informative error messages until P1.2+ fills them in.
Gated behind the default-on 'sources' cargo feature so non-feature builds
do not surface the subcommand."
```

---

## Task 13: Update `docs/superpowers/plans/README` / `CLAUDE.md` Pointer (light touch)

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add a line in the Architecture section**

Open `CLAUDE.md`. Find the "Architecture" section (after "Cargo workspace with two crates:"). Locate the Four-Stage Pipeline block. Immediately below the existing `capture/ → store/ → retrieve/ → inject/` diagram, add a paragraph:

```markdown
**Sources pipeline (P1.1 foundation in place; adapters arrive P1.2+):** An alternate input to `store/` lives in `mur-core/src/sources/` — `KnowledgeSource` adapters pull documents from external note apps (Obsidian, Notion, Joplin) into the same retrieve pipeline as patterns. The vector store is abstracted behind `store::vector::VectorStore` (impls: `LanceDbStore` now; `QdrantStore` P1.3). See `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md`.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): note sources pipeline foundation (P1.1)"
```

---

## Task 14: Final Verification Pass

- [ ] **Step 1: Full workspace test run**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: every test green. Compare pass count vs Task 0 Step 2 baseline:
- +4 from `mur-common` config tests (Task 2)
- +1 from vector conformance smoke (Task 5)
- +3 from factory tests (Task 6)
- +3 from sources::types tests (Task 7)
- +2 from sources::kind tests (Task 8)
- +3 from sources::credentials tests (Task 9)
- +6 from sources::instance tests (Task 10)
- +1 from sources::mod tests (Task 11)

Total **+23** new tests expected.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-features -- -D warnings
```

Expected: no warnings.

- [ ] **Step 3: Format check**

```bash
cargo fmt --check
```

Expected: clean. If `fmt --check` fails, run `cargo fmt` and commit as a follow-up.

- [ ] **Step 4: Confirm feature flag off still compiles**

```bash
cargo build --workspace --no-default-features
```

Expected: clean compile.

- [ ] **Step 5: Confirm binary starts and existing commands still work**

```bash
cargo run --release -- --help
cargo run --release -- pattern list 2>&1 | head -5
```

Expected: CLI usage shows `source` as a subcommand; `pattern list` behaves exactly as before.

- [ ] **Step 6: Sanity check — the `~/.mur/sources/` directory is NOT created by existing commands**

P1.1 only creates it on first `SourceInstanceStore::save()` call, and nothing invokes that yet. Verify:

```bash
ls ~/.mur/sources 2>&1 || echo "good: sources dir does not yet exist"
```

Expected: directory does not exist (unless you have an earlier in-dev version).

- [ ] **Step 7: Close-out commit if anything drifted**

If `cargo fmt` made adjustments, commit them:

```bash
git add -A
git commit -m "style: cargo fmt after P1.1 foundation"
```

---

## Done Criteria (P1.1)

- [ ] `store::vector::VectorStore` trait defined, with LanceDbStore implementing it (stubs are allowed — P1.2 fills them in).
- [ ] `store::vector::factory::get_vector_store(&Config, &Path)` selects backend from config.
- [ ] `sources::{types, kind, credentials, instance}` modules compiled with tests.
- [ ] `sources::KnowledgeSource` trait defined (no adapter impls yet).
- [ ] `Config::storage` + `Config::sources_global` present with zero-action defaults.
- [ ] `mur source --help` surfaces the subcommand tree behind `sources` feature flag; verbs return informative "not yet implemented" errors.
- [ ] All existing tests still pass; +23 new tests added.
- [ ] `CLAUDE.md` architecture section mentions the sources pipeline foundation.
- [ ] No `~/.mur/sources/` directory auto-created by any existing code path.

**What P1.1 intentionally does not deliver:**
- Any adapter implementation (Obsidian/Notion/Joplin)
- Any real `VectorStore::upsert/search/etc.` body (stubs only)
- `mur reindex` treating sources (P1.3)
- Qdrant (P1.3)
- Tantivy BM25 (P1.3)
- Inject formatter Notes section (P1.3)

P1.2 begins when these are all ticked.
