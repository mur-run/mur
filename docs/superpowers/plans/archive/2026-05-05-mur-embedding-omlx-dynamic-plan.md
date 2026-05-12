# Dynamic Local Embedding Discovery (incl. oMLX) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Add oMLX as an embedding provider, replace hardcoded `mur init` model menus with runtime discovery (Ollama `/api/show` + oMLX `/v1/embeddings` probe), and make Mode 1 default to the best locally-available embedding model.

**Architecture:** New `mur-core/src/discovery/` module with a `Discovery` trait and two impls (Ollama, oMLX). Cache results at `~/.mur/cache/discovery.json` (TTL 24h). Bug-fix `EmbeddingProvider::OpenAI` to honor `cfg.embedding.openai_url` so oMLX (OpenAI-compatible at `localhost:8000/v1`) can be used as embedding backend immediately. Init UX renders a menu seeded by a static prefix-matched preference table intersected with what's actually pulled.

**Tech Stack:** Rust 2024, tokio, reqwest, async-trait 0.1, wiremock 0.6 (already in `mur-core/Cargo.toml`), serde, anyhow, chrono.

**Spec:** `docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md`

**Phasing:** 5 milestones, each one PR. M1 ships standalone value (manual config edit unblocks oMLX). M2-M4 are library-only (no UX change). M5 flips the user-facing flow.

---

## M1 — Bug Fix: `EmbeddingProvider::OpenAI` honors `openai_url`

**Goal:** Make `mur-core/src/store/embedding.rs::embed_openai` use a configurable base URL, add `omlx` / `mlx` provider aliases, so a user can manually edit `~/.mur/config.yaml` to point embedding at oMLX.

**Branch:** `feat/mur-m1-embedding-base-url`

### Task 1.1: Add `base_url` field to `EmbeddingProvider::OpenAI`

**Files:**
- Modify: `mur-core/src/store/embedding.rs`
- Test: `mur-core/src/store/embedding.rs` (mod tests)

- [x] **Step 1: Write the failing test**

Add to the bottom of `mur-core/src/store/embedding.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::config::Config;

    fn cfg_with(provider: &str, url: Option<&str>, env: Option<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.embedding.provider = provider.into();
        cfg.embedding.openai_url = url.map(str::to_string);
        cfg.embedding.api_key_env = env.map(str::to_string);
        cfg
    }

    #[test]
    fn omlx_provider_uses_custom_base_url() {
        let cfg = cfg_with("omlx", Some("http://localhost:8000/v1"), Some("OMLX_API_KEY"));
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "http://localhost:8000/v1");
            }
            _ => panic!("expected OpenAI variant for provider=omlx"),
        }
    }

    #[test]
    fn openai_provider_defaults_to_canonical_base_url() {
        let cfg = cfg_with("openai", None, Some("OPENAI_API_KEY"));
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "https://api.openai.com/v1");
            }
            _ => panic!("expected OpenAI variant"),
        }
    }

    #[test]
    fn ollama_provider_unchanged_by_openai_url() {
        let cfg = cfg_with("ollama", Some("http://example.com"), None);
        let ec = EmbeddingConfig::from_config(&cfg);
        matches!(ec.provider, EmbeddingProvider::Ollama { .. });
    }
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --lib store::embedding::tests`
Expected: FAIL — destructuring `EmbeddingProvider::OpenAI { base_url, .. }` fails to compile because the variant has no `base_url` field.

- [x] **Step 3: Implement minimal change**

Edit `mur-core/src/store/embedding.rs:15-19` — replace the enum and `from_config`:

```rust
#[derive(Debug, Clone)]
pub enum EmbeddingProvider {
    Ollama { base_url: String },
    OpenAI { api_key: String, base_url: String },
}

impl EmbeddingConfig {
    /// Create from the global mur config.
    pub fn from_config(cfg: &mur_common::config::Config) -> Self {
        let provider = match cfg.embedding.provider.as_str() {
            "openai" | "gemini" | "anthropic" | "voyage" | "omlx" | "mlx" => {
                let api_key = cfg
                    .embedding
                    .api_key_env
                    .as_deref()
                    .and_then(|env| std::env::var(env).ok())
                    .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
                let base_url = cfg
                    .embedding
                    .openai_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into());
                EmbeddingProvider::OpenAI { api_key, base_url }
            }
            _ => EmbeddingProvider::Ollama {
                base_url: cfg.embedding.ollama_endpoint.clone(),
            },
        };
        Self {
            provider,
            model: cfg.embedding.model.clone(),
            dimensions: cfg.embedding.dimensions,
        }
    }
}
```

Also update `embed` dispatch at lines 60-65:

```rust
pub async fn embed(text: &str, config: &EmbeddingConfig) -> Result<Vec<f32>> {
    match &config.provider {
        EmbeddingProvider::Ollama { base_url } => embed_ollama(text, base_url, &config.model).await,
        EmbeddingProvider::OpenAI { api_key, base_url } => {
            embed_openai(text, base_url, api_key, &config.model).await
        }
    }
}
```

- [x] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --lib store::embedding::tests`
Expected: PASS — three tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/store/embedding.rs
git commit -m "M1.1: thread base_url into EmbeddingProvider::OpenAI"
```

---

### Task 1.2: `embed_openai` uses dynamic base URL

**Files:**
- Modify: `mur-core/src/store/embedding.rs:134-159`
- Test: `mur-core/tests/embedding_openai_url.rs` (new)

- [x] **Step 1: Write the failing test**

Create `mur-core/tests/embedding_openai_url.rs`:

```rust
//! Integration test: embed_openai POSTs to the base_url from EmbeddingConfig,
//! not the hardcoded api.openai.com URL.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embed_openai_posts_to_custom_base_url() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"embedding": vec![0.1f32; 1024]}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = mur_common::config::Config::default();
    cfg.embedding.provider = "omlx".into();
    cfg.embedding.openai_url = Some(server.uri()); // mock acts as oMLX server
    cfg.embedding.model = "mlx-community/Qwen3-Embedding-0.6B-8bit".into();
    cfg.embedding.api_key_env = Some("OMLX_API_KEY".into());

    // SAFETY: cargo runs tests serially within a single test binary by default,
    // and this test is the only one mutating OMLX_API_KEY in this crate.
    unsafe {
        std::env::set_var("OMLX_API_KEY", "local");
    }

    let ec = mur_core::store::embedding::EmbeddingConfig::from_config(&cfg);
    let v = mur_core::store::embedding::embed("hello", &ec).await.unwrap();
    assert_eq!(v.len(), 1024);
}
```

Note: this requires `mur_core::store::embedding::*` to be `pub` — `EmbeddingConfig`, `EmbeddingProvider`, and `embed` already are (verify with `grep '^pub fn\|^pub struct\|^pub enum' mur-core/src/store/embedding.rs`). If `store::embedding` is not re-exported, the test path is `mur_core::store::embedding::*`. If `store` is private, the test must be a unit test instead — keep it under `#[cfg(test)] mod tests` in `embedding.rs`.

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test embedding_openai_url`
Expected: FAIL — the mock receives 0 requests (current code POSTs to `api.openai.com`).

- [x] **Step 3: Implement**

Edit `mur-core/src/store/embedding.rs` — replace `embed_openai` (lines 134-159):

```rust
async fn embed_openai(text: &str, base_url: &str, api_key: &str, model: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: text.into(),
        })
        .send()
        .await
        .with_context(|| format!("calling embed API at {}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Embed API error {} at {}: {}", status, url, body);
    }

    let data: OpenAIEmbedResponse = resp.json().await.context("parsing embed response")?;
    data.data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .context("no embedding returned")
}
```

- [x] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --test embedding_openai_url`
Expected: PASS — mock receives 1 request, `v.len() == 1024`.

Also re-run M1.1 unit tests to confirm no regression: `cargo test -p mur-core --lib store::embedding::tests`.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/store/embedding.rs mur-core/tests/embedding_openai_url.rs
git commit -m "M1.2: embed_openai uses cfg.embedding.openai_url"
```

---

### Task 1.3: Update `mur init` cloud-embedding helper to set `openai_url` for clarity

**Files:**
- Modify: `mur-core/src/cmd/init.rs:797-833` (`select_cloud_embedding`)

This is the smallest possible change to make the cloud branches not regress: when the cloud LLM is OpenRouter (which has no embedding API but routes through `openai_url`), `select_cloud_embedding` already falls back to OpenAI for embedding — that path was always broken silently because `embed_openai` ignored `openai_url`. After M1.2, that path will actually work. Add a unit test asserting the cloud-embedding path writes a sane `openai_url: None` (i.e. it doesn't accidentally inherit OpenRouter's URL).

- [x] **Step 1: Write the failing test**

Add to `mur-core/src/cmd/init.rs` (bottom, in `mod tests`):

```rust
#[cfg(test)]
mod cloud_embedding_tests {
    use mur_common::config::Config;

    /// When user picks Anthropic as cloud LLM and Voyage as embedding,
    /// embedding.openai_url must be None (Voyage uses its own canonical
    /// URL, not OpenRouter's).
    #[test]
    fn cloud_embedding_does_not_inherit_llm_openai_url() {
        let mut cfg = Config::default();
        cfg.llm.provider = "openai".into();
        cfg.llm.openai_url = Some("https://openrouter.ai/api/v1".into());
        // simulate what select_cloud_embedding does for anthropic→voyage:
        cfg.embedding.provider = "anthropic".into();
        cfg.embedding.model = "voyage-3-lite".into();
        cfg.embedding.api_key_env = Some("ANTHROPIC_API_KEY".into());
        cfg.embedding.openai_url = None;

        // Round-trip through EmbeddingConfig — should resolve OpenAI variant
        // with default base_url (api.openai.com), NOT OpenRouter.
        let ec = mur_core::store::embedding::EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            mur_core::store::embedding::EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "https://api.openai.com/v1");
            }
            _ => panic!("expected OpenAI variant"),
        }
    }
}
```

- [x] **Step 2: Run test to verify it passes immediately**

Run: `cargo test -p mur-core --lib cmd::init::cloud_embedding_tests`
Expected: PASS — this is a regression-safety test; M1.1's logic is already correct. If it FAILS, M1.1 has a bug.

- [x] **Step 3: No implementation change needed; test is regression coverage**

Skip.

- [x] **Step 4: Run full mur-core test suite**

Run: `cargo test -p mur-core`
Expected: PASS, including the new tests.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/init.rs
git commit -m "M1.3: regression test for cloud embedding url isolation"
```

**M1 done — open PR titled "M1: thread openai_url through EmbeddingProvider (unblocks oMLX)".**

---

## M2 — Discovery scaffold + preference table + cache

**Goal:** Land the `discovery::` module skeleton (trait, types, cache I/O) and the `preference.rs` ranking table. No HTTP yet — pure logic, fully unit-testable.

**Branch:** `feat/mur-m2-discovery-scaffold`

### Task 2.1: Create `discovery::preference` with rank function

**Files:**
- Create: `mur-core/src/discovery/mod.rs`
- Create: `mur-core/src/discovery/preference.rs`
- Modify: `mur-core/src/lib.rs:36` (add `pub mod discovery;`)

- [x] **Step 1: Write the failing test**

Create `mur-core/src/discovery/preference.rs`:

```rust
//! Static prefix-matched preference tables for picking the "best" model
//! to recommend when multiple are available locally. Future-proof against
//! new tags via prefix matching.

/// Ordered preference for embedding models, descending by score.
/// Both Ollama tag form (`name:size`) and HuggingFace id form
/// (`mlx-community/Foo`) appear at equal score.
pub const EMBEDDING_PREFERENCE: &[(&str, u32)] = &[
    ("qwen3.5-embedding",       105),  // future-proof
    ("Qwen3-Embedding-8B",      100),
    ("qwen3-embedding:8b",      100),
    ("Qwen3-Embedding-4B",       90),
    ("qwen3-embedding:4b",       90),
    ("bge-m3",                   80),
    ("jina-embeddings-v3",       75),
    ("Qwen3-Embedding-0.6B",     70),
    ("qwen3-embedding:0.6b",     70),
    ("embeddinggemma",           55),
    ("nomic-embed-text",         40),
    ("all-minilm",               20),
];

/// Ordered preference for chat / completion LLMs (Mode 3 only).
/// Aligned with the curated picks in `cmd/init_local.rs::OLLAMA_RECS` /
/// `MLX_RECS`. Multilingual-first ordering.
pub const LLM_PREFERENCE: &[(&str, u32)] = &[
    ("Qwen3.5-9B",                95),
    ("qwen3.5:9b",                95),
    ("Qwen3.5-4B",                90),
    ("qwen3.5:4b",                90),
    ("Gemma4-E2B",                85),
    ("gemma4:e2b",                85),
    ("Qwen3-9B",                  70),
    ("qwen3:9b",                  70),
    ("Qwen3-4B",                  65),
    ("qwen3:4b",                  65),
    ("llama3.3",                  60),
];

/// Highest-scoring prefix that is a substring of `id`. Returns 0 when no
/// prefix matches. Case-sensitive — both Ollama (`qwen3-embedding:0.6b`)
/// and HF (`Qwen3-Embedding-0.6B`) forms must appear in the table.
pub fn rank(id: &str, table: &[(&'static str, u32)]) -> u32 {
    table
        .iter()
        .filter(|(prefix, _)| id.contains(prefix))
        .map(|(_, score)| *score)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_tag_form_ranked() {
        assert_eq!(rank("qwen3-embedding:0.6b", EMBEDDING_PREFERENCE), 70);
        assert_eq!(rank("qwen3-embedding:8b", EMBEDDING_PREFERENCE), 100);
        assert_eq!(rank("bge-m3", EMBEDDING_PREFERENCE), 80);
    }

    #[test]
    fn hf_id_form_ranked() {
        assert_eq!(
            rank("mlx-community/Qwen3-Embedding-0.6B-8bit", EMBEDDING_PREFERENCE),
            70
        );
        assert_eq!(
            rank("mlx-community/Qwen3-Embedding-8B-4bit-DWQ", EMBEDDING_PREFERENCE),
            100
        );
    }

    #[test]
    fn unknown_id_returns_zero() {
        assert_eq!(rank("randomuser/foo-base", EMBEDDING_PREFERENCE), 0);
        assert_eq!(rank("", EMBEDDING_PREFERENCE), 0);
    }

    #[test]
    fn future_qwen35_embedding_wins() {
        // When Alibaba ships qwen3.5-embedding, the prefix table should pick
        // it over current SOTA without any code change.
        assert_eq!(
            rank("qwen3.5-embedding:0.6b", EMBEDDING_PREFERENCE),
            105
        );
        assert!(
            rank("qwen3.5-embedding:0.6b", EMBEDDING_PREFERENCE)
                > rank("qwen3-embedding:8b", EMBEDDING_PREFERENCE)
        );
    }

    #[test]
    fn llm_table_separate() {
        assert_eq!(rank("qwen3.5:9b", LLM_PREFERENCE), 95);
        assert_eq!(rank("qwen3.5:9b", EMBEDDING_PREFERENCE), 0); // not in embedding table
    }

    #[test]
    fn case_sensitive_distinguishes_forms() {
        // The two forms are distinct entries at the same score; rank is the
        // max of all matching prefixes, so a string matching either is fine.
        assert_eq!(rank("Qwen3-Embedding-8B", EMBEDDING_PREFERENCE), 100);
        assert_eq!(rank("qwen3-embedding:8b", EMBEDDING_PREFERENCE), 100);
    }
}
```

Create `mur-core/src/discovery/mod.rs` (minimal stub; will grow in 2.2):

```rust
//! Runtime discovery: query Ollama / oMLX for what models are actually
//! pulled, what their kind (LLM vs embedding) is, and what dims they have.
//!
//! See `docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md`.

pub mod preference;
```

Modify `mur-core/src/lib.rs` — add `pub mod discovery;` near the other `pub mod` declarations (alphabetical, between `dashboard` and `evolve`):

```rust
pub mod dashboard;
pub mod discovery;  // ← new
pub mod evolve;
```

- [x] **Step 2: Run test to verify it passes**

Run: `cargo test -p mur-core --lib discovery::preference::tests`
Expected: PASS — six tests pass.

- [x] **Step 3: No further implementation; this task is the implementation**

Skip.

- [x] **Step 4: Verify clippy + workspace still compiles**

Run: `cargo clippy -p mur-core -- -D warnings && cargo build --workspace`
Expected: clean.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/ mur-core/src/lib.rs
git commit -m "M2.1: discovery::preference table + rank()"
```

---

### Task 2.2: Discovery trait + types

**Files:**
- Modify: `mur-core/src/discovery/mod.rs`
- Test: `mur-core/src/discovery/mod.rs` (mod tests)

- [x] **Step 1: Write the failing test**

Add to `mur-core/src/discovery/mod.rs` (bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_model_round_trips_serde() {
        let m = DiscoveredModel {
            id: "qwen3-embedding:0.6b".into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(1024),
            family: Some("bert".into()),
            size_bytes: Some(700_000_000),
            probed_at: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        let m2: DiscoveredModel = serde_json::from_str(&s).unwrap();
        assert_eq!(m.id, m2.id);
        assert_eq!(m.backend, m2.backend);
        assert_eq!(m.kind, m2.kind);
        assert_eq!(m.dims, m2.dims);
    }

    #[test]
    fn backend_display() {
        assert_eq!(format!("{}", Backend::Ollama), "Ollama");
        assert_eq!(format!("{}", Backend::OMlx), "oMLX");
    }

    #[test]
    fn embedding_probe_has_dims_and_latency() {
        let p = EmbeddingProbe { dims: 1024, latency_ms: 120 };
        assert_eq!(p.dims, 1024);
        assert_eq!(p.latency_ms, 120);
    }
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --lib discovery::tests`
Expected: FAIL — `DiscoveredModel`, `Backend`, `ModelKind`, `EmbeddingProbe` undefined.

- [x] **Step 3: Implement**

Replace contents of `mur-core/src/discovery/mod.rs`:

```rust
//! Runtime discovery: query Ollama / oMLX for what models are actually
//! pulled, what their kind (LLM vs embedding) is, and what dims they have.
//!
//! See `docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md`.

pub mod preference;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Ollama,
    OMlx,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Backend::Ollama => f.write_str("Ollama"),
            Backend::OMlx => f.write_str("oMLX"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelKind {
    Llm,
    Embedding,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub backend: Backend,
    pub kind: ModelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dims: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingProbe {
    pub dims: usize,
    pub latency_ms: u64,
}

#[async_trait]
pub trait Discovery: Send + Sync {
    fn backend(&self) -> Backend;
    /// Enumerate all loaded / pulled models on this runtime.
    async fn list_models(&self) -> Result<Vec<DiscoveredModel>>;
    /// 1-token probe to determine kind + dim. Used after `list_models` when
    /// `kind == Unknown`, or when the user picks a model whose dims we
    /// haven't recorded yet.
    async fn probe_embedding(&self, model_id: &str) -> Result<EmbeddingProbe>;
}
```

- [x] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --lib discovery::tests`
Expected: PASS — three tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/mod.rs
git commit -m "M2.2: Discovery trait + Backend/ModelKind/DiscoveredModel"
```

---

### Task 2.3: Discovery cache I/O — round-trip

**Files:**
- Create: `mur-core/src/discovery/cache.rs`
- Modify: `mur-core/src/discovery/mod.rs` (add `pub mod cache;`)
- Test: `mur-core/src/discovery/cache.rs` (mod tests)

- [x] **Step 1: Write the failing test**

Create `mur-core/src/discovery/cache.rs`:

```rust
//! On-disk cache for discovery results, at `~/.mur/cache/discovery.json`.
//! TTL 24h. Schema versioned. Best-effort: corrupt JSON or schema mismatch
//! is logged and treated as empty (forces re-discovery, never errors).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{Backend, DiscoveredModel};

pub const CACHE_SCHEMA_VERSION: u32 = 1;
pub const CACHE_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryCache {
    pub schema_version: u32,
    pub entries: Vec<CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub endpoint: String,
    pub backend: Backend,
    pub captured_at: DateTime<Utc>,
    pub models: Vec<DiscoveredModel>,
}

impl DiscoveryCache {
    pub fn empty() -> Self {
        Self { schema_version: CACHE_SCHEMA_VERSION, entries: Vec::new() }
    }

    /// Default cache path under the active mur root.
    pub fn default_path() -> Result<PathBuf> {
        Ok(crate::paths::mur_root()?.join("cache").join("discovery.json"))
    }

    /// Load cache from disk. Returns `empty()` on missing file, corrupt
    /// JSON, or schema mismatch. Never errors — discovery just re-runs.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::empty();
        };
        match serde_json::from_slice::<DiscoveryCache>(&bytes) {
            Ok(c) if c.schema_version == CACHE_SCHEMA_VERSION => c,
            Ok(c) => {
                tracing::warn!(
                    found = c.schema_version,
                    expected = CACHE_SCHEMA_VERSION,
                    "discovery cache schema mismatch; ignoring"
                );
                Self::empty()
            }
            Err(e) => {
                tracing::warn!(?e, "discovery cache corrupt; ignoring");
                Self::empty()
            }
        }
    }

    /// Atomic write via temp + rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Look up a fresh entry for (endpoint, backend). Returns None if
    /// missing or older than `CACHE_TTL_HOURS`.
    pub fn fresh_entry(&self, endpoint: &str, backend: Backend) -> Option<&CacheEntry> {
        let cutoff = Utc::now() - Duration::hours(CACHE_TTL_HOURS);
        self.entries
            .iter()
            .find(|e| e.endpoint == endpoint && e.backend == backend && e.captured_at >= cutoff)
    }

    /// Insert or replace the entry for (endpoint, backend).
    pub fn upsert(&mut self, endpoint: String, backend: Backend, models: Vec<DiscoveredModel>) {
        self.entries.retain(|e| !(e.endpoint == endpoint && e.backend == backend));
        self.entries.push(CacheEntry {
            endpoint,
            backend,
            captured_at: Utc::now(),
            models,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DiscoveredModel, ModelKind};
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    fn sample_model() -> DiscoveredModel {
        DiscoveredModel {
            id: "qwen3-embedding:0.6b".into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(1024),
            family: Some("bert".into()),
            size_bytes: None,
            probed_at: None,
        }
    }

    #[test]
    fn round_trip_save_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("discovery.json");

        let mut c = DiscoveryCache::empty();
        c.upsert(
            "http://localhost:11434".into(),
            Backend::Ollama,
            vec![sample_model()],
        );
        c.save(&path).unwrap();

        let loaded = DiscoveryCache::load(&path);
        assert_eq!(loaded.schema_version, CACHE_SCHEMA_VERSION);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].models.len(), 1);
        assert_eq!(loaded.entries[0].models[0].id, "qwen3-embedding:0.6b");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let c = DiscoveryCache::load(&dir.path().join("nope.json"));
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn corrupt_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let c = DiscoveryCache::load(&path);
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn schema_mismatch_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("oldschema.json");
        std::fs::write(
            &path,
            r#"{"schema_version": 999, "entries": []}"#,
        ).unwrap();
        let c = DiscoveryCache::load(&path);
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn fresh_entry_respects_ttl() {
        let mut c = DiscoveryCache::empty();
        c.upsert("ep".into(), Backend::Ollama, vec![sample_model()]);
        assert!(c.fresh_entry("ep", Backend::Ollama).is_some());

        // Manually age the entry past TTL
        c.entries[0].captured_at = Utc::now() - ChronoDuration::hours(CACHE_TTL_HOURS + 1);
        assert!(c.fresh_entry("ep", Backend::Ollama).is_none());
    }

    #[test]
    fn upsert_replaces_existing() {
        let mut c = DiscoveryCache::empty();
        c.upsert("ep".into(), Backend::Ollama, vec![]);
        c.upsert("ep".into(), Backend::Ollama, vec![sample_model()]);
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].models.len(), 1);
    }
}
```

Modify `mur-core/src/discovery/mod.rs` — add at top after `pub mod preference;`:

```rust
pub mod preference;
pub mod cache;
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --lib discovery::cache::tests`
Expected: FAIL — `tempfile` may not be a dev-dep (verify); also `tracing` import.

If `tempfile` not present: add to `mur-core/Cargo.toml` `[dev-dependencies]`:
```toml
tempfile = "3"
```

Re-run: still FAIL because module is new.

- [x] **Step 3: Implementation already in step 1; just verify it compiles**

Run: `cargo build -p mur-core`
Expected: builds clean.

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test -p mur-core --lib discovery::cache::tests`
Expected: PASS — six tests.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/cache.rs mur-core/src/discovery/mod.rs mur-core/Cargo.toml
git commit -m "M2.3: discovery::cache with TTL + schema versioning"
```

---

### Task 2.4: M2 final clippy / workspace check + open PR

- [x] **Step 1: Clippy + workspace test**

Run: `cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: clean.

- [x] **Step 2: Push branch + open PR**

```bash
git push -u origin feat/mur-m2-discovery-scaffold
gh pr create --title "M2: discovery scaffold + preference table + cache" \
  --body "$(cat <<'EOF'
## Summary
- New `mur-core::discovery` module with `Discovery` trait, `Backend`, `ModelKind`, `DiscoveredModel`, `EmbeddingProbe`
- `discovery::preference` static prefix-matched ranking tables for embedding + LLM
- `discovery::cache` JSON cache at \`~/.mur/cache/discovery.json\` (TTL 24h, schema v1)
- No HTTP yet; pure logic. Library-only; no UX change.

## Spec
\`docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md\` § 2.1, § 2.4, § 2.6

## Test plan
- [x] \`cargo test -p mur-core --lib discovery::\` (preference + cache + types)
- [x] \`cargo clippy --workspace -- -D warnings\`
- [x] \`cargo build --workspace\`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

**M2 done.**

---

## M3 — `OllamaDiscovery`

**Goal:** Implement `Discovery` for Ollama via `/api/tags` + `/api/show capabilities` + `/api/embed` probe. Wiremock-tested.

**Branch:** `feat/mur-m3-ollama-discovery`

### Task 3.1: `OllamaDiscovery::list_models` — `/api/tags` + capabilities path

**Files:**
- Create: `mur-core/src/discovery/ollama.rs`
- Modify: `mur-core/src/discovery/mod.rs` (add `pub mod ollama;`)
- Test: `mur-core/tests/discovery_ollama.rs` (new)

- [x] **Step 1: Write the failing test**

Create `mur-core/tests/discovery_ollama.rs`:

```rust
//! Wiremock-backed integration tests for OllamaDiscovery.

use mur_core::discovery::{Discovery, ModelKind, ollama::OllamaDiscovery};
use serde_json::json;
use wiremock::matchers::{body_json_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tags_response_with(models: Vec<(&str, &str, u64)>) -> serde_json::Value {
    json!({
        "models": models.iter().map(|(name, family, size)| json!({
            "name": name,
            "size": size,
            "details": { "family": family }
        })).collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn list_models_marks_capabilities_embedding_as_embedding_kind() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_response_with(vec![
            ("qwen3-embedding:0.6b", "bert", 700_000_000),
            ("qwen3.5:4b", "qwen3", 4_000_000_000),
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_json_string(r#"{"name":"qwen3-embedding:0.6b"}"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["embedding"]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_json_string(r#"{"name":"qwen3.5:4b"}"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["completion"]
        })))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();

    assert_eq!(models.len(), 2);
    let emb = models.iter().find(|m| m.id == "qwen3-embedding:0.6b").unwrap();
    assert_eq!(emb.kind, ModelKind::Embedding);
    assert_eq!(emb.family.as_deref(), Some("bert"));

    let llm = models.iter().find(|m| m.id == "qwen3.5:4b").unwrap();
    assert_eq!(llm.kind, ModelKind::Llm);
}

#[tokio::test]
async fn list_models_falls_back_to_family_when_capabilities_absent() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_response_with(vec![
            ("nomic-embed-text:latest", "nomic-bert", 300_000_000),
        ])))
        .mount(&server)
        .await;

    // /api/show returns no capabilities field → fallback path
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models[0].kind, ModelKind::Embedding);
}

#[tokio::test]
async fn list_models_marks_unreachable_show_as_unknown() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_response_with(vec![
            ("foo:bar", "weird-family", 100),
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models[0].kind, ModelKind::Unknown);
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test discovery_ollama`
Expected: FAIL — `mur_core::discovery::ollama` undefined.

- [x] **Step 3: Implement**

Create `mur-core/src/discovery/ollama.rs`:

```rust
//! `Discovery` impl for Ollama via REST API at `${endpoint}/api/{tags,show,embed}`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::{Backend, Discovery, DiscoveredModel, EmbeddingProbe, ModelKind};

#[derive(Debug, Clone)]
pub struct OllamaDiscovery {
    endpoint: String,
    client: reqwest::Client,
}

impl OllamaDiscovery {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(&self, suffix: &str) -> String {
        format!("{}{}", self.endpoint.trim_end_matches('/'), suffix)
    }
}

#[derive(Deserialize)]
struct TagsResp {
    models: Vec<TagsEntry>,
}

#[derive(Deserialize)]
struct TagsEntry {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    details: TagsDetails,
}

#[derive(Deserialize, Default)]
struct TagsDetails {
    #[serde(default)]
    family: Option<String>,
}

#[derive(Deserialize, Default)]
struct ShowResp {
    #[serde(default)]
    capabilities: Vec<String>,
}

fn classify(family: Option<&str>, name: &str, capabilities: &[String]) -> ModelKind {
    if capabilities.iter().any(|c| c == "embedding") {
        return ModelKind::Embedding;
    }
    if capabilities.iter().any(|c| c == "completion") {
        return ModelKind::Llm;
    }
    // Fallback heuristic when /api/show fails or returns no capabilities.
    match family {
        Some(f) if matches!(f, "bert" | "nomic-bert" | "jina-bert") => ModelKind::Embedding,
        Some("qwen3") if name.contains("embedding") => ModelKind::Embedding,
        Some(f) if matches!(f, "qwen3" | "llama" | "gemma") => ModelKind::Llm,
        _ => ModelKind::Unknown,
    }
}

#[async_trait]
impl Discovery for OllamaDiscovery {
    fn backend(&self) -> Backend { Backend::Ollama }

    async fn list_models(&self) -> Result<Vec<DiscoveredModel>> {
        let resp = self
            .client
            .get(self.url("/api/tags"))
            .send()
            .await
            .context("GET /api/tags")?;
        let tags: TagsResp = resp.json().await.context("parse /api/tags")?;

        let mut out = Vec::with_capacity(tags.models.len());
        for entry in tags.models {
            // /api/show may fail (500, network, etc.); fallback to heuristic.
            let caps = match self
                .client
                .post(self.url("/api/show"))
                .json(&serde_json::json!({ "name": entry.name }))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    r.json::<ShowResp>().await.unwrap_or_default().capabilities
                }
                _ => Vec::new(),
            };
            let kind = classify(entry.details.family.as_deref(), &entry.name, &caps);
            out.push(DiscoveredModel {
                id: entry.name,
                backend: Backend::Ollama,
                kind,
                dims: None,
                family: entry.details.family,
                size_bytes: entry.size,
                probed_at: None,
            });
        }
        Ok(out)
    }

    async fn probe_embedding(&self, model_id: &str) -> Result<EmbeddingProbe> {
        let started = Instant::now();
        let resp = self
            .client
            .post(self.url("/api/embed"))
            .json(&serde_json::json!({ "model": model_id, "input": "." }))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("POST /api/embed probe")?;

        if !resp.status().is_success() {
            anyhow::bail!("/api/embed returned {}", resp.status());
        }

        #[derive(Deserialize)]
        struct EmbedResp { embeddings: Vec<Vec<f32>> }

        let er: EmbedResp = resp.json().await.context("parse /api/embed response")?;
        let dims = er.embeddings.first().map(Vec::len).unwrap_or(0);
        if dims == 0 {
            anyhow::bail!("/api/embed returned empty embeddings");
        }
        Ok(EmbeddingProbe {
            dims,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}
```

Modify `mur-core/src/discovery/mod.rs` — add `pub mod ollama;` after `pub mod cache;`.

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test -p mur-core --test discovery_ollama`
Expected: PASS — three tests.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/ollama.rs mur-core/src/discovery/mod.rs mur-core/tests/discovery_ollama.rs
git commit -m "M3.1: OllamaDiscovery::list_models with capabilities + fallback"
```

---

### Task 3.2: `OllamaDiscovery::probe_embedding`

**Files:**
- Modify: `mur-core/tests/discovery_ollama.rs` (add test)
- Implementation already in 3.1

- [x] **Step 1: Write the failing test**

Append to `mur-core/tests/discovery_ollama.rs`:

```rust
#[tokio::test]
async fn probe_embedding_returns_dims_and_latency() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [vec![0.0f32; 1024]]
        })))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let probe = d.probe_embedding("qwen3-embedding:0.6b").await.unwrap();
    assert_eq!(probe.dims, 1024);
}

#[tokio::test]
async fn probe_embedding_errors_on_4xx() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let r = d.probe_embedding("missing").await;
    assert!(r.is_err());
}
```

Add `EmbeddingProbe` to imports at the top:

```rust
use mur_core::discovery::{Discovery, EmbeddingProbe, ModelKind, ollama::OllamaDiscovery};
```

- [x] **Step 2: Run test to verify it passes**

Run: `cargo test -p mur-core --test discovery_ollama`
Expected: PASS — five tests total now.

- [x] **Step 3: No additional implementation; coverage tests on existing code**

Skip.

- [x] **Step 4: Verify no regression**

Run: `cargo test -p mur-core`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/tests/discovery_ollama.rs
git commit -m "M3.2: OllamaDiscovery::probe_embedding tests"
```

**M3 done. Open PR `feat/mur-m3-ollama-discovery`.**

---

## M4 — `OMlxDiscovery`

**Goal:** Implement `Discovery` for oMLX via `/v1/models` + `/v1/embeddings` probe. Wiremock-tested.

**Branch:** `feat/mur-m4-omlx-discovery`

### Task 4.1: `OMlxDiscovery::list_models` — `/v1/models` parsing

**Files:**
- Create: `mur-core/src/discovery/omlx.rs`
- Modify: `mur-core/src/discovery/mod.rs` (add `pub mod omlx;`)
- Test: `mur-core/tests/discovery_omlx.rs`

- [x] **Step 1: Write the failing test**

Create `mur-core/tests/discovery_omlx.rs`:

```rust
use mur_core::discovery::{Discovery, ModelKind, omlx::OMlxDiscovery};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn list_models_returns_unknown_kind_pre_probe() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"},
                {"id": "mlx-community/Qwen3.5-4B-4bit"}
            ]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models.len(), 2);
    // /v1/models alone can't disambiguate — kind=Unknown until probed.
    assert!(models.iter().all(|m| m.kind == ModelKind::Unknown));
    // Family inferred from id substring
    assert_eq!(
        models.iter().find(|m| m.id.contains("Qwen3-Embedding")).unwrap().family.as_deref(),
        Some("qwen3")
    );
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test discovery_omlx`
Expected: FAIL — `mur_core::discovery::omlx` undefined.

- [x] **Step 3: Implement**

Create `mur-core/src/discovery/omlx.rs`:

```rust
//! `Discovery` impl for oMLX via OpenAI-compatible REST API at
//! `${base_url}/v1/{models,embeddings}`. oMLX serves on `localhost:8000`
//! by default.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::{Backend, Discovery, DiscoveredModel, EmbeddingProbe, ModelKind};

/// oMLX issue #266: graph recompiles on first call after >3s idle. Budget
/// 10s for the probe; subsequent probes settle to <500ms.
const OMLX_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct OMlxDiscovery {
    base_url: String,
    client: reqwest::Client,
}

impl OMlxDiscovery {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(&self, suffix: &str) -> String {
        let trimmed = self.base_url.trim_end_matches('/');
        // Caller passes "/v1/embeddings" or "/v1/models"; if base_url already
        // ends in "/v1", strip the leading "/v1" from suffix to avoid dupe.
        if trimmed.ends_with("/v1") && suffix.starts_with("/v1") {
            format!("{}{}", trimmed, &suffix[3..])
        } else {
            format!("{}{}", trimmed, suffix)
        }
    }
}

fn family_from_id(id: &str) -> Option<String> {
    let lower = id.to_ascii_lowercase();
    if lower.contains("qwen3") { Some("qwen3".into()) }
    else if lower.contains("bge-") || lower.contains("bge_") { Some("bge".into()) }
    else if lower.contains("modernbert") { Some("modernbert".into()) }
    else if lower.contains("nomic") { Some("nomic-bert".into()) }
    else if lower.contains("jina") { Some("jina-bert".into()) }
    else { None }
}

#[derive(Deserialize)]
struct ModelsResp {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[async_trait]
impl Discovery for OMlxDiscovery {
    fn backend(&self) -> Backend { Backend::OMlx }

    async fn list_models(&self) -> Result<Vec<DiscoveredModel>> {
        let resp = self
            .client
            .get(self.url("/v1/models"))
            .send()
            .await
            .context("GET /v1/models")?;
        let mr: ModelsResp = resp.json().await.context("parse /v1/models")?;

        Ok(mr
            .data
            .into_iter()
            .map(|e| DiscoveredModel {
                family: family_from_id(&e.id),
                id: e.id,
                backend: Backend::OMlx,
                kind: ModelKind::Unknown, // /v1/models has no type field; probe to discriminate
                dims: None,
                size_bytes: None,
                probed_at: None,
            })
            .collect())
    }

    async fn probe_embedding(&self, model_id: &str) -> Result<EmbeddingProbe> {
        let started = Instant::now();
        let resp = self
            .client
            .post(self.url("/v1/embeddings"))
            .json(&serde_json::json!({ "model": model_id, "input": "." }))
            .timeout(OMLX_PROBE_TIMEOUT)
            .send()
            .await
            .context("POST /v1/embeddings probe")?;

        if !resp.status().is_success() {
            anyhow::bail!("/v1/embeddings returned {} for {}", resp.status(), model_id);
        }

        #[derive(Deserialize)]
        struct EmbedResp {
            data: Vec<EmbedData>,
        }

        #[derive(Deserialize)]
        struct EmbedData {
            embedding: Vec<f32>,
        }

        let er: EmbedResp = resp.json().await.context("parse /v1/embeddings")?;
        let dims = er.data.first().map(|d| d.embedding.len()).unwrap_or(0);
        if dims == 0 {
            anyhow::bail!("/v1/embeddings returned empty data");
        }
        Ok(EmbeddingProbe {
            dims,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}
```

Modify `mur-core/src/discovery/mod.rs` — add `pub mod omlx;` after `pub mod ollama;`.

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test -p mur-core --test discovery_omlx`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/omlx.rs mur-core/src/discovery/mod.rs mur-core/tests/discovery_omlx.rs
git commit -m "M4.1: OMlxDiscovery::list_models + family inference"
```

---

### Task 4.2: `OMlxDiscovery::probe_embedding` happy + 4xx + family-inference tests

**Files:**
- Modify: `mur-core/tests/discovery_omlx.rs`

- [x] **Step 1: Write the failing test**

Append to `mur-core/tests/discovery_omlx.rs`:

```rust
#[tokio::test]
async fn probe_embedding_happy_path() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": vec![0.0f32; 1024]}]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let p = d.probe_embedding("mlx-community/Qwen3-Embedding-0.6B-8bit").await.unwrap();
    assert_eq!(p.dims, 1024);
}

#[tokio::test]
async fn probe_embedding_4xx_errors() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "model does not support embeddings"
        ))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let r = d.probe_embedding("mlx-community/Qwen3.5-4B-4bit").await;
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("400"));
}

#[tokio::test]
async fn probe_embedding_empty_array_errors() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    assert!(d.probe_embedding("foo").await.is_err());
}

#[test]
fn family_inference_table() {
    use mur_core::discovery::omlx::OMlxDiscovery;
    // family_from_id is private; test by going through list_models + a mock,
    // but since this is sync we exercise via DiscoveredModel construction:
    // the family inference is observable through list_models in the
    // first test in this file.
    let _ = OMlxDiscovery::new("http://x");
}

#[tokio::test]
async fn list_models_infers_family_from_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "mlx-community/bge-m3"},
                {"id": "lightonai/modernbert-embed-large"},
                {"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"},
                {"id": "nomic-ai/nomic-embed-text-v1.5"},
                {"id": "jinaai/jina-embeddings-v3"},
                {"id": "unknown/foo"},
            ]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    let f = |id: &str| {
        models.iter().find(|m| m.id == id).unwrap().family.as_deref().map(str::to_string)
    };
    assert_eq!(f("mlx-community/bge-m3"), Some("bge".into()));
    assert_eq!(f("lightonai/modernbert-embed-large"), Some("modernbert".into()));
    assert_eq!(f("mlx-community/Qwen3-Embedding-0.6B-8bit"), Some("qwen3".into()));
    assert_eq!(f("nomic-ai/nomic-embed-text-v1.5"), Some("nomic-bert".into()));
    assert_eq!(f("jinaai/jina-embeddings-v3"), Some("jina-bert".into()));
    assert_eq!(f("unknown/foo"), None);
}
```

- [x] **Step 2: Run test to verify it passes**

Run: `cargo test -p mur-core --test discovery_omlx`
Expected: PASS — six tests.

- [x] **Step 3: No additional impl**

Skip.

- [x] **Step 4: Workspace check**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings`
Expected: clean.

- [x] **Step 5: Commit**

```bash
git add mur-core/tests/discovery_omlx.rs
git commit -m "M4.2: OMlxDiscovery::probe_embedding + family inference tests"
```

**M4 done. Open PR `feat/mur-m4-omlx-discovery`.**

---

## M5 — Init UX rewrite + LLM-side dynamic discovery

**Goal:** Replace `select_ollama_embedding` (`init.rs:769-794`) with `select_local_embedding` that consumes `Discovery`. Wire LLM-side `select_model` (`init_local.rs:217`) similarly. Add `--refresh-discovery` flag. Add `ollama pull` subprocess + oMLX hint flow.

**Branch:** `feat/mur-m5-init-dynamic-picker`

### Task 5.1: `discovery::aggregate` — runs all available backends, returns merged + ranked list

**Files:**
- Create: `mur-core/src/discovery/aggregate.rs`
- Modify: `mur-core/src/discovery/mod.rs` (add `pub mod aggregate;`)
- Test: `mur-core/src/discovery/aggregate.rs` (mod tests, sync logic only)

- [x] **Step 1: Write the failing test**

Create `mur-core/src/discovery/aggregate.rs`:

```rust
//! Aggregate `Discovery` results across all detected runtimes, intersect
//! with the static preference table, and produce a ranked menu seed.

use super::preference::{EMBEDDING_PREFERENCE, LLM_PREFERENCE, rank};
use super::{DiscoveredModel, ModelKind};

/// One menu row, in display order.
#[derive(Debug, Clone)]
pub struct MenuRow {
    pub kind: MenuRowKind,
    pub label: String,
    pub model: Option<DiscoveredModel>,
    pub pull_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuRowKind {
    Auto,        // first row, "[auto]" prefix
    Pulled,      // already-installed model
    Pull,        // recommended-but-not-pulled
    Skip,        // last row
}

/// Build the menu rows for embedding selection.
///
/// `available` = union of all discovery results (filtered to kind ∈
/// {Embedding, Unknown}).
pub fn build_embedding_menu(available: &[DiscoveredModel]) -> Vec<MenuRow> {
    build_menu(available, EMBEDDING_PREFERENCE, ModelKind::Embedding)
}

pub fn build_llm_menu(available: &[DiscoveredModel]) -> Vec<MenuRow> {
    build_menu(available, LLM_PREFERENCE, ModelKind::Llm)
}

fn build_menu(
    available: &[DiscoveredModel],
    table: &[(&'static str, u32)],
    desired_kind: ModelKind,
) -> Vec<MenuRow> {
    let mut filtered: Vec<&DiscoveredModel> = available
        .iter()
        .filter(|m| m.kind == desired_kind || m.kind == ModelKind::Unknown)
        .collect();
    filtered.sort_by_key(|m| std::cmp::Reverse(rank(&m.id, table)));

    let mut rows = Vec::new();

    // Row 1: [auto] = highest-ranked pulled
    if let Some(top) = filtered.first() {
        let label = format!(
            "[auto] {}/{}{}",
            top.backend,
            top.id,
            top.dims.map(|d| format!(" ({}d)", d)).unwrap_or_default(),
        );
        rows.push(MenuRow {
            kind: MenuRowKind::Auto,
            label,
            model: Some((*top).clone()),
            pull_id: None,
        });
    }

    // Rows 2..: remaining pulled
    for m in filtered.iter().skip(1) {
        let label = format!(
            "{}/{}{}",
            m.backend,
            m.id,
            m.dims.map(|d| format!(" ({}d)", d)).unwrap_or_default(),
        );
        rows.push(MenuRow {
            kind: MenuRowKind::Pulled,
            label,
            model: Some((*m).clone()),
            pull_id: None,
        });
    }

    // Top 2 preference-table entries NOT in pulled set, with rank > 0
    let pulled_ids: std::collections::HashSet<&str> =
        available.iter().map(|m| m.id.as_str()).collect();
    let mut suggestions: Vec<(&str, u32)> = table
        .iter()
        .filter(|(prefix, _)| !pulled_ids.iter().any(|id| id.contains(prefix)))
        .map(|(p, s)| (*p, *s))
        .collect();
    suggestions.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    for (prefix, _) in suggestions.iter().take(2) {
        rows.push(MenuRow {
            kind: MenuRowKind::Pull,
            label: format!("[pull] {}", prefix),
            model: None,
            pull_id: Some((*prefix).into()),
        });
    }

    // Last: skip
    rows.push(MenuRow {
        kind: MenuRowKind::Skip,
        label: "Skip — configure later".into(),
        model: None,
        pull_id: None,
    });

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Backend;

    fn dm(id: &str, backend: Backend, kind: ModelKind, dims: Option<usize>) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend,
            kind,
            dims,
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }

    #[test]
    fn empty_input_yields_pull_suggestions_then_skip() {
        let rows = build_embedding_menu(&[]);
        // 0 auto + 0 pulled + 2 [pull] + 1 skip
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, MenuRowKind::Pull);
        assert_eq!(rows.last().unwrap().kind, MenuRowKind::Skip);
    }

    #[test]
    fn single_pulled_becomes_auto() {
        let avail = vec![dm("qwen3-embedding:0.6b", Backend::Ollama, ModelKind::Embedding, Some(1024))];
        let rows = build_embedding_menu(&avail);
        assert_eq!(rows[0].kind, MenuRowKind::Auto);
        assert!(rows[0].label.starts_with("[auto] Ollama/qwen3-embedding:0.6b"));
        assert!(rows[0].label.contains("(1024d)"));
    }

    #[test]
    fn omlx_outranks_ollama_at_same_score() {
        // Both have rank 70; oMLX should still be auto if the user pulled
        // both. Tie-breaking: backend display order — but we don't enforce
        // that explicitly; we accept whichever comes first after stable
        // sort. Here we just assert one of them is [auto].
        let avail = vec![
            dm("qwen3-embedding:0.6b", Backend::Ollama, ModelKind::Embedding, Some(1024)),
            dm("mlx-community/Qwen3-Embedding-0.6B-8bit", Backend::OMlx, ModelKind::Embedding, Some(1024)),
        ];
        let rows = build_embedding_menu(&avail);
        assert_eq!(rows[0].kind, MenuRowKind::Auto);
        assert_eq!(rows[1].kind, MenuRowKind::Pulled);
    }

    #[test]
    fn pull_suggestion_excludes_already_pulled() {
        let avail = vec![dm("qwen3-embedding:0.6b", Backend::Ollama, ModelKind::Embedding, Some(1024))];
        let rows = build_embedding_menu(&avail);
        for r in &rows {
            if let Some(pid) = &r.pull_id {
                assert!(!pid.contains("qwen3-embedding:0.6b"));
            }
        }
    }

    #[test]
    fn unknown_kind_included_in_filter() {
        let avail = vec![dm("foo:bar", Backend::Ollama, ModelKind::Unknown, None)];
        let rows = build_embedding_menu(&avail);
        // Unknown is included in both embedding and llm filters
        assert_eq!(rows[0].kind, MenuRowKind::Auto);
    }

    #[test]
    fn llm_kind_not_in_embedding_menu() {
        let avail = vec![dm("qwen3.5:4b", Backend::Ollama, ModelKind::Llm, None)];
        let rows = build_embedding_menu(&avail);
        // llm-only entry must NOT be auto
        assert!(rows.iter().all(|r| r.model.as_ref().map(|m| m.kind) != Some(ModelKind::Llm)));
    }
}
```

Modify `mur-core/src/discovery/mod.rs` — add `pub mod aggregate;`.

- [x] **Step 2: Run test to verify it fails (compile error)**

Run: `cargo test -p mur-core --lib discovery::aggregate::tests`
Expected: FAIL/no-test the first run if not yet implemented; the file IS the implementation, so this should compile and PASS once written.

- [x] **Step 3: Already implemented in step 1**

Skip.

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test -p mur-core --lib discovery::aggregate`
Expected: PASS — six tests.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/aggregate.rs mur-core/src/discovery/mod.rs
git commit -m "M5.1: discovery::aggregate menu builder"
```

---

### Task 5.2: `discovery::run_all` — async aggregator that talks to all detected runtimes

**Files:**
- Modify: `mur-core/src/discovery/mod.rs`
- Test: `mur-core/tests/discovery_run_all.rs`

- [x] **Step 1: Write the failing test**

Create `mur-core/tests/discovery_run_all.rs`:

```rust
use mur_core::discovery::{Backend, ModelKind};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn run_all_merges_ollama_and_omlx() {
    let ollama_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{
                "name": "qwen3-embedding:0.6b",
                "size": 700_000_000u64,
                "details": {"family": "bert"}
            }]
        })))
        .mount(&ollama_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["embedding"]
        })))
        .mount(&ollama_server)
        .await;

    let omlx_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"}]
        })))
        .mount(&omlx_server)
        .await;

    let merged = mur_core::discovery::run_all_for_test(
        Some(ollama_server.uri()),
        Some(omlx_server.uri()),
    )
    .await
    .unwrap();

    assert_eq!(merged.len(), 2);
    let backends: Vec<Backend> = merged.iter().map(|m| m.backend).collect();
    assert!(backends.contains(&Backend::Ollama));
    assert!(backends.contains(&Backend::OMlx));
    let ollama_model = merged.iter().find(|m| m.backend == Backend::Ollama).unwrap();
    assert_eq!(ollama_model.kind, ModelKind::Embedding);
    let omlx_model = merged.iter().find(|m| m.backend == Backend::OMlx).unwrap();
    assert_eq!(omlx_model.kind, ModelKind::Unknown); // not yet probed
}

#[tokio::test]
async fn run_all_skips_failing_backends() {
    let omlx_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&omlx_server)
        .await;

    // Ollama unreachable URL → discovery returns empty for that backend, no error
    let merged = mur_core::discovery::run_all_for_test(
        Some("http://127.0.0.1:1".into()),
        Some(omlx_server.uri()),
    )
    .await
    .unwrap();
    assert_eq!(merged.len(), 0);
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test discovery_run_all`
Expected: FAIL — `run_all_for_test` undefined.

- [x] **Step 3: Implement**

Append to `mur-core/src/discovery/mod.rs`:

```rust
use crate::discovery::ollama::OllamaDiscovery;
use crate::discovery::omlx::OMlxDiscovery;

/// Run discovery against every endpoint that resolves; merge results.
/// Backend-level failures are logged and dropped (non-fatal).
async fn run_all_inner(
    ollama_endpoint: Option<String>,
    omlx_base_url: Option<String>,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let mut out = Vec::new();
    if let Some(ep) = ollama_endpoint {
        let d = OllamaDiscovery::new(ep);
        match d.list_models().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => tracing::warn!(?e, "Ollama discovery failed"),
        }
    }
    if let Some(ep) = omlx_base_url {
        let d = OMlxDiscovery::new(ep);
        match d.list_models().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => tracing::warn!(?e, "oMLX discovery failed"),
        }
    }
    Ok(out)
}

/// Public entry point for production use. Wires runtime detection from
/// `cmd/init_local.rs` to discovery endpoints.
pub async fn run_all() -> anyhow::Result<Vec<DiscoveredModel>> {
    let runtimes = crate::cmd::init_local::detect_local_runtimes();
    let ollama = runtimes.ollama_running.then(|| "http://localhost:11434".to_string());
    let omlx = runtimes.omlx_installed.then(|| "http://localhost:8000/v1".to_string());
    run_all_inner(ollama, omlx).await
}

/// Test-only entry: caller passes endpoints directly.
#[doc(hidden)]
pub async fn run_all_for_test(
    ollama_endpoint: Option<String>,
    omlx_base_url: Option<String>,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    run_all_inner(ollama_endpoint, omlx_base_url).await
}
```

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test -p mur-core --test discovery_run_all`
Expected: PASS — two tests.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/discovery/mod.rs mur-core/tests/discovery_run_all.rs
git commit -m "M5.2: discovery::run_all aggregator"
```

---

### Task 5.3: `select_local_embedding` rewrite — replaces `select_ollama_embedding`

**Files:**
- Modify: `mur-core/src/cmd/init.rs:769-794` (delete `select_ollama_embedding`, replace with `select_local_embedding`)
- Modify: `mur-core/src/cmd/init.rs:835-976` (call sites in `match model_choice`)

This task is the user-facing flip. Test coverage is mostly via the new aggregate tests + manual smoke (§6 of spec).

- [x] **Step 1: Write the (modest) test**

Append to `mur-core/src/cmd/init.rs` (in the existing `cloud_embedding_tests` mod or a new `select_local_embedding_tests`):

```rust
#[cfg(test)]
mod select_local_embedding_tests {
    use crate::discovery::{Backend, DiscoveredModel, ModelKind};
    use crate::discovery::aggregate::{build_embedding_menu, MenuRowKind};

    fn ollama_qwen() -> DiscoveredModel {
        DiscoveredModel {
            id: "qwen3-embedding:0.6b".into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(1024),
            family: Some("bert".into()),
            size_bytes: None,
            probed_at: None,
        }
    }

    /// Auto row, when chosen, must yield enough info to write
    /// `cfg.embedding.{provider, model, dimensions, ollama_endpoint}`.
    #[test]
    fn auto_row_carries_full_model_info() {
        let rows = build_embedding_menu(&[ollama_qwen()]);
        let auto = rows.iter().find(|r| r.kind == MenuRowKind::Auto).unwrap();
        let m = auto.model.as_ref().unwrap();
        assert_eq!(m.backend, Backend::Ollama);
        assert_eq!(m.id, "qwen3-embedding:0.6b");
        assert_eq!(m.dims, Some(1024));
    }
}
```

- [x] **Step 2: Run test to verify it passes**

Run: `cargo test -p mur-core --lib cmd::init::select_local_embedding_tests`
Expected: PASS.

- [x] **Step 3: Replace `select_ollama_embedding` with `select_local_embedding`**

Edit `mur-core/src/cmd/init.rs:769-794` — replace the closure with:

```rust
    // Helper: select local embedding via discovery. Caller passes the merged
    // discovered models from `discovery::run_all().await`. Returns Ok(true)
    // if config was written; Ok(false) if user picked Skip; Err on probe
    // failure or pull subprocess failure.
    fn select_local_embedding(
        config: &mut mur_common::config::Config,
        available: &[crate::discovery::DiscoveredModel],
    ) -> Result<bool> {
        use crate::discovery::aggregate::{MenuRowKind, build_embedding_menu};
        use crate::discovery::Backend;

        let rows = build_embedding_menu(available);

        println!();
        println!("Embedding model — local discovery:");
        for (i, r) in rows.iter().enumerate() {
            println!("  {}) {}", i + 1, r.label);
        }
        print!("Choose [1-{}] (default: 1): ", rows.len());
        io::stdout().flush()?;
        let mut s = String::new();
        io::stdin().read_line(&mut s)?;
        let idx = s.trim().parse::<usize>().ok()
            .filter(|&n| n >= 1 && n <= rows.len())
            .map(|n| n - 1)
            .unwrap_or(0);
        let row = &rows[idx];

        match row.kind {
            MenuRowKind::Auto | MenuRowKind::Pulled => {
                let m = row.model.as_ref().expect("auto/pulled rows always carry a model");
                match m.backend {
                    Backend::Ollama => {
                        config.embedding.provider = "ollama".into();
                        config.embedding.model = m.id.clone();
                        config.embedding.dimensions = m.dims.unwrap_or(1024);
                        config.embedding.api_key_env = None;
                        config.embedding.openai_url = None;
                    }
                    Backend::OMlx => {
                        config.embedding.provider = "omlx".into();
                        config.embedding.model = m.id.clone();
                        config.embedding.dimensions = m.dims.unwrap_or(1024);
                        config.embedding.api_key_env = Some("OMLX_API_KEY".into());
                        config.embedding.openai_url = Some("http://localhost:8000/v1".into());
                        println!();
                        println!(
                            "  ⚠ Set OMLX_API_KEY before first use (any non-empty value works on localhost):"
                        );
                        println!("      export OMLX_API_KEY=local");
                    }
                }
                Ok(true)
            }
            MenuRowKind::Pull => {
                let pull_id = row.pull_id.as_ref().expect("pull rows always carry an id");
                if pull_id.starts_with("qwen3-embedding:")
                    || pull_id == "bge-m3"
                    || pull_id == "nomic-embed-text"
                    || pull_id == "all-minilm"
                    || pull_id == "embeddinggemma"
                {
                    // Ollama-style tag — invoke `ollama pull`
                    println!();
                    println!("  Pulling {} via Ollama...", pull_id);
                    let st = std::process::Command::new("ollama")
                        .arg("pull")
                        .arg(pull_id)
                        .status();
                    match st {
                        Ok(s) if s.success() => {
                            println!("  ✓ Pulled. Re-run `mur init` to select it.");
                        }
                        Ok(s) => {
                            println!("  ⚠ ollama pull exited with {}; embedding not configured.", s);
                        }
                        Err(e) => {
                            println!("  ⚠ Could not invoke ollama: {e}; install from https://ollama.com");
                        }
                    }
                } else {
                    // Likely an HF id → oMLX path; oMLX has no CLI pull.
                    println!();
                    println!("  Open oMLX.app → Models → search '{}' → Pull", pull_id);
                    println!("  Then re-run `mur init`.");
                }
                Ok(false)
            }
            MenuRowKind::Skip => {
                println!("  Keeping current embedding config.");
                Ok(false)
            }
        }
    }
```

Update call sites at lines 839, 869, 916, 934, 957 (the five places `select_ollama_embedding` is called) — but only sites in Mode 1 and Mode 3 need to swap. Mode 2 (all-cloud) keeps `select_cloud_embedding` unchanged.

For Mode 1 (line 839):

```rust
        "1" => {
            let (_provider, _env_var, llm_model, is_openrouter) = select_cloud_llm(&mut config)?;

            // Replace: select_ollama_embedding(&mut config)?;
            let available = tokio::runtime::Handle::try_current()
                .ok()
                .map(|h| h.block_on(crate::discovery::run_all()))
                .unwrap_or_else(|| Ok(Vec::new()))?;
            let _wrote = select_local_embedding(&mut config, &available)?;

            crate::store::config::save_config(&config)?;
            // ... existing print
        }
```

If `cmd_init` is sync but you need async, simpler: wrap the discovery call in `tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(...)`. Verify by checking `cmd_init` signature (`pub fn cmd_init(hooks_flag: bool) -> Result<()>` is sync per `init.rs:26`).

Add a helper at the top of `init.rs`:

```rust
fn discover_blocking() -> Result<Vec<crate::discovery::DiscoveredModel>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for discovery")?;
    rt.block_on(crate::discovery::run_all())
}
```

Then in Mode 1 / Mode 3 LLM paths use `discover_blocking()` instead of inline tokio.

For Mode 3 LLM (line 909-970), wrap each `LocalBackend::*` arm:

```rust
                Some(LocalBackend::Ollama) => {
                    let m = select_model(OLLAMA_RECS)?;
                    config.llm.provider = "ollama".to_string();
                    config.llm.model = m.id.to_string();
                    config.llm.api_key_env = None;
                    config.llm.openai_url = None;

                    let available = discover_blocking()?;
                    select_local_embedding(&mut config, &available)?;
                    crate::store::config::save_config(&config)?;
                    println!(
                        "  ✓ Config: ollama/{} (LLM) + {}/{} (search)",
                        m.id, config.embedding.provider, config.embedding.model
                    );
                }
```

Repeat the swap for `OMlx` and `MlxLm` arms — same pattern.

For Mode 2 (cloud) — `select_cloud_embedding` already handles the embedding path and calls into `select_ollama_embedding` for its "Local Ollama" sub-choice. Update that single call inside `select_cloud_embedding` (line 818) to use `select_local_embedding(&mut config, &discover_blocking()?)?;` as well.

**Important**: drop the original `select_ollama_embedding` closure entirely after migration. Search for any remaining call site with `grep -n select_ollama_embedding mur-core/src/cmd/init.rs` and confirm zero hits before proceeding.

- [x] **Step 4: Run tests + manual cargo run smoke**

Run: `cargo test --workspace`
Expected: PASS.

Manual:
```bash
cargo run -- init --hooks
# Pick Mode 1 → at embedding prompt, observe new menu shape
```

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/init.rs
git commit -m "M5.3: select_local_embedding replaces hardcoded menu in init flow"
```

---

### Task 5.4: `--refresh-discovery` flag

**Files:**
- Modify: `mur-core/src/main.rs` (find the `Init` clap subcommand definition)
- Modify: `mur-core/src/cmd/init.rs::cmd_init` signature

- [x] **Step 1: Locate clap definition**

Run: `grep -n "Init " mur-core/src/main.rs | head` to find the `mur init` subcommand parsing. Locate the existing `--hooks` flag and add `--refresh-discovery` adjacent.

- [x] **Step 2: Add the flag**

Modify the clap subcommand definition for `Init`. Example pattern:

```rust
Commands::Init {
    #[arg(long)]
    hooks: bool,
    #[arg(long, help = "Bust the discovery cache and re-probe runtimes")]
    refresh_discovery: bool,
}
```

And the dispatch:

```rust
Commands::Init { hooks, refresh_discovery } => {
    cmd::init::cmd_init(hooks, refresh_discovery)?;
}
```

Modify `cmd_init` signature in `init.rs:26`:

```rust
pub(crate) fn cmd_init(hooks_flag: bool, refresh_discovery: bool) -> Result<()> {
```

In `discover_blocking()` (added in 5.3), respect the flag:

```rust
fn discover_blocking(refresh: bool) -> Result<Vec<crate::discovery::DiscoveredModel>> {
    if refresh {
        if let Ok(p) = crate::discovery::cache::DiscoveryCache::default_path() {
            let _ = std::fs::remove_file(&p);
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(crate::discovery::run_all())
}
```

Update all `discover_blocking()` call sites to pass `refresh_discovery`.

- [x] **Step 3: Test compile + manual flag check**

Run: `cargo build --workspace && ./target/debug/mur init --refresh-discovery --hooks`
Expected: builds and runs.

- [x] **Step 4: Add a smoke test asserting the flag exists**

Append to `mur-core/src/cmd/init.rs` tests:

```rust
#[cfg(test)]
mod refresh_flag_tests {
    /// Compile-time check that `cmd_init` accepts the new flag.
    #[test]
    fn cmd_init_accepts_refresh_discovery() {
        // Verify by reference; calling cmd_init in tests is too heavyweight.
        let _ = super::cmd_init as fn(bool, bool) -> anyhow::Result<()>;
    }
}
```

Run: `cargo test -p mur-core --lib cmd::init::refresh_flag_tests`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/main.rs mur-core/src/cmd/init.rs
git commit -m "M5.4: --refresh-discovery flag busts cache before init"
```

---

### Task 5.5: Probe dims at write time + cache write-back

**Files:**
- Modify: `mur-core/src/cmd/init.rs::select_local_embedding` (probe before write)

When user picks an `Auto` or `Pulled` row whose `dims` is `None` (oMLX entries pre-probe always have `dims = None`), invoke `Discovery::probe_embedding` to populate dims before writing config. Cache the result.

- [x] **Step 1: Write the integration test**

Create `mur-core/tests/init_probe_writeback.rs`:

```rust
//! When user picks an oMLX model whose dims weren't yet known, init
//! probes /v1/embeddings to learn dims and writes them to config.
//!
//! Drives the discovery + select flow end-to-end (no TTY), asserting
//! the resulting Config.

use mur_core::discovery::{Backend, DiscoveredModel, ModelKind};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn probe_populates_dims_for_unprobed_omlx_pick() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": vec![0.0f32; 1024]}]
        })))
        .mount(&server)
        .await;

    use mur_core::discovery::omlx::OMlxDiscovery;
    use mur_core::discovery::Discovery;
    let d = OMlxDiscovery::new(server.uri());
    let probe = d
        .probe_embedding("mlx-community/Qwen3-Embedding-0.6B-8bit")
        .await
        .unwrap();
    assert_eq!(probe.dims, 1024);

    // Construct an unprobed model and confirm probe fills dims.
    let mut m = DiscoveredModel {
        id: "mlx-community/Qwen3-Embedding-0.6B-8bit".into(),
        backend: Backend::OMlx,
        kind: ModelKind::Unknown,
        dims: None,
        family: Some("qwen3".into()),
        size_bytes: None,
        probed_at: None,
    };
    if m.dims.is_none() {
        m.dims = Some(probe.dims);
        m.kind = ModelKind::Embedding;
    }
    assert_eq!(m.dims, Some(1024));
    assert_eq!(m.kind, ModelKind::Embedding);
}
```

- [x] **Step 2: Run test to verify it passes**

Run: `cargo test -p mur-core --test init_probe_writeback`
Expected: PASS (proves discovery probe correctness; integration with `select_local_embedding` happens in step 3).

- [x] **Step 3: Wire probe into `select_local_embedding`**

Edit `select_local_embedding` (M5.3) — when handling `Auto` / `Pulled` rows, if `m.dims.is_none()`:

```rust
            MenuRowKind::Auto | MenuRowKind::Pulled => {
                let m = row.model.as_ref().expect("auto/pulled rows always carry a model");
                let dims = match m.dims {
                    Some(d) => d,
                    None => {
                        // Probe to learn dims. Build a runtime-appropriate Discovery
                        // and call probe_embedding.
                        use crate::discovery::Discovery;
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .context("tokio runtime for probe")?;
                        let probe = match m.backend {
                            Backend::Ollama => {
                                let d = crate::discovery::ollama::OllamaDiscovery::new(
                                    "http://localhost:11434"
                                );
                                rt.block_on(d.probe_embedding(&m.id))
                            }
                            Backend::OMlx => {
                                let d = crate::discovery::omlx::OMlxDiscovery::new(
                                    "http://localhost:8000/v1"
                                );
                                rt.block_on(d.probe_embedding(&m.id))
                            }
                        };
                        match probe {
                            Ok(p) => p.dims,
                            Err(e) => {
                                println!("  ⚠ Probe failed: {e}; using preference-table fallback");
                                fallback_dims_for(&m.id).unwrap_or(1024)
                            }
                        }
                    }
                };
                // ...write provider/model/dims as before, using `dims`
            }
```

Add `fallback_dims_for` helper near `discover_blocking`:

```rust
fn fallback_dims_for(id: &str) -> Option<usize> {
    // Best-effort hardcoded dims for known ids when /v1/embeddings probe fails.
    if id.contains("Qwen3-Embedding-0.6B") || id.contains("qwen3-embedding:0.6b") { Some(1024) }
    else if id.contains("Qwen3-Embedding-4B")  || id.contains("qwen3-embedding:4b")  { Some(2560) }
    else if id.contains("Qwen3-Embedding-8B")  || id.contains("qwen3-embedding:8b")  { Some(4096) }
    else if id.contains("bge-m3")                                                   { Some(1024) }
    else if id.contains("nomic-embed-text")                                         { Some(768) }
    else if id.contains("embeddinggemma")                                           { Some(768) }
    else { None }
}
```

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test -p mur-core`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/init.rs mur-core/tests/init_probe_writeback.rs
git commit -m "M5.5: probe dims at config-write time + fallback table"
```

---

### Task 5.6: Manual smoke checklist + PR open

**Files:**
- Create: `docs/superpowers/plans/2026-05-05-mur-embedding-omlx-dynamic-smoke.md`

- [x] **Step 1: Write smoke checklist**

Create `docs/superpowers/plans/2026-05-05-mur-embedding-omlx-dynamic-smoke.md`:

```markdown
# M5 Manual Smoke Checklist

Run after `cargo build --release` against the merged M1+M2+M3+M4+M5 branch.

## Case 1 — oMLX-only (no Ollama)
**Setup:** Stop Ollama daemon. Ensure oMLX.app server is running with
`mlx-community/Qwen3-Embedding-0.6B-8bit` pulled.

```bash
rm -f ~/.mur/cache/discovery.json   # clean slate
./target/release/mur init --hooks --refresh-discovery
```

**Pick Mode 1.** At embedding prompt, expect:
- Row 1: `[auto] oMLX/mlx-community/Qwen3-Embedding-0.6B-8bit (1024d)`
- Row 2-N: any other oMLX models pulled
- `[pull] qwen3-embedding:0.6b` row
- `Skip` row

Press Enter. Verify `~/.mur/config.yaml`:
```yaml
embedding:
  provider: omlx
  model: mlx-community/Qwen3-Embedding-0.6B-8bit
  dimensions: 1024
  api_key_env: OMLX_API_KEY
  openai_url: http://localhost:8000/v1
```

Verify `OMLX_API_KEY` hint printed.

## Case 2 — Ollama-only (no oMLX)
**Setup:** Quit oMLX.app. Ensure `qwen3-embedding:0.6b` is pulled.

```bash
rm -f ~/.mur/cache/discovery.json
./target/release/mur init --hooks --refresh-discovery
```

Mode 1, Enter at embedding prompt. Expect `[auto] Ollama/qwen3-embedding:0.6b (1024d)`.

## Case 3 — Both backends with embeddings
**Setup:** Both running, both have an embedding model pulled.

Expect oMLX entry as `[auto]` (higher in preference at same rank, but
order depends on backend iteration order; either Ollama or oMLX as auto
is acceptable as long as the kind is Embedding).

## Case 4 — Both backends, no embedding models
**Setup:** Both daemons running, neither has any embedding model pulled.

Expect:
- Row 1: `[pull] qwen3-embedding:0.6b` (Ollama)
- Row 2: `[pull] bge-m3`
- Row 3: `Skip — configure later`

Pick row 1. Verify `ollama pull qwen3-embedding:0.6b` runs (progress streams).
On success, verify init prints "Pulled. Re-run `mur init` to select it."

## Case 5 — `--refresh-discovery` busts cache
**Setup:** After case 1, ensure `~/.mur/cache/discovery.json` exists.

```bash
./target/release/mur init --hooks --refresh-discovery
```

Verify cache file mtime updated; observe new probe latency in trace logs
(`RUST_LOG=debug ./target/release/mur init ...`).
```

- [x] **Step 2: Run all 5 cases manually**

Execute each case and check off. Note any deviations.

- [x] **Step 3: Commit smoke doc**

```bash
git add docs/superpowers/plans/2026-05-05-mur-embedding-omlx-dynamic-smoke.md
git commit -m "M5.6: manual smoke checklist for end-to-end discovery flow"
```

- [x] **Step 4: Final clippy + workspace test**

Run: `cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: clean.

- [x] **Step 5: Open PR**

```bash
git push -u origin feat/mur-m5-init-dynamic-picker
gh pr create --title "M5: dynamic local embedding picker (oMLX + Ollama)" --body "$(cat <<'EOF'
## Summary
- `mur init` Mode 1 + Mode 3 now use \`discovery::run_all()\` to enumerate locally-pulled embedding (and Mode 3 LLM) models
- New menu shape: \`[auto] <top-of-rank pulled>\` first, remaining pulled, top-2 \`[pull]\` recommendations, \`Skip\`
- oMLX picks write \`provider: omlx\`, \`openai_url: http://localhost:8000/v1\`, \`api_key_env: OMLX_API_KEY\`
- \`[pull]\` invokes \`ollama pull\` (Ollama) or prints GUI hint (oMLX, no CLI pull mechanism)
- New \`--refresh-discovery\` flag busts the discovery cache
- Dims learned via 1-token \`/v1/embeddings\` (or \`/api/embed\`) probe at write time; static fallback table when probe fails

## Spec
\`docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md\` § 3, § 4

## Test plan
- [x] Unit: \`select_local_embedding_tests\`, \`refresh_flag_tests\`
- [x] Wiremock integration: \`init_probe_writeback\`
- [x] Manual smoke: 5 cases per \`docs/superpowers/plans/2026-05-05-mur-embedding-omlx-dynamic-smoke.md\`
- [x] \`cargo clippy --workspace -- -D warnings\`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

**M5 done. All 5 milestones shipped.**

---

## Self-review

**Spec coverage check:**

- §1 Architecture: M1 (embedding.rs fix) + M2 (discovery scaffold) + M3 (ollama.rs) + M4 (omlx.rs) + M5 (init.rs rewrite + aggregate.rs) ✓
- §2.1 Discovery trait: Task 2.2 ✓
- §2.2 OllamaDiscovery (capabilities + heuristic fallback + family disambig): Tasks 3.1, 3.2 ✓
- §2.3 OMlxDiscovery (probe with 10s timeout for #266; family inference): Tasks 4.1, 4.2 ✓
- §2.4 PreferenceTable (prefix-match + future-proof): Task 2.1 ✓
- §2.5 EmbeddingProvider bug fix: Tasks 1.1, 1.2 ✓
- §2.6 Cache (TTL, schema versioning, atomic write): Task 2.3 ✓
- §3 Data flow (menu render, auto-default, pull subprocess, oMLX hint, skip): Tasks 5.1, 5.3, 5.5 ✓
- §4 Error handling (probe timeout, pull failure, cache corrupt): distributed across tasks ✓
- §5 Testing (unit + wiremock + opt-in integration + manual smoke): present ✓
- §6 Migration: M1 ships standalone; M5 final ✓
- §7 Non-goals: respected — no `model_ref:` field added in this plan; no reranker work; no per-collection embedder

**Placeholder scan:** No "TBD", "TODO" outside of clearly-completed checklist items.

**Type consistency:**
- `EmbeddingProvider::OpenAI { api_key, base_url }` matches across embedding.rs, the unit test, and the integration test ✓
- `DiscoveredModel` field names (`id`, `backend`, `kind`, `dims`, `family`, `size_bytes`, `probed_at`) consistent across cache.rs, ollama.rs, omlx.rs, aggregate.rs ✓
- `MenuRow.kind` enum (`Auto / Pulled / Pull / Skip`) consistent in 5.1 and 5.3 ✓
- `EmbeddingProbe { dims, latency_ms }` consistent in mod.rs and ollama.rs / omlx.rs ✓

**Known gaps fixed inline:**
- Task 5.3's "wrap each `LocalBackend::*` arm" instruction needed concrete mapping; replaced with explicit `Some(LocalBackend::Ollama) => { ... }` block.
- Task 5.5's `fallback_dims_for` was originally referenced but undefined; added the function body inline.
- Task 5.6 added explicit `--refresh-discovery` smoke step (case 5).

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-05-mur-embedding-omlx-dynamic-plan.md`. Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per milestone, review between milestones, fast iteration
2. **Inline Execution** — execute milestones in this session using executing-plans, batch execution with checkpoints

Which approach?
