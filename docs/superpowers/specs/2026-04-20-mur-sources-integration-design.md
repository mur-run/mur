# mur Sources Integration — Design Spec

- **Status**: Draft — awaiting user approval before implementation plan
- **Author**: Claude (brainstorming session)
- **Date**: 2026-04-20
- **Scope**: Phase 1 of external knowledge source integration for mur
- **Related**: `docs/mur-函數清單與競品分析.md` (§ Karpathy LLM Wiki, line 236)

---

## 1. Problem & Goal

mur today learns patterns from AI coding sessions. Users also keep substantial knowledge in dedicated note apps (Notion, Obsidian, Joplin, OneNote, Apple Notes, Notability, NotebookLM). Today there is no bridge: the knowledge in those apps is invisible to AI tools that mur injects into.

**Goal**: Let users connect note apps to mur so `mur search` and AI-session injection can surface content across **patterns + external notes** from a single unified index — while keeping the source apps as the authoritative stores.

**Non-goal**: mur does not become a note app, does not replace Notion/Obsidian, and does not maintain bidirectional sync. External notes remain owned by their source apps.

## 2. Decisions (from brainstorming Q&A)

| # | Decision | Rationale |
|---|----------|-----------|
| Q1 | Primary use-case = **unified search** across pattern + external corpus | User explicit choice (B) |
| Q2 | Indexing = **local pull-sync** into mur's vector store | Matches mur's offline-first DNA; consistent ranking |
| Q3 | Retrieval corpus = **unified** (patterns + notes in one ranker), with **per-source weight + scope** | Answer "B + C" compromise; respects source boundaries without fragmenting corpus |
| Q4 | Phase 1 adapters = **Obsidian + Notion + Joplin** | Covers one local-file + one OAuth cloud + one local-DB; proves both connection styles |
| Q5 | MCP / federated query = **trait signature pre-designed, implementation deferred** | `SourceKind::FederatedQuery` variant exists but Phase 1 has zero impls |
| Q6 | Vector stores = `VectorStore` **trait** with **LanceDB (default) + Qdrant** impls | User wants pluggability; trait validates with ≥2 impls |
| Q7 | Sources **do not** feed `evolve` pattern extraction — strict separation | Keeps pattern quality high; avoids Mem0-style junk; notes are live source-of-truth elsewhere |
| CLI | Command verb = **`mur source`** (not `mur link`, not `mur notes`) | Abstract noun matches `KnowledgeSource` trait; future-proof for non-note sources (Slack/Linear/MCP) |
| Store | LanceDB and Qdrant are **alternative** backends (one active), switch via `mur reindex --vector-backend <B>` | YAGNI; per-table backend future extension noted but not built |

## 3. Architecture

### 3.1 Pipeline

mur's existing four-stage pipeline gains a second input alongside `capture/`:

```
┌──────────┐    ┌──────────┐
│ capture  │    │ sources  │  ← new stage
│ (session │    │ (external│
│  -> pat) │    │  notes)  │
└────┬─────┘    └────┬─────┘
     │               │
     └───────┬───────┘
             ↓
      ┌──────────────┐
      │   store/     │   patterns yaml  ∥  sources LanceDB/Qdrant
      └──────┬───────┘
             ↓
      ┌──────────────┐
      │  retrieve/   │   unified corpus query, source-weighted
      └──────┬───────┘
             ↓
      ┌──────────────┐
      │   inject/    │   formatter: Patterns section + Notes section
      └──────────────┘
```

### 3.2 New Rust Modules

```
mur-core/src/
├── sources/                    # new
│   ├── mod.rs                  # KnowledgeSource trait + SourceRegistry
│   ├── kind.rs                 # SourceKind enum: PullIndex | FederatedQuery (stub)
│   ├── credentials.rs          # OS keyring wrapper
│   ├── sync.rs                 # Sync orchestrator (manual/watch/scheduled)
│   ├── chunker/
│   │   ├── markdown.rs         # shared Obsidian + Joplin
│   │   └── notion_blocks.rs    # Notion-specific
│   └── adapters/
│       ├── obsidian.rs
│       ├── notion.rs
│       └── joplin.rs
└── store/
    └── vector/                 # new abstraction layer
        ├── mod.rs              # VectorStore trait + SearchFilter
        ├── lancedb.rs          # renamed from store/lancedb.rs
        └── qdrant.rs           # new
```

Existing `store/lancedb.rs` (472 lines) becomes `store/vector/lancedb.rs` and its `VectorStore` struct is renamed `LanceDbStore` to free the name for the trait.

### 3.3 Core Traits

```rust
#[async_trait]
pub trait KnowledgeSource: Send + Sync {
    fn id(&self) -> &str;                                 // e.g. "notion:work"
    fn kind(&self) -> SourceKind;                         // PullIndex | FederatedQuery
    fn weight(&self) -> f32;

    async fn list_documents(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)>;

    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document>;
    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>>;

    async fn list_deleted_since(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<Vec<String>>;
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()>;
    async fn search(&self, query_vec: &[f32], k: usize, filter: &SearchFilter) -> Result<Vec<Hit>>;
    async fn delete_by_external_ids(&self, source_id: &str, ids: &[String]) -> Result<()>;
    async fn delete_by_source(&self, source_id: &str) -> Result<()>;
    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>>;
    async fn count(&self, source_id: Option<&str>) -> Result<usize>;
    async fn rebuild_index(&self) -> Result<()>;
}
```

`SourceKind::FederatedQuery` is a stub for Phase 2 (MCP / NotebookLM). Phase 1 only implements `PullIndex`.

## 4. CLI Surface

All new commands live under `mur source <verb>`. No `mur link` alias.

```
mur source
├── add <TYPE> [INSTANCE] [flags...]
├── list [--json] [--verbose]
├── remove <ID> [--keep-index]
├── sync [<ID>] [--full] [--watch]
├── status [<ID>]
├── weight <ID> <float>
├── scope <ID> [adapter-specific flags]
├── test <ID>
├── reindex <ID> [--vector-backend <B>]
├── install-schedule         # generate launchd/systemd unit
└── disable / enable <ID>
```

### 4.1 Adapter Types as Subcommands

Rather than a `--type` flag, the adapter type is itself a subcommand of `add`. This enables adapter-specific flags with clap's native validation.

```
mur source add notion [INSTANCE] [--workspace <id>] [--token <pat>]
mur source add obsidian [INSTANCE] --vault <path> [--exclude-folder <folder>]
mur source add joplin [INSTANCE] --db <path>
                            |  --server <url> --token <token>
```

`INSTANCE` is an optional positional name. Without it, mur auto-generates an ID:

- `mur source add notion` → id = `notion` (first) or `notion:<rand4>` (second+)
- `mur source add notion work` → id = `notion:work`

### 4.2 Existing Commands, Extended

- **`mur search`** now queries unified corpus. New flags: `--source <id>`, `--type patterns|sources|all` (default `all`), `--only-patterns`, `--only-sources`.
- **`mur reindex`** now rebuilds both `patterns.lance` and `sources.lance` (or Qdrant collections). Add `--vector-backend <lancedb|qdrant>` to migrate.

### 4.3 Output & Exit Codes

- Table by default (via `comfy-table`), `--json` for machine output, `--quiet` suppresses non-error output.
- Exit codes: `0` ok, `1` generic, `2` auth, `3` network, `4` config.
- Long ops use `indicatif` progress bars (existing dep).

### 4.4 Adapter Registry = Closed Set (Phase 1)

`notion | obsidian | joplin` hardcoded as a clap enum in `main.rs`. `SourceRegistry` uses a `match` — no dyn trait factory, no plugin loading. Phase 2 may revisit for MCP / WASM plugins.

## 5. Data Model

### 5.1 Disk Layout

```
~/.mur/
├── config.yaml                 # extended: storage, sources_global
├── patterns/*.yaml             # unchanged
├── sources/                    # new
│   ├── notion:work.yaml
│   ├── obsidian:main.yaml
│   └── joplin:main.yaml
├── lancedb/                    # existing when backend=lancedb
│   ├── patterns.lance
│   └── sources.lance           # new table
└── tantivy/                    # new — unified BM25 index
    └── sources/
```

Credentials never hit disk. They live in OS keyring (`keyring-rs` crate: macOS Keychain / Linux Secret Service / Windows Credential Manager).

### 5.2 Source Instance YAML

```yaml
# ~/.mur/sources/notion:work.yaml
id: notion:work
type: notion
enabled: true
weight: 1.0
scope:
  workspace: Engineering
  include_page_ids: []
  exclude_tags: [personal]
sync:
  last_cursor: "abc123"        # adapter-defined opaque string
  last_sync_at: 2026-04-20T10:30:00Z
  last_error: null
  errors_tail:                 # bounded to last 50
    - { at: 2026-04-19T09:12Z, doc: "page-abc", msg: "HTTP 502" }
stats:
  doc_count: 312
  chunk_count: 1248
  indexed_bytes: 892341
keyring_entry: "mur:notion:work"
```

Each yaml stays ~1–3 KB regardless of how many documents the source has. Actual document bytes live in the vector store.

### 5.3 Config Schema Extensions

```rust
// mur-common/src/config.rs
pub struct Config {
    // ...existing...
    #[serde(default)]
    pub storage: StorageConfig,             // new
    #[serde(default)]
    pub sources_global: SourcesGlobalConfig,// new
}

pub struct StorageConfig {
    pub vector_backend: String,             // "lancedb" (default) | "qdrant"
    pub qdrant_url: Option<String>,         // e.g. http://localhost:6333
    pub qdrant_api_key_ref: Option<String>, // keyring account name
}

pub struct SourcesGlobalConfig {
    pub poll_interval_secs: u64,            // default 600
    pub max_chunks_per_sync: usize,         // default 10_000
    pub max_parallel_sources: usize,        // default 3
    pub default_weight: f32,                // default 1.0
    pub embedding_batch_size: usize,        // default 32
}
```

Both structs implement `Default`. Existing users get working defaults on first read — zero-action upgrade.

### 5.4 Core Types (`mur-common/src/sources.rs`)

```rust
pub struct Document {
    pub source_id: String,
    pub external_id: String,       // adapter-native
    pub title: String,
    pub body: DocumentBody,
    pub url: Option<String>,       // deep-link back to source app
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub metadata: serde_json::Value,
}

pub enum DocumentBody {
    Markdown(String),
    PlainText(String),
    NotionBlocks(Vec<NotionBlock>),
}

pub struct Chunk {
    pub chunk_id: String,          // uuid v4
    pub source_id: String,
    pub external_id: String,
    pub ordinal: usize,
    pub text: String,              // plaintext for embedding
    pub heading_path: Vec<String>,
    pub char_range: (usize, usize),
    pub updated_at: DateTime<Utc>,
}

pub struct EmbeddedChunk {
    pub chunk: Chunk,
    pub embedding: Vec<f32>,
}

pub struct Hit {
    pub chunk_id: String,
    pub source_id: String,
    pub score: f32,                // post-weight
    pub text: String,
    pub title: String,
    pub url: Option<String>,
    pub heading_path: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

pub struct SearchFilter {
    pub source_ids: Option<Vec<String>>,   // None = all
    pub types: SourceTypeSet,              // Patterns | Sources | Both
    pub since: Option<DateTime<Utc>>,
}
```

Uniqueness key across all storage is `(source_id, external_id)`. Adapters are not required to produce globally unique IDs.

## 6. Sync Engine

### 6.1 Triggers

| Mode | Command | Description |
|------|---------|-------------|
| Manual | `mur source sync [<id>]` | One-shot; progress bar |
| Watch | `mur source sync --watch` | Foreground daemon; fsevents + polling; Ctrl+C for clean shutdown |
| Scheduled | `mur source install-schedule` | Generates `launchd` plist (macOS) or `systemd --user` unit (Linux). mur itself does not fork a long-lived daemon — OS handles scheduling |

### 6.2 Sync Flow (Single Source)

```
1. Load ~/.mur/sources/<id>.yaml                     → cursor, scope, weight
2. Fetch credentials from keyring
3. adapter.list_documents(cursor)                    → (doc_refs, new_cursor)
4. Batch-process (size = embedding_batch_size):
   a. adapter.fetch(doc_ref)                         → Document
   b. adapter.chunk(doc)                             → Vec<Chunk>
   c. embedding.embed_batch(chunks)                  → Vec<EmbeddedChunk>
   d. vector_store.upsert(chunks)
   e. tantivy.index(chunks)                          (for BM25)
   [cursor advances ONLY on batch success]
5. Detect deletions via set diff:
   indexed = vector_store.list_external_ids(source_id)
   current = set(external_id for doc in adapter.list_documents(None))
   deleted = indexed - current
   vector_store.delete_by_external_ids(source_id, deleted)
   tantivy.delete(deleted)
6. Atomic write updated yaml (temp file + rename)
```

### 6.3 Error Handling

- **Resumable**: cursor advances per batch; interrupted sync picks up exactly where it left off.
- **Per-doc isolation**: one failing doc logs an error into `sync.errors_tail[]` (bounded to 50 entries) but does not abort the sync.
- **Exponential backoff**: 1s / 2s / 4s / 8s, max 4 retries per doc. HTTP 429 / 503 honor `Retry-After` header.
- **Rate limiting**: `governor` crate token bucket per-adapter (Notion: 3 req/sec default).

### 6.4 Concurrency

- Within a source: sequential batches (avoid embedding API quota blowouts).
- Across sources: `tokio::try_join_all`, bounded at `sources_global.max_parallel_sources` (default 3).

### 6.5 Deletion Detection

No sidecar file. Compute `indexed_ids - current_adapter_ids` each sync by querying the vector store. Requires `VectorStore::list_external_ids` (added to trait).

Per-adapter behavior:
- Obsidian: fsevents drives real-time deletes in `--watch`; sync mode uses set diff.
- Notion: pages with `archived: true` are excluded from `list_documents`; set diff removes them.
- Joplin (local DB): `WHERE deleted_time IS NULL` filter; set diff for anything removed.
- Joplin Server: `/api/items/deleted` endpoint if available, else set diff.

### 6.6 Credentials (OS Keyring)

Keyring schema:

```
Service:  "mur"
Account:  "<source_id>:<field>"
Examples:
  "notion:work:access_token"
  "notion:personal:access_token"
  "joplin:main:api_token"
```

Obsidian has no credentials (path-based). Keyring library: `keyring-rs`.

### 6.7 OAuth Flow (Notion)

```
1. User runs `mur source add notion [instance]`
2. mur spins up an axum server on a random localhost port (reuses server.rs)
3. Browser opens to Notion OAuth URL (client_id built into binary, PKCE code_challenge)
4. User authorizes workspace in Notion
5. Notion redirects to http://localhost:<port>/callback?code=...
6. mur exchanges code for access_token + workspace info
7. Keyring gets access_token; yaml stores workspace_id / name
8. Localhost server shuts down
9. Shell prints: "Connected to workspace <name>. Run `mur source sync <id>` to index."
```

Notion access tokens do not expire — no refresh logic. Future OneNote / Google integrations will need refresh; pattern left room for it (no contract change).

mur registers a **public Notion integration** — `client_id` embedded in binary, no `client_secret` (PKCE flow).

**Fallback**: if Notion's public-integration review is pending, users can provide their own Internal Integration Token (PAT) via `mur source add notion --token <pat>`. This skips OAuth entirely, stores the token in keyring, and is treated the same by all downstream code. We ship with this path enabled as an escape hatch. See §13 risks.

## 7. Adapter Specs

### 7.1 Obsidian

| Item | Design |
|------|--------|
| Connection | `mur source add obsidian [INSTANCE] --vault <path>` |
| Auth | None |
| Validation | `<vault>/.obsidian/` must exist |
| external_id | Relative path string (`notes/ideas/foo.md`). Rename ⇒ delete+create from mur's view; acceptable Phase 1 trade-off. |
| URL | `obsidian://open?vault=<encoded_name>&file=<encoded_path>` |
| Discovery | Recursive `*.md` scan, exclude `.obsidian/`, `.trash/`, user `--exclude-folder` |
| Chunking | Markdown heading-aware (shared chunker); 1500-token max; paragraph-boundary fallback |
| Metadata | YAML frontmatter → `Document.tags` + `Document.metadata` |
| Wikilinks | `[[Other Note]]` and `![[embed.md]]` preserved as plaintext in chunk text |
| Tags | Inline `#tag` + frontmatter `tags:` merged, deduped |
| File watcher | `notify::RecommendedWatcher`; debounce 500ms; whitelist `*.md` (ignores `.obsidian/workspace.json` churn) |
| updated_at | File `mtime` |
| Deletion | Watch: `Remove` events. Sync: set diff. |

### 7.2 Notion

| Item | Design |
|------|--------|
| Connection | `mur source add notion [INSTANCE] [--workspace <id>] [--token <pat>]` |
| Auth | OAuth 2.0 + PKCE, localhost callback (§6.7) |
| external_id | Notion page UUID |
| URL | `https://www.notion.so/<workspace_slug>/<page_id>` |
| Discovery | `POST /v1/search` with `filter.value="page"`, 100/page |
| Incremental | `last_edited_time >= <cursor>` filter; cursor = RFC3339 timestamp |
| Chunking | Block-aware (`notion_blocks.rs`): `heading_1/2/3` boundaries; `code` → markdown fence; `table` → markdown table; `toggle` → expanded content; `callout`/`quote` prefixed |
| Content | chunk.text = simplified markdown (for embedding); `Document.body = NotionBlocks` (for future fidelity rendering) |
| Rate limit | `governor` token bucket at 3 req/sec; honor `429 + Retry-After` |
| Pagination | 100/call, `has_more + next_cursor` |
| Deletion | `archived: true` pages excluded from `list_documents`; set diff catches |

Scope: `mur source scope notion:work --workspace Engineering --include-page-ids a,b,c --exclude-tag personal`.

**Known limitation (Phase 1)**: database page properties are not indexed — only body blocks.

### 7.3 Joplin

Two modes supported; Web Clipper API excluded (requires Joplin app running — too fragile).

#### 7.3.1 Local SQLite

| Item | Design |
|------|--------|
| Connection | `mur source add joplin [INSTANCE] --db <path>` |
| Default path detection | macOS: `~/Library/Application Support/joplin-desktop/database.sqlite`; Linux: `~/.config/joplin-desktop/database.sqlite` |
| Auth | File read permission only. `rusqlite` opens read-only with `immutable=true` URI flag to avoid lock conflicts with running Joplin. |
| external_id | Joplin note UUID (TEXT primary key) |
| URL | `joplin://x-callback-url/openNote?id=<id>` |
| Discovery | `SELECT id,title,body,updated_time,parent_id FROM notes WHERE is_conflict=0 AND deleted_time IS NULL` |
| Incremental | `updated_time > <cursor_epoch_ms>` |
| Chunking | Body is markdown → shared chunker |
| Folders | Join `folders` table; `Document.metadata.notebook_path = ["Tech","Architecture"]` |
| Tags | Join `note_tags` + `tags` tables |

#### 7.3.2 Joplin Server

| Item | Design |
|------|--------|
| Connection | `mur source add joplin [INSTANCE] --server <url> --token <token>` |
| Auth | Bearer token in keyring |
| API | `/api/items?type=note` paginated; `/api/items/<id>` for body |
| Remaining | Same chunking, tags, notebooks as Local DB |

Scope: `mur source scope joplin:main --notebook "Tech" --exclude-tag draft`.

### 7.4 Shared Dependencies (Cargo.toml)

```toml
notify = "6"              # file watcher
rusqlite = { version = "0.32", features = ["bundled"] }
oauth2 = "4"              # PKCE
keyring = "3"             # OS credential store
governor = "0.7"          # rate limiting
pulldown-cmark = "0.12"   # markdown parsing
tantivy = "0.22"          # BM25 index
qdrant-client = "1.12"    # Qdrant REST+gRPC client
```

Existing deps (`tokio`, `reqwest`, `serde`, `serde_yaml`, `axum`, `tracing`, `anyhow`, `dialoguer`, `indicatif`) are reused.

## 8. Retrieval Integration

### 8.1 Unified Flow

```
query (from mur search OR inject trigger)
  ↓
embed(query) → query_vec
  ↓
┌────────────────────────┬──────────────────────────┐
│ score_patterns()       │ score_sources()          │
│ [existing formula]     │ [new, simpler formula]   │
└───────────┬────────────┴────────────┬─────────────┘
            ↓                         ↓
         top k*2                    top k*2
            └────────────┬────────────┘
                         ↓
                 unified re-rank
                 (apply source_weight)
                         ↓
                 floor 0.35 filter
                         ↓
                    top k (default 5)
                         ↓
          ┌──────────────┴─────────────┐
          ↓                            ↓
       mur search                 inject formatter
```

### 8.2 Scoring Formulas

**Patterns (existing, unchanged)**:
```
score = 0.7*vec_sim + 0.3*bm25
      × recency × effectiveness × importance × time_decay × length_norm
```

**Sources (new, simpler)**:
```
score = 0.7*vec_sim + 0.3*bm25
      × source_weight × freshness_factor × length_norm
```

Excluded from source formula: `effectiveness` (no usage data), `importance` (no lifecycle), `time_decay` (replaced by `freshness_factor`).

**`freshness_factor`** = `exp(-age_days / 365)` — gentle annual half-life. Patterns use tier-specific half-lives (14d/90d/365d); source freshness is driven by source app's own `updated_at`, so we don't punish.

**`source_weight`** = value in `<source_id>.yaml:weight`, default 1.0. Multiplicative; no hard filter. To exclude a source entirely: `mur source disable <id>` (sets `enabled: false`).

### 8.3 BM25 Backend Consistency

**Decision**: One unified `tantivy` index, regardless of vector backend (LanceDB or Qdrant).

- LanceDB has an FTS; Qdrant does not.
- Piggy-backing each backend's native FTS would cause ranking to change on backend swap — breaks the "free choice" promise.
- tantivy is Rust-native, pure-embedded, ~5–10% disk overhead of chunk corpus.
- Index location: `~/.mur/tantivy/sources/`.
- Recovery: if tantivy index is corrupt, rebuild from vector store (text stored in both) via `mur reindex`.

### 8.4 Inject Formatter

AI-session injection separates pattern and source context:

```
# From your learning history

## Patterns (3)

[Pattern: async-error-handling] (maturity: Stable, confidence: 0.87)
  When async operations chain, use Result<T,E> + ? rather than...

[Pattern: react-useReducer-when]
  Prefer useReducer over useState when state transitions form...

## Notes (2)

[Note: Obsidian / Projects / Auth Design.md § "JWT refresh rotation"]
  URL: obsidian://open?vault=main&file=Projects%2FAuth%20Design.md
  We decided on 15-min access tokens + 7-day refresh because...

[Note: Notion / Engineering / Q2 Roadmap]
  URL: https://www.notion.so/...
  Quarter theme: API versioning + observability...
```

Rationale:
1. AI model understands provenance (rule-I-learned vs reference-I-wrote).
2. URLs let the user click back to source apps for context.
3. Per-type token caps prevent one category from dominating.

### 8.5 Token Budgets

- Total budget: 2500 tokens (was 2000).
- Split: max 5 patterns + max 3 notes.
- Per-chunk truncation: source chunks over 400 tokens get truncated at paragraph boundary, suffixed with `...` and URL.

### 8.6 `mur search` Output

```
$ mur search "OAuth refresh flow"

Score   Type     ID                                   Title / Heading
─────   ──────   ──────────────────────────────────   ─────────────────────────
0.89    note     obsidian:main / Auth Design.md § ... "JWT refresh rotation"
0.82    pattern  oauth-pkce-localhost                 PKCE for local-app OAuth
0.76    note     notion:work / Q1-incident-tokens     "token revoke fallout"
0.71    pattern  refresh-token-rotation               Rotation strategy
0.64    note     joplin:main / Tech / auth-notes      ...

Flags: --source <id> | --type patterns|sources|all | --json | --only-patterns | --only-sources
```

### 8.7 Latency Budget

Target p95 < 200 ms end-to-end:

- `embed(query)`: ~50ms cold, ~0ms warm (embedding cache)
- tantivy BM25: ~5ms
- Vector search × 2 (patterns + sources, parallel): ~30ms
- Merge + rank: < 1ms
- **Total**: ~100ms median, ~180ms p95 — comfortably within hook budget.

### 8.8 Retrieve Module Changes

```
mur-core/src/retrieve/
├── mod.rs          # new retrieve_unified() entry
├── scoring.rs      # existing pattern scoring untouched; new score_sources()
└── gate.rs         # unchanged
```

### 8.9 Phase 1 Exclusions (Retrieve)

- Cross-encoder reranking (e.g. Cohere Rerank): +300ms, not worth it for Phase 1.
- Query expansion / HyDE.
- Personalization / usage-based weight learning.
- Snippet highlighting in terminal output.

## 9. Testing Strategy

### 9.1 Unit Tests (per adapter)

- Obsidian: chunker correctness, frontmatter parsing, wikilink preservation, watcher debounce.
- Notion: block→markdown conversion, OAuth URL construction, pagination cursor handling, rate-limit backoff.
- Joplin: SQL queries, tag/notebook joins, server API pagination.
- Target: ≥80% line coverage per adapter module.

### 9.2 Trait Conformance Tests

**`VectorStore` common suite** (`store/vector/tests.rs`) — every impl must pass:

```rust
#[async_trait]
pub trait VectorStoreTestSuite {
    async fn test_upsert_roundtrip();
    async fn test_search_returns_correct_k();
    async fn test_delete_by_source_removes_all();
    async fn test_delete_by_external_ids_preserves_others();
    async fn test_list_external_ids_completeness();
}

// Both LanceDbStore and QdrantStore run this suite.
```

**`KnowledgeSource` + `sync` tests** (`sources/tests.rs`) — mock adapter validates orchestrator:

- `test_sync_resumes_after_crash` — cursor advances correctly post-restart
- `test_deletion_detected_via_set_diff`
- `test_per_doc_error_does_not_abort`
- `test_rate_limit_triggers_backoff`

### 9.3 End-to-End Smoke

- Obsidian: `tempfile` vault → `add` → `sync` → `search` → assert hit.
- Notion: CI secret `NOTION_TEST_TOKEN`, fixed test workspace.
- Joplin: committed fixture SQLite.

### 9.4 Retrieve Determinism Tests

Fixed corpus (5 hardcoded patterns + 10 hardcoded source chunks), known query, **assert top-k order**:

```rust
#[test]
fn retrieve_unified_mixed_corpus_ordering() { /* ... */ }

#[test]
fn source_weight_changes_ranking() { /* set weight=2.0, assert note outranks pattern */ }

#[test]
fn disabled_source_absent_from_results() { /* enabled=false */ }
```

## 10. Migration Path (Existing Users)

Zero action required on upgrade.

| Change | Effect on existing user |
|--------|-------------------------|
| New `config.storage.vector_backend` field | Defaults to `"lancedb"` — matches current behavior |
| New `config.sources_global` fields | All have sensible defaults |
| New `~/.mur/sources/` directory | Created on first `mur source add` |
| New LanceDB `sources.lance` table | Created on first source sync; `patterns.lance` untouched |
| Existing CLI commands | Behavior identical until the first source is added. `mur search` defaults to `--type all`, but with no sources configured the result set equals today's pattern-only output. Opt-in from the user's side via `mur source add`. |

On first run after upgrade: `mur --version` shows new version; `mur source --help` shows new command tree; everything else works identically.

## 11. Rollout — Phase 1 Sub-Milestones

Phase 1 is large. Split into 4 independently-shippable milestones.

### P1.1 — Foundation (Est. 1–2 weeks)

- Refactor existing `VectorStore` struct → rename `LanceDbStore`; extract trait.
- Add `KnowledgeSource` trait skeleton (no adapter impls yet).
- `sources/` module skeleton + yaml IO.
- Config schema extensions (backward compatible).
- `VectorStore` common test suite. All existing pattern behavior still green.
- **No user-facing features** — `mur source` commands gated behind feature flag.

### P1.2 — Obsidian Adapter (Est. 1 week)

- `sources/adapters/obsidian.rs` + shared markdown chunker.
- `mur source add obsidian / sync / list / remove / status / test` working.
- Basic file watcher (not yet full `--watch` mode).
- E2E test with temp vault.
- **Delivers**: `mur source add obsidian --vault ~/Vault`; `mur search` returns Obsidian hits.

### P1.3 — Retrieve Integration + Qdrant (Est. 1–2 weeks)

- Unified retrieve (`retrieve/mod.rs` additions, source scoring).
- tantivy BM25 index.
- Inject formatter with Notes section.
- `QdrantStore` impl (passes common suite).
- `mur reindex --vector-backend <B>`.
- **Delivers**: full retrieve story; user can swap to Qdrant.

### P1.4 — Notion + Joplin + Watch + Schedule (Est. 2 weeks)

- `sources/adapters/notion.rs` (OAuth + block chunker + rate limit).
- `sources/adapters/joplin.rs` (Local DB + Server modes).
- Full `--watch` mode (cloud polling + Obsidian fsevents).
- `mur source install-schedule` (launchd / systemd unit generators).
- Docs & release notes.
- **Delivers**: Phase 1 complete.

**Total time**: 5–7 weeks at 1 FTE.

### Descope Priority (if Timeline Compresses)

1. `install-schedule` (drop; doc manual cron instead)
2. Joplin Server mode (drop; keep Local DB only)
3. `--watch` mode (drop; manual sync only)
4. Qdrant backend (drop; LanceDB only)
5. Notion adapter (drop; ship Obsidian + Joplin only — last resort)

Tantivy and trait conformance tests are non-negotiable — they guard quality.

## 12. Documentation Updates (per CLAUDE.md checklist)

- `README.md` — new `mur source` section with usage examples.
- `docs/sources.md` — new — adapter deep-dive + troubleshooting.
- `CLAUDE.md` — update architecture section (add `sources/` pipeline stage).
- `mur-server/dashboard/docs-content/` — sources chapter.
- `mur-server/dashboard/src/components/docs/coreNavigation.tsx` — menu entry.
- `mur-server/dashboard/src/app/products/core/page.tsx` — feature bullet.

## 13. Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Notion public-integration OAuth app approval / quota | Medium | High | Both paths are supported from day 1 (§6.7): public OAuth for the common case, user-provided PAT as manual escape hatch. If approval stalls or rate limits bite, users can continue via PAT. |
| Tantivy learning curve | Medium | Medium | Ship minimal BM25 first (title + text); rely on `tantivy` crate examples; prepare fallback to LanceDB FTS if needed. |
| macOS fsevents storm from `.obsidian/workspace.json` churn | High | Low | Whitelist filter: only `*.md` events reach handler. |
| Qdrant local install UX friction | Medium | Medium | Provide `docker-compose.yml` sample; default remains LanceDB. |
| Large vault set-diff slowness (>50k chunks) | Low | Medium | Measure; if slow, switch to incremental deletion events via watcher for that adapter. |

## 14. Observability (Phase 1 minimal)

- Existing `tracing` infrastructure.
- Structured events in sync loop: `sync.start`, `sync.batch`, `sync.doc_success`, `sync.doc_error`, `sync.deletions`, `sync.complete`.
- `RUST_LOG=mur_core::sources=debug` reveals detail.
- `mur source status <id>` surfaces key metrics for interactive debugging.
- Rolling log at `~/.mur/logs/sources.log` (7-day retention).
- **No network telemetry added** in Phase 1.

## 15. Explicit Non-Goals / Phase 2+

These are intentionally out of scope:

- Bidirectional sync (writing patterns back into Obsidian/Notion).
- Pattern extraction from notes (`mur learn from-source`) — Q7 decision A.
- Additional adapters: OneNote, Apple Notes, Notability, NotebookLM.
- MCP client (`SourceKind::FederatedQuery` adapters).
- Per-table vector backends (patterns → LanceDB, sources → Qdrant split).
- Shared / team source subscriptions (`mur source share / publish`).
- Web UI for source management.
- `mur link` / `mur connect` command aliases.
- Cross-encoder reranking, HyDE, personalization.
- Canvas (`.canvas`) files in Obsidian.
- Notion database property indexing.

Future extensions are accommodated without breaking changes:
- `KnowledgeSource` trait already defines `SourceKind::FederatedQuery` variant for MCP.
- `Pattern.source_ref: Option<SourceRef>` (reserved) for future note→pattern extraction.
- `StorageConfig` split into `patterns_backend` / `sources_backend` is a single-field addition.

---

## Appendix A — Decision Log

| Q | Question | Options | Chosen | Date |
|---|----------|---------|--------|------|
| 1 | Primary use-case for note app integration | Inject / Search / Learn / Bidi | **Search** | 2026-04-20 |
| 2 | Indexing mode | Local / Federated / Hybrid | **Local** | 2026-04-20 |
| 3 | Retrieve corpus boundary | Isolated / Unified / Per-source opt-in | **Unified + per-source scope/weight** | 2026-04-20 |
| 4 | Phase 1 adapters | Permutations | **Obsidian + Notion + Joplin** | 2026-04-20 |
| 5 | MCP / NotebookLM strategy | Now / Later / Trait-stub | **Trait-stub (Phase 2 impl)** | 2026-04-20 |
| 6 | VectorStore backends | Single / Multiple | **LanceDB + Qdrant trait-abstracted** | 2026-04-20 |
| 7 | Source → pattern extraction | Strict / Opt-in / Auto | **Strict (Phase 1)** | 2026-04-20 |
| CLI | Command verb | `link` / `source` / `notes` / `connect` | **`mur source`** | 2026-04-20 |
| Store | Dual vector backend lifecycle | Both-active / Either / Split | **Either (single active)** | 2026-04-20 |
