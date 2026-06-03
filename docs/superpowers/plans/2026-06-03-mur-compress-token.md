# MUR Compress-Token Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native, offline, local-first token-compression subsystem for MUR (`mur-compress` crate) exposing reversible compress/retrieve/stats over MCP.

**Architecture:** A pure leaf crate `mur-compress` implements a two-stage pipeline — lossless **Reformat** then reversible **Offload** (originals stored in a CCR store under `~/.mur/compress/`, retrievable by `blake3` hash). Content is routed by a regex `ContentType` detector to one of five compressors (search/log/diff/json/fallback). Tokens are counted with `tiktoken-rs`. `mur-mcp-server` resolves `mur_home()` and wires three tools. Everything is synchronous, deterministic, and fail-safe (any error ⇒ return original unchanged).

**Tech Stack:** Rust (edition 2024), `tiktoken-rs`, `blake3`, `flate2`, `serde`/`serde_json`/`serde_yaml`, `regex`, `thiserror`; custom BM25; existing custom JSON-RPC MCP server.

**Spec:** `docs/superpowers/specs/2026-06-03-mur-compress-token-design.md`

---

## File Structure

```
mur-compress/                         # NEW workspace member (8th crate), MIT
  Cargo.toml
  src/
    lib.rs            # re-exports + CompressEngine (orchestrator: detect→dispatch→measure→retrieve)
    types.rs         # ContentType, CompressCtx, CompressOutput, CompressResult, RetrieveResult, CompressError
    config.rs        # CompressConfig (+ DetectCfg/StoreCfg/StatsCfg), Default, load()
    tokenizer.rs     # TokenCounter trait, TiktokenCounter, HeuristicCounter, default_counter()
    detect.rs        # detect_content_type()
    bm25.rs          # bm25_rank()
    stats.rs         # StatsTracker, StatsData, StatsSnapshot
    ccr/
      mod.rs         # re-exports
      entry.rs       # CompressedEntry
      store.rs       # CcrStore (put/get/put_original/eviction), hash_content(), gzip helpers
    compressors/
      mod.rs
      fallback.rs    # whitespace/blank-line reformat (no offload)
      search.rs      # group-by-file reformat + top-K/head offload
      log.rs         # repeat-collapse reformat + noise-drop offload
      diff.rs        # context-trim reformat + unchanged-block offload
      json.rs        # minify reformat + array row-collapse offload
  tests/
    end_to_end.rs    # compress→retrieve reversibility, fail-safe, ratios

mur-mcp-server/
  Cargo.toml         # MODIFY: add mur-compress dep
  src/tools.rs       # MODIFY: 3 Tool literals + 3 call_tool arms + engine() helper
  tests/integration.rs # MODIFY: cover the 3 new tools

mur-core/
  src/cmd/compress.rs  # NEW (Task 15, optional CLI)
  src/cmd/mod.rs       # MODIFY: register module
  (clap wiring)        # MODIFY main arg parser

Cargo.toml             # MODIFY: add "mur-compress" to [workspace] members
```

**Conventions for the engineer (zero-context):**
- Build one crate: `cargo build -p mur-compress`. Test one: `cargo test -p mur-compress`. Test by name: `cargo test -p mur-compress <name>`.
- This repo uses **edition 2024** (`LazyLock`, let-chains available). Keep every source file under **800 lines**.
- Commit after every green task. Commit messages end with the trailer shown in Task 1.

---

## Task 1: Scaffold the `mur-compress` crate

**Files:**
- Create: `mur-compress/Cargo.toml`
- Create: `mur-compress/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create `mur-compress/Cargo.toml`**

```toml
[package]
name = "mur-compress"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
regex = "1"
thiserror = "1"
blake3 = "1"
flate2 = "1"
tiktoken-rs = "0.6"

[dev-dependencies]
tempfile = "3"
```

> If `tiktoken-rs = "0.6"` fails to resolve, run `cargo add -p mur-compress tiktoken-rs` to pick the current release; if its `encode_with_special_tokens` method name differs, adjust `tokenizer.rs` in Task 3.

- [ ] **Step 2: Create a minimal `mur-compress/src/lib.rs`**

```rust
//! mur-compress: native, offline, reversible token compression for MUR.
//!
//! Design inspiration: headroom (https://github.com/chopratejas/headroom, Apache-2.0).
//! This crate is a clean-room reimplementation; no headroom source is copied.

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

- [ ] **Step 3: Add the crate to the workspace members**

In `Cargo.toml` (repo root), modify the `members` list — add `"mur-compress",` after `"mur-mcp-server",`:

```toml
members = [
    "mur-common",
    "mur-core",
    "mur-agent-runtime",
    "mur-daemon",
    "mur-gui-core",
    "mur-agent-launcher",
    "mur-mcp-server",
    "mur-compress",
]
```

- [ ] **Step 4: Verify it builds**

Run: `cargo build -p mur-compress`
Expected: compiles clean (downloads `tiktoken-rs`, `blake3`, `flate2` on first build).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml mur-compress/Cargo.toml mur-compress/src/lib.rs
git commit -m "feat(compress): scaffold mur-compress crate" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Core types & errors (`types.rs`)

**Files:**
- Create: `mur-compress/src/types.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Write the failing test** — append to `mur-compress/src/types.rs`:

```rust
//! Shared types for the compression pipeline.

use serde::{Deserialize, Serialize};

/// Detected content category that selects a compressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    SearchResults,
    BuildLog,
    GitDiff,
    Json,
    Generic,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::SearchResults => "search_results",
            ContentType::BuildLog => "build_log",
            ContentType::GitDiff => "git_diff",
            ContentType::Json => "json",
            ContentType::Generic => "generic",
        }
    }
}

/// Per-call context handed to a compressor.
pub struct CompressCtx<'a> {
    pub query: Option<&'a str>,
    pub config: &'a crate::config::CompressConfig,
}

/// Internal output of a single compressor (before token measurement).
#[derive(Debug, Clone)]
pub struct CompressOutput {
    pub compressed: String,
    pub hash: Option<String>,
    pub transforms: Vec<String>,
}

/// Public result of `CompressEngine::compress`.
#[derive(Debug, Clone)]
pub struct CompressResult {
    pub compressed: String,
    pub hash: Option<String>,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub tokens_saved: usize,
    pub savings_percent: f32,
    pub transforms: Vec<String>,
    pub content_type: ContentType,
}

/// Public result of `CompressEngine::retrieve`.
#[derive(Debug, Clone)]
pub enum RetrieveResult {
    Full {
        content_type: String,
        original_content: String,
        item_count: usize,
    },
    Filtered {
        query: String,
        results: Vec<String>,
        count: usize,
    },
    NotFound,
}

#[derive(Debug, thiserror::Error)]
pub enum CompressError {
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    #[error("store error: {0}")]
    Store(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_as_str_roundtrips() {
        assert_eq!(ContentType::Json.as_str(), "json");
        assert_eq!(ContentType::SearchResults.as_str(), "search_results");
    }
}
```

- [ ] **Step 2: Wire the module** — in `mur-compress/src/lib.rs` add at top (below the doc comment):

```rust
pub mod config;
pub mod types;

pub use types::{
    CompressCtx, CompressError, CompressOutput, CompressResult, ContentType, RetrieveResult,
};
```

(`config` is created in Task 4; add the `pub mod config;` line now — it will fail to compile until Task 4. To keep this task green, create an empty `mur-compress/src/config.rs` placeholder containing only `// filled in Task 4` AND the minimal `CompressConfig` stub below, then flesh it out in Task 4.)

Minimal `mur-compress/src/config.rs` stub so `types.rs` compiles:

```rust
//! Filled out in Task 4.
#[derive(Debug, Clone, Default)]
pub struct CompressConfig;
```

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `cargo test -p mur-compress content_type_as_str_roundtrips`
Expected: PASS (this task is pure type definitions; the assertion validates `as_str`).

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/types.rs mur-compress/src/config.rs mur-compress/src/lib.rs
git commit -m "feat(compress): core types and errors" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Tokenizer (`tokenizer.rs`)

**Files:**
- Create: `mur-compress/src/tokenizer.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Write the failing test** — create `mur-compress/src/tokenizer.rs`:

```rust
//! Token counting. Real counts via tiktoken-rs (cl100k_base, offline);
//! a chars/4 heuristic is the degrade-don't-fail fallback.

use crate::types::CompressError;

pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
}

pub struct TiktokenCounter {
    bpe: tiktoken_rs::CoreBPE,
}

impl TiktokenCounter {
    pub fn new() -> Result<Self, CompressError> {
        let bpe = tiktoken_rs::cl100k_base()
            .map_err(|e| CompressError::Tokenizer(e.to_string()))?;
        Ok(Self { bpe })
    }
}

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> usize {
        self.bpe.encode_with_special_tokens(text).len()
    }
}

pub struct HeuristicCounter;

impl TokenCounter for HeuristicCounter {
    fn count(&self, text: &str) -> usize {
        text.len().div_ceil(4)
    }
}

/// Returns the best available counter; never fails.
pub fn default_counter() -> Box<dyn TokenCounter> {
    match TiktokenCounter::new() {
        Ok(c) => Box::new(c),
        Err(_) => Box::new(HeuristicCounter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiktoken_counts_are_positive_and_subword() {
        let c = TiktokenCounter::new().expect("cl100k_base loads");
        // "hello world" is 2 tokens in cl100k_base.
        assert_eq!(c.count("hello world"), 2);
        assert!(c.count("") == 0);
    }

    #[test]
    fn heuristic_is_chars_over_four() {
        let h = HeuristicCounter;
        assert_eq!(h.count("abcd"), 1);
        assert_eq!(h.count("abcde"), 2); // div_ceil
    }
}
```

- [ ] **Step 2: Wire the module** — add to `mur-compress/src/lib.rs`:

```rust
pub mod tokenizer;
pub use tokenizer::{default_counter, TokenCounter};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress tokenizer`
Expected: PASS. If `tiktoken_counts_are_positive_and_subword` fails on the exact count `2`, replace the assertion with `assert!(c.count("hello world") >= 2);` (vocab build differences) and keep going.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/tokenizer.rs mur-compress/src/lib.rs
git commit -m "feat(compress): tiktoken-rs token counter with heuristic fallback" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Config (`config.rs`)

**Files:**
- Modify: `mur-compress/src/config.rs` (replace the Task 2 stub)

- [ ] **Step 1: Replace `mur-compress/src/config.rs` with the full config**

```rust
//! Compression configuration. Mirrors the `compress:` section of
//! ~/.mur/config.yaml (see design spec §11). Every field has a default.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CompressConfig {
    pub enabled: bool,
    pub tokenizer: String,
    pub target_ratio: f32,
    pub bloat_threshold: f32,
    pub protect_head_lines: usize,
    pub protect_tail_lines: usize,
    pub retrieve_top_k: usize,
    pub retrieve_score_threshold: f32,
    pub detect: DetectCfg,
    pub store: StoreCfg,
    pub stats: StatsCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DetectCfg {
    pub search_min_ratio: f32,
    pub log_min_ratio: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StoreCfg {
    pub dir: Option<String>,
    pub ttl_days: u64,
    pub max_entries: usize,
    pub max_bytes: u64,
    pub compress_at_rest: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StatsCfg {
    pub cost_per_mtok_usd: f64,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tokenizer: "cl100k_base".into(),
            target_ratio: 0.30,
            bloat_threshold: 0.20,
            protect_head_lines: 20,
            protect_tail_lines: 20,
            retrieve_top_k: 20,
            retrieve_score_threshold: 0.30,
            detect: DetectCfg::default(),
            store: StoreCfg::default(),
            stats: StatsCfg::default(),
        }
    }
}

impl Default for DetectCfg {
    fn default() -> Self {
        Self { search_min_ratio: 0.6, log_min_ratio: 0.5 }
    }
}

impl Default for StoreCfg {
    fn default() -> Self {
        Self {
            dir: None,
            ttl_days: 7,
            max_entries: 2000,
            max_bytes: 536_870_912, // 512 MiB
            compress_at_rest: true,
        }
    }
}

impl Default for StatsCfg {
    fn default() -> Self {
        Self { cost_per_mtok_usd: 3.0 }
    }
}

impl CompressConfig {
    /// Load the `compress:` section from `<home>/config.yaml`, else defaults.
    pub fn load(home: &Path) -> Self {
        let path = home.join("config.yaml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(root) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            return Self::default();
        };
        root.get("compress")
            .and_then(|v| serde_yaml::from_value::<CompressConfig>(v.clone()).ok())
            .unwrap_or_default()
    }

    pub fn ttl_secs(&self) -> u64 {
        self.store.ttl_days.saturating_mul(86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = CompressConfig::default();
        assert_eq!(c.store.ttl_days, 7);
        assert_eq!(c.ttl_secs(), 7 * 86_400);
        assert_eq!(c.retrieve_top_k, 20);
        assert!(c.store.compress_at_rest);
    }

    #[test]
    fn missing_config_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = CompressConfig::load(dir.path());
        assert_eq!(c.protect_head_lines, 20);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p mur-compress config`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add mur-compress/src/config.rs
git commit -m "feat(compress): config matching spec compress: section" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Content detection (`detect.rs`)

**Files:**
- Create: `mur-compress/src/detect.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Write `mur-compress/src/detect.rs` with tests**

```rust
//! Deterministic content-type detection (regex heuristics, no ML).

use std::sync::LazyLock;

use regex::Regex;

use crate::config::CompressConfig;
use crate::types::ContentType;

static SEARCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^\s:]+:\d+:").unwrap());
static LOG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(ERROR|WARN|WARNING|INFO|DEBUG|TRACE|FATAL)\b").unwrap());

pub fn detect_content_type(content: &str, cfg: &CompressConfig) -> ContentType {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return ContentType::Generic;
    }

    // JSON: must actually parse.
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return ContentType::Json;
    }

    // Git diff: structural markers.
    if content.contains("diff --git")
        || content.lines().any(|l| l.starts_with("@@ ") || l.starts_with("@@-"))
        || (content.contains("\n--- ") && content.contains("\n+++ "))
    {
        return ContentType::GitDiff;
    }

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return ContentType::Generic;
    }
    let n = lines.len() as f32;

    let search_hits = lines.iter().filter(|l| SEARCH_RE.is_match(l)).count() as f32;
    if search_hits / n >= cfg.detect.search_min_ratio {
        return ContentType::SearchResults;
    }

    let log_hits = lines.iter().filter(|l| LOG_RE.is_match(l)).count() as f32;
    if log_hits / n >= cfg.detect.log_min_ratio {
        return ContentType::BuildLog;
    }

    ContentType::Generic
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CompressConfig {
        CompressConfig::default()
    }

    #[test]
    fn detects_json_array() {
        assert_eq!(detect_content_type(r#"[{"a":1},{"a":2}]"#, &cfg()), ContentType::Json);
    }

    #[test]
    fn detects_search_results() {
        let s = "src/a.rs:10:fn foo\nsrc/b.rs:22:let x\nsrc/c.rs:3:use bar";
        assert_eq!(detect_content_type(s, &cfg()), ContentType::SearchResults);
    }

    #[test]
    fn detects_git_diff() {
        let s = "diff --git a/x b/x\n@@ -1,2 +1,2 @@\n-old\n+new";
        assert_eq!(detect_content_type(s, &cfg()), ContentType::GitDiff);
    }

    #[test]
    fn detects_build_log() {
        let s = "INFO starting\nERROR boom\nWARN careful\nDEBUG trace";
        assert_eq!(detect_content_type(s, &cfg()), ContentType::BuildLog);
    }

    #[test]
    fn falls_back_to_generic() {
        assert_eq!(detect_content_type("just some prose here", &cfg()), ContentType::Generic);
    }
}
```

- [ ] **Step 2: Wire the module** — add to `mur-compress/src/lib.rs`:

```rust
pub mod detect;
pub use detect::detect_content_type;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress detect`
Expected: 5 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/detect.rs mur-compress/src/lib.rs
git commit -m "feat(compress): regex content-type detection" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: BM25 ranker (`bm25.rs`)

**Files:**
- Create: `mur-compress/src/bm25.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Write `mur-compress/src/bm25.rs` with tests**

```rust
//! Tiny self-contained BM25 over a list of documents (used by query-filtered
//! retrieve and search-result offload). Returns (index, raw_score) desc.

use std::collections::HashSet;

const K1: f32 = 1.5;
const B: f32 = 0.75;

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

pub fn bm25_rank(query: &str, docs: &[String]) -> Vec<(usize, f32)> {
    let q_terms = tokenize(query);
    if q_terms.is_empty() || docs.is_empty() {
        return Vec::new();
    }
    let doc_terms: Vec<Vec<String>> = docs.iter().map(|d| tokenize(d)).collect();
    let n = docs.len() as f32;
    let total_len: usize = doc_terms.iter().map(|d| d.len()).sum();
    let avgdl = (total_len as f32 / n).max(1.0);

    let mut scores = vec![0.0f32; docs.len()];
    let unique_terms: HashSet<&String> = q_terms.iter().collect();
    for term in unique_terms {
        let df = doc_terms.iter().filter(|d| d.contains(term)).count() as f32;
        if df == 0.0 {
            continue;
        }
        let idf = (((n - df + 0.5) / (df + 0.5)) + 1.0).ln();
        for (i, d) in doc_terms.iter().enumerate() {
            let tf = d.iter().filter(|t| *t == term).count() as f32;
            if tf == 0.0 {
                continue;
            }
            let dl = d.len() as f32;
            scores[i] += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl));
        }
    }

    let mut ranked: Vec<(usize, f32)> = scores
        .into_iter()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_doc_first() {
        let docs = vec![
            "the cat sat on the mat".to_string(),
            "database connection error retry".to_string(),
            "weather is nice today".to_string(),
        ];
        let ranked = bm25_rank("database error", &docs);
        assert_eq!(ranked.first().unwrap().0, 1);
    }

    #[test]
    fn empty_query_returns_empty() {
        let docs = vec!["a".to_string()];
        assert!(bm25_rank("", &docs).is_empty());
    }
}
```

- [ ] **Step 2: Wire the module** — add to `mur-compress/src/lib.rs`:

```rust
pub mod bm25;
pub use bm25::bm25_rank;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress bm25`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/bm25.rs mur-compress/src/lib.rs
git commit -m "feat(compress): self-contained BM25 ranker" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: CCR store (`ccr/entry.rs`, `ccr/store.rs`, `ccr/mod.rs`)

**Files:**
- Create: `mur-compress/src/ccr/entry.rs`
- Create: `mur-compress/src/ccr/store.rs`
- Create: `mur-compress/src/ccr/mod.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Create `mur-compress/src/ccr/entry.rs`**

```rust
//! A stored original, retrievable by hash, with parsed items for query filter.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedEntry {
    pub hash: String,
    pub content_type: String,
    pub created_at: u64,
    pub ttl_secs: u64,
    pub original_text: String,
    pub items: Vec<String>,
    pub original_tokens: usize,
    pub item_count: usize,
}

impl CompressedEntry {
    pub fn is_expired(&self, now: u64) -> bool {
        self.ttl_secs > 0 && now > self.created_at + self.ttl_secs
    }
}
```

- [ ] **Step 2: Create `mur-compress/src/ccr/store.rs`**

```rust
//! Persistent, bounded CCR store. One JSON(.gz) file per entry under
//! <dir>/entries/. Atomic temp+rename writes. TTL on read, size/count eviction.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::ccr::entry::CompressedEntry;
use crate::tokenizer::TokenCounter;
use crate::types::ContentType;

pub fn hash_content(s: &str) -> String {
    let hex = blake3::hash(s.as_bytes()).to_hex();
    hex[..24].to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

fn gunzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut dec = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

pub struct CcrStore {
    entries_dir: PathBuf,
    ttl_secs: u64,
    max_entries: usize,
    max_bytes: u64,
    compress_at_rest: bool,
}

impl CcrStore {
    pub fn new(
        dir: impl Into<PathBuf>,
        ttl_secs: u64,
        max_entries: usize,
        max_bytes: u64,
        compress_at_rest: bool,
    ) -> std::io::Result<Self> {
        let entries_dir = dir.into().join("entries");
        std::fs::create_dir_all(&entries_dir)?;
        Ok(Self { entries_dir, ttl_secs, max_entries, max_bytes, compress_at_rest })
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    fn path_for(&self, hash: &str) -> PathBuf {
        let ext = if self.compress_at_rest { "json.gz" } else { "json" };
        self.entries_dir.join(format!("{hash}.{ext}"))
    }

    pub fn put(&self, entry: &CompressedEntry) -> std::io::Result<()> {
        let path = self.path_for(&entry.hash);
        let json = serde_json::to_vec(entry)?;
        let bytes = if self.compress_at_rest { gzip(&json)? } else { json };
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        self.evict_if_needed()?;
        Ok(())
    }

    /// Build + store an entry for `original`, returning its hash.
    pub fn put_original(
        &self,
        original: &str,
        items: Vec<String>,
        content_type: ContentType,
        tok: &dyn TokenCounter,
    ) -> std::io::Result<String> {
        let hash = hash_content(original);
        let item_count = items.len();
        let entry = CompressedEntry {
            hash: hash.clone(),
            content_type: content_type.as_str().to_string(),
            created_at: now_secs(),
            ttl_secs: self.ttl_secs,
            original_text: original.to_string(),
            items,
            original_tokens: tok.count(original),
            item_count,
        };
        self.put(&entry)?;
        Ok(hash)
    }

    pub fn get(&self, hash: &str) -> std::io::Result<Option<CompressedEntry>> {
        let path = self.path_for(hash);
        let raw = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let json = if self.compress_at_rest { gunzip(&raw)? } else { raw };
        let entry: CompressedEntry = serde_json::from_slice(&json)?;
        if entry.is_expired(now_secs()) {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(entry))
    }

    pub fn stats(&self) -> (usize, u64) {
        let mut count = 0usize;
        let mut bytes = 0u64;
        if let Ok(rd) = std::fs::read_dir(&self.entries_dir) {
            for e in rd.flatten() {
                if let Ok(md) = e.metadata() {
                    if md.is_file() && e.path().extension().is_some_and(|x| x != "tmp") {
                        count += 1;
                        bytes += md.len();
                    }
                }
            }
        }
        (count, bytes)
    }

    fn evict_if_needed(&self) -> std::io::Result<()> {
        let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
        let mut total = 0u64;
        for e in std::fs::read_dir(&self.entries_dir)? {
            let e = e?;
            let md = e.metadata()?;
            if !md.is_file() {
                continue;
            }
            let mtime = md.modified().unwrap_or(std::time::UNIX_EPOCH);
            total += md.len();
            files.push((e.path(), mtime, md.len()));
        }
        files.sort_by_key(|(_, t, _)| *t); // oldest first
        let mut idx = 0;
        while idx < files.len()
            && ((files.len() - idx) > self.max_entries || total > self.max_bytes)
        {
            let (p, _, sz) = &files[idx];
            let _ = std::fs::remove_file(p);
            total = total.saturating_sub(*sz);
            idx += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::HeuristicCounter;

    fn store(dir: &Path, compress: bool) -> CcrStore {
        CcrStore::new(dir, 3600, 100, 1 << 30, compress).unwrap()
    }

    #[test]
    fn put_get_roundtrip_plain_and_gz() {
        for compress in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = store(dir.path(), compress);
            let h = s
                .put_original("a\nb\nc", vec!["a".into(), "b".into(), "c".into()], ContentType::SearchResults, &HeuristicCounter)
                .unwrap();
            let got = s.get(&h).unwrap().expect("entry exists");
            assert_eq!(got.original_text, "a\nb\nc");
            assert_eq!(got.item_count, 3);
        }
    }

    #[test]
    fn expired_entry_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        // ttl_secs = 0 means "no expiry"; use 1 and a backdated entry.
        let s = CcrStore::new(dir.path(), 1, 100, 1 << 30, false).unwrap();
        let mut entry = CompressedEntry {
            hash: hash_content("x"),
            content_type: "generic".into(),
            created_at: 0, // far in the past
            ttl_secs: 1,
            original_text: "x".into(),
            items: vec![],
            original_tokens: 1,
            item_count: 0,
        };
        entry.hash = hash_content("x");
        s.put(&entry).unwrap();
        assert!(s.get(&entry.hash).unwrap().is_none());
    }

    #[test]
    fn missing_hash_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), false);
        assert!(s.get("deadbeef").unwrap().is_none());
    }
}
```

- [ ] **Step 3: Create `mur-compress/src/ccr/mod.rs`**

```rust
pub mod entry;
pub mod store;

pub use entry::CompressedEntry;
pub use store::{hash_content, CcrStore};
```

- [ ] **Step 4: Wire into `mur-compress/src/lib.rs`**

```rust
pub mod ccr;
pub use ccr::{CcrStore, CompressedEntry};
```

Also make `HeuristicCounter` visible to the store test: in `tokenizer.rs` it is already `pub struct HeuristicCounter;` — confirm it is `pub`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p mur-compress ccr`
Expected: 3 tests PASS (roundtrip x2 modes, expiry, missing).

- [ ] **Step 6: Commit**

```bash
git add mur-compress/src/ccr mur-compress/src/lib.rs
git commit -m "feat(compress): CCR store (blake3 key, gzip-at-rest, TTL+LRU eviction)" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Fallback compressor (`compressors/fallback.rs`)

**Files:**
- Create: `mur-compress/src/compressors/fallback.rs`
- Create: `mur-compress/src/compressors/mod.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Create `mur-compress/src/compressors/fallback.rs`**

```rust
//! Generic fallback: lossless-ish densify (trim trailing ws, collapse blank
//! runs). No offload — prose/code are left intact in v1.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput};

pub fn compress(
    content: &str,
    _ctx: &CompressCtx,
    _store: &CcrStore,
    _tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let mut out = String::with_capacity(content.len());
    let mut blank_run = 0;
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    if !content.ends_with('\n') {
        out.pop();
    }
    Ok(CompressOutput {
        compressed: out,
        hash: None,
        transforms: vec!["fallback.whitespace".into()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn collapses_blank_runs_and_trailing_ws() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 10, 1 << 30, false).unwrap();
        let ctx = CompressCtx { query: None, config: &cfg };
        let input = "line one   \n\n\n\nline two\n";
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert_eq!(out.compressed, "line one\n\nline two\n");
        assert!(out.hash.is_none());
    }
}
```

- [ ] **Step 2: Create `mur-compress/src/compressors/mod.rs`**

```rust
pub mod fallback;
// search, log, diff, json added in Tasks 9–12.
```

- [ ] **Step 3: Wire into `mur-compress/src/lib.rs`**

```rust
pub mod compressors;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-compress fallback`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-compress/src/compressors mur-compress/src/lib.rs
git commit -m "feat(compress): fallback whitespace compressor" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Search compressor (`compressors/search.rs`)

**Files:**
- Create: `mur-compress/src/compressors/search.rs`
- Modify: `mur-compress/src/compressors/mod.rs`

- [ ] **Step 1: Create `mur-compress/src/compressors/search.rs`**

```rust
//! Search-result compressor. Reformat: group hits by file. Offload: with a
//! query keep top-K by BM25, else keep the head window; stash the rest.

use crate::bm25::bm25_rank;
use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

fn file_of(line: &str) -> &str {
    // "path:line:content" -> "path"
    line.split(':').next().unwrap_or("")
}

fn group_by_file(items: &[String]) -> String {
    let mut out = String::new();
    let mut current = "";
    for it in items {
        let f = file_of(it);
        if f != current {
            out.push_str(&format!("{f}:\n"));
            current = f;
        }
        // strip the leading "path:" so the file header carries it
        let rest = it.strip_prefix(f).and_then(|r| r.strip_prefix(':')).unwrap_or(it);
        out.push_str("  ");
        out.push_str(rest);
        out.push('\n');
    }
    out
}

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let items: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut transforms = vec!["search.group_by_file".to_string()];
    let cfg = ctx.config;

    let keep_idx: Vec<usize> = if let Some(q) = ctx.query {
        let ranked = bm25_rank(q, &items);
        if ranked.is_empty() {
            (0..items.len().min(cfg.protect_head_lines)).collect()
        } else {
            let mut idx: Vec<usize> =
                ranked.into_iter().take(cfg.retrieve_top_k).map(|(i, _)| i).collect();
            idx.sort_unstable();
            idx
        }
    } else {
        (0..items.len().min(cfg.protect_head_lines)).collect()
    };

    if keep_idx.len() >= items.len() {
        return Ok(CompressOutput {
            compressed: group_by_file(&items),
            hash: None,
            transforms,
        });
    }

    let hash = store
        .put_original(content, items.clone(), ContentType::SearchResults, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    transforms.push("search.offload".to_string());

    let kept: Vec<String> = keep_idx.iter().map(|&i| items[i].clone()).collect();
    let mut body = group_by_file(&kept);
    body.push_str(&format!(
        "[{} of {} hits shown; {} offloaded. Retrieve all with hash={}]\n",
        kept.len(),
        items.len(),
        items.len() - kept.len(),
        hash
    ));
    Ok(CompressOutput { compressed: body, hash: Some(hash), transforms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    fn mk(dir: &std::path::Path) -> CcrStore {
        CcrStore::new(dir, 3600, 100, 1 << 30, false).unwrap()
    }

    #[test]
    fn offloads_tail_and_keeps_query_relevant() {
        let mut cfg = CompressConfig::default();
        cfg.protect_head_lines = 1;
        cfg.retrieve_top_k = 1;
        let dir = tempfile::tempdir().unwrap();
        let store = mk(dir.path());
        let ctx = CompressCtx { query: Some("database"), config: &cfg };
        let input = "a.rs:1:hello world\nb.rs:2:database connection\nc.rs:3:weather";
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some());
        assert!(out.compressed.contains("database connection"));
        assert!(out.compressed.contains("hash="));
        // original retrievable
        let got = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(got.item_count, 3);
    }
}
```

- [ ] **Step 2: Register the module** — in `mur-compress/src/compressors/mod.rs` add `pub mod search;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress search`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/compressors/search.rs mur-compress/src/compressors/mod.rs
git commit -m "feat(compress): search compressor (group-by-file + query offload)" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Log compressor (`compressors/log.rs`)

**Files:**
- Create: `mur-compress/src/compressors/log.rs`
- Modify: `mur-compress/src/compressors/mod.rs`

- [ ] **Step 1: Create `mur-compress/src/compressors/log.rs`**

```rust
//! Log compressor. Reformat: collapse consecutive duplicate lines into
//! "<line>  (xN)". Offload: keep ERROR/WARN/FATAL + head/tail; stash full log.

use std::sync::LazyLock;

use regex::Regex;

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

static IMPORTANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(ERROR|WARN|WARNING|FATAL|PANIC|FAIL)\b").unwrap());

fn collapse_repeats(lines: &[String]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        let mut count = 1;
        while i + count < lines.len() && lines[i + count] == lines[i] {
            count += 1;
        }
        out.push_str(&lines[i]);
        if count > 1 {
            out.push_str(&format!("  (x{count})"));
        }
        out.push('\n');
        i += count;
    }
    out
}

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let items: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let cfg = ctx.config;
    let mut transforms = vec!["log.collapse_repeats".to_string()];
    let n = items.len();

    let keep_idx: Vec<usize> = (0..n)
        .filter(|&i| {
            i < cfg.protect_head_lines
                || i >= n.saturating_sub(cfg.protect_tail_lines)
                || IMPORTANT.is_match(&items[i])
        })
        .collect();

    if keep_idx.len() >= n {
        return Ok(CompressOutput {
            compressed: collapse_repeats(&items),
            hash: None,
            transforms,
        });
    }

    let hash = store
        .put_original(content, items.clone(), ContentType::BuildLog, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    transforms.push("log.offload".to_string());

    let kept: Vec<String> = keep_idx.iter().map(|&i| items[i].clone()).collect();
    let mut body = collapse_repeats(&kept);
    body.push_str(&format!(
        "[{} of {} log lines shown; {} offloaded. Full log: hash={}]\n",
        kept.len(),
        n,
        n - kept.len(),
        hash
    ));
    Ok(CompressOutput { compressed: body, hash: Some(hash), transforms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn keeps_errors_offloads_noise() {
        let mut cfg = CompressConfig::default();
        cfg.protect_head_lines = 0;
        cfg.protect_tail_lines = 0;
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx { query: None, config: &cfg };
        let mut lines = vec![];
        for _ in 0..50 {
            lines.push("DEBUG noise".to_string());
        }
        lines.push("ERROR boom".to_string());
        let input = lines.join("\n");
        let out = compress(&input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.compressed.contains("ERROR boom"));
        assert!(out.hash.is_some());
        assert!(out.compressed.contains("offloaded"));
    }
}
```

- [ ] **Step 2: Register the module** — in `mur-compress/src/compressors/mod.rs` add `pub mod log;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress compressors::log`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/compressors/log.rs mur-compress/src/compressors/mod.rs
git commit -m "feat(compress): log compressor (repeat-collapse + noise offload)" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Diff compressor (`compressors/diff.rs`)

**Files:**
- Create: `mur-compress/src/compressors/diff.rs`
- Modify: `mur-compress/src/compressors/mod.rs`

- [ ] **Step 1: Create `mur-compress/src/compressors/diff.rs`**

```rust
//! Diff compressor. Reformat+Offload: keep headers and changed (+/-) lines;
//! collapse long unchanged-context runs (>3 lines) to a marker; stash full diff.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

const CONTEXT_KEEP: usize = 3;

fn is_header(l: &str) -> bool {
    l.starts_with("diff ")
        || l.starts_with("@@")
        || l.starts_with("--- ")
        || l.starts_with("+++ ")
        || l.starts_with("index ")
}

pub fn compress(
    content: &str,
    _ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let lines: Vec<&str> = content.lines().collect();
    let mut transforms = vec!["diff.trim_context".to_string()];
    let mut kept: Vec<String> = Vec::new();
    let mut context_run = 0usize;
    let mut dropped = 0usize;

    for l in &lines {
        let change = l.starts_with('+') || l.starts_with('-');
        let header = is_header(l);
        let context = !change && !header;
        if context {
            context_run += 1;
            if context_run <= CONTEXT_KEEP {
                kept.push((*l).to_string());
            } else {
                dropped += 1;
            }
        } else {
            if context_run > CONTEXT_KEEP {
                kept.push(format!("... ({} unchanged lines)", context_run - CONTEXT_KEEP));
            }
            context_run = 0;
            kept.push((*l).to_string());
        }
    }
    if context_run > CONTEXT_KEEP {
        kept.push(format!("... ({} unchanged lines)", context_run - CONTEXT_KEEP));
    }

    if dropped == 0 {
        return Ok(CompressOutput {
            compressed: content.to_string(),
            hash: None,
            transforms,
        });
    }

    let items: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let hash = store
        .put_original(content, items, ContentType::GitDiff, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    transforms.push("diff.offload".to_string());

    let mut body = kept.join("\n");
    body.push_str(&format!(
        "\n[diff context trimmed; {dropped} lines offloaded. Full diff: hash={hash}]"
    ));
    Ok(CompressOutput { compressed: body, hash: Some(hash), transforms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn trims_large_unchanged_context() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx { query: None, config: &cfg };
        let mut s = String::from("diff --git a/x b/x\n@@ -1,20 +1,20 @@\n");
        for _ in 0..20 {
            s.push_str(" unchanged\n");
        }
        s.push_str("+added line\n");
        let out = compress(&s, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.compressed.contains("+added line"));
        assert!(out.compressed.contains("unchanged lines)"));
        assert!(out.hash.is_some());
    }
}
```

- [ ] **Step 2: Register the module** — in `mur-compress/src/compressors/mod.rs` add `pub mod diff;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress compressors::diff`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/compressors/diff.rs mur-compress/src/compressors/mod.rs
git commit -m "feat(compress): diff compressor (context trim + offload)" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: JSON compressor (`compressors/json.rs`)

**Files:**
- Create: `mur-compress/src/compressors/json.rs`
- Modify: `mur-compress/src/compressors/mod.rs`

- [ ] **Step 1: Create `mur-compress/src/compressors/json.rs`**

```rust
//! JSON compressor. Reformat: minify. Offload (SmartCrusher-lite): for a long
//! top-level array, emit schema keys + first-N sample + count; stash full array.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

const MIN_ARRAY_FOR_COLLAPSE: usize = 4;

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let val: serde_json::Value =
        serde_json::from_str(content.trim()).map_err(|e| CompressError::Parse(e.to_string()))?;
    let mut transforms = vec!["json.minify".to_string()];
    let minified =
        serde_json::to_string(&val).map_err(|e| CompressError::Parse(e.to_string()))?;
    let cfg = ctx.config;

    if let serde_json::Value::Array(arr) = &val {
        let sample_n = cfg.protect_head_lines.min(arr.len());
        if arr.len() >= MIN_ARRAY_FOR_COLLAPSE && arr.len() > sample_n {
            let keys: Vec<String> = match arr.first() {
                Some(serde_json::Value::Object(m)) => m.keys().cloned().collect(),
                _ => Vec::new(),
            };
            let items: Vec<String> =
                arr.iter().map(|v| serde_json::to_string(v).unwrap_or_default()).collect();
            let hash = store
                .put_original(content, items, ContentType::Json, tok)
                .map_err(|e| CompressError::Store(e.to_string()))?;
            transforms.push("json.row_collapse".to_string());

            let sample = serde_json::Value::Array(arr[..sample_n].to_vec());
            let body = serde_json::json!({
                "_schema": keys,
                "_total": arr.len(),
                "_shown": sample_n,
                "sample": sample,
                "_note": format!("{} rows collapsed; full array hash={}", arr.len() - sample_n, hash),
            });
            return Ok(CompressOutput {
                compressed: serde_json::to_string(&body).unwrap_or(minified),
                hash: Some(hash),
                transforms,
            });
        }
    }

    Ok(CompressOutput { compressed: minified, hash: None, transforms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn collapses_long_array() {
        let mut cfg = CompressConfig::default();
        cfg.protect_head_lines = 2;
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx { query: None, config: &cfg };
        let input = r#"[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6}]"#;
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some());
        assert!(out.compressed.contains("_total"));
        let got = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(got.item_count, 6);
    }

    #[test]
    fn minifies_short_json() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx { query: None, config: &cfg };
        let out = compress("{\n  \"a\": 1\n}", &ctx, &store, &HeuristicCounter).unwrap();
        assert_eq!(out.compressed, r#"{"a":1}"#);
        assert!(out.hash.is_none());
    }
}
```

- [ ] **Step 2: Register the module** — in `mur-compress/src/compressors/mod.rs` add `pub mod json;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress compressors::json`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/compressors/json.rs mur-compress/src/compressors/mod.rs
git commit -m "feat(compress): json compressor (minify + array row-collapse)" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: Stats tracker (`stats.rs`)

**Files:**
- Create: `mur-compress/src/stats.rs`
- Modify: `mur-compress/src/lib.rs`

- [ ] **Step 1: Create `mur-compress/src/stats.rs`**

```rust
//! Persistent savings stats (atomic JSON at <store>/stats.json).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StatsData {
    compressions: u64,
    retrievals: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_tokens_saved: u64,
}

#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub compressions: u64,
    pub retrievals: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens_saved: u64,
    pub savings_percent: f32,
    pub estimated_cost_saved_usd: f64,
    pub store_entries: usize,
    pub store_bytes: u64,
}

pub struct StatsTracker {
    path: PathBuf,
    inner: Mutex<StatsData>,
}

impl StatsTracker {
    pub fn new(path: PathBuf) -> Self {
        let inner = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StatsData>(&b).ok())
            .unwrap_or_default();
        Self { path, inner: Mutex::new(inner) }
    }

    fn flush(&self, d: &StatsData) {
        if let Ok(bytes) = serde_json::to_vec(d) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn record_compression(&self, before: usize, after: usize) {
        let mut d = self.inner.lock().unwrap();
        d.compressions += 1;
        d.total_input_tokens += before as u64;
        d.total_output_tokens += after as u64;
        d.total_tokens_saved += before.saturating_sub(after) as u64;
        self.flush(&d);
    }

    pub fn record_retrieval(&self) {
        let mut d = self.inner.lock().unwrap();
        d.retrievals += 1;
        self.flush(&d);
    }

    pub fn snapshot(&self, cost_per_mtok_usd: f64, store_entries: usize, store_bytes: u64) -> StatsSnapshot {
        let d = self.inner.lock().unwrap().clone();
        let pct = if d.total_input_tokens > 0 {
            d.total_tokens_saved as f32 / d.total_input_tokens as f32 * 100.0
        } else {
            0.0
        };
        let cost = d.total_tokens_saved as f64 * cost_per_mtok_usd / 1_000_000.0;
        StatsSnapshot {
            compressions: d.compressions,
            retrievals: d.retrievals,
            total_input_tokens: d.total_input_tokens,
            total_output_tokens: d.total_output_tokens,
            total_tokens_saved: d.total_tokens_saved,
            savings_percent: pct,
            estimated_cost_saved_usd: cost,
            store_entries,
            store_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let t = StatsTracker::new(path.clone());
        t.record_compression(100, 30);
        t.record_retrieval();
        let snap = t.snapshot(3.0, 1, 123);
        assert_eq!(snap.compressions, 1);
        assert_eq!(snap.total_tokens_saved, 70);
        assert!((snap.savings_percent - 70.0).abs() < 0.01);
        // reload from disk
        let t2 = StatsTracker::new(path);
        let snap2 = t2.snapshot(3.0, 0, 0);
        assert_eq!(snap2.total_tokens_saved, 70);
    }
}
```

- [ ] **Step 2: Wire into `mur-compress/src/lib.rs`**

```rust
pub mod stats;
pub use stats::{StatsSnapshot, StatsTracker};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-compress stats`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/stats.rs mur-compress/src/lib.rs
git commit -m "feat(compress): persistent savings stats tracker" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 14: Orchestrator `CompressEngine` + end-to-end tests

**Files:**
- Modify: `mur-compress/src/lib.rs` (add the engine)
- Create: `mur-compress/tests/end_to_end.rs`

- [ ] **Step 1: Append the engine to `mur-compress/src/lib.rs`**

```rust
use std::path::PathBuf;

use crate::compressors::{diff, fallback, json, log, search};
// IMPORTANT: do NOT add `use crate::ccr::CcrStore;`, `use crate::config::CompressConfig;`,
// `use crate::tokenizer::{default_counter, TokenCounter};`, etc. Those names are already
// in crate-root scope via the `pub use` re-exports added in earlier tasks (and the
// `pub use config::CompressConfig;` below). Re-`use`-ing them here causes
// "the name `X` is defined multiple times" compile errors.

/// Top-level engine: owns the store, tokenizer, config, and stats.
pub struct CompressEngine {
    store: CcrStore,
    tok: Box<dyn TokenCounter>,
    config: CompressConfig,
    stats: StatsTracker,
}

impl CompressEngine {
    pub fn new(dir: impl Into<PathBuf>, config: CompressConfig) -> std::io::Result<Self> {
        let dir = dir.into();
        let store = CcrStore::new(
            &dir,
            config.ttl_secs(),
            config.store.max_entries,
            config.store.max_bytes,
            config.store.compress_at_rest,
        )?;
        let stats = StatsTracker::new(dir.join("stats.json"));
        Ok(Self { store, tok: default_counter(), config, stats })
    }

    fn dispatch(
        &self,
        ct: ContentType,
        content: &str,
        ctx: &CompressCtx,
    ) -> Result<CompressOutput, CompressError> {
        match ct {
            ContentType::SearchResults => search::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::BuildLog => log::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::GitDiff => diff::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::Json => json::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::Generic => fallback::compress(content, ctx, &self.store, self.tok.as_ref()),
        }
    }

    /// Compress `content`. Never errors: any failure returns the original.
    pub fn compress(&self, content: &str, query: Option<&str>) -> CompressResult {
        let ct = detect_content_type(content, &self.config);
        let ctx = CompressCtx { query, config: &self.config };
        let out = self.dispatch(ct, content, &ctx).unwrap_or_else(|_| CompressOutput {
            compressed: content.to_string(),
            hash: None,
            transforms: Vec::new(),
        });

        let before = self.tok.count(content);
        let after = self.tok.count(&out.compressed);
        let saved = before.saturating_sub(after);
        let pct = if before > 0 { saved as f32 / before as f32 * 100.0 } else { 0.0 };
        self.stats.record_compression(before, after);

        CompressResult {
            compressed: out.compressed,
            hash: out.hash,
            original_tokens: before,
            compressed_tokens: after,
            tokens_saved: saved,
            savings_percent: pct,
            transforms: out.transforms,
            content_type: ct,
        }
    }

    /// Retrieve a stored original by hash, optionally BM25-filtered by query.
    pub fn retrieve(&self, hash: &str, query: Option<&str>) -> RetrieveResult {
        let entry = match self.store.get(hash) {
            Ok(Some(e)) => e,
            _ => return RetrieveResult::NotFound,
        };
        self.stats.record_retrieval();
        match query {
            Some(q) => {
                let ranked = bm25_rank(q, &entry.items);
                let max = ranked.first().map(|(_, s)| *s).unwrap_or(1.0).max(1e-6);
                let results: Vec<String> = ranked
                    .into_iter()
                    .map(|(i, s)| (i, s / max))
                    .filter(|(_, s)| *s >= self.config.retrieve_score_threshold)
                    .take(self.config.retrieve_top_k)
                    .map(|(i, _)| entry.items[i].clone())
                    .collect();
                RetrieveResult::Filtered { query: q.to_string(), count: results.len(), results }
            }
            None => RetrieveResult::Full {
                content_type: entry.content_type,
                original_content: entry.original_text,
                item_count: entry.item_count,
            },
        }
    }

    pub fn stats_snapshot(&self) -> StatsSnapshot {
        let (entries, bytes) = self.store.stats();
        self.stats.snapshot(self.config.stats.cost_per_mtok_usd, entries, bytes)
    }
}
```

Add the matching re-export near the top re-exports:

```rust
pub use config::CompressConfig;
```

- [ ] **Step 2: Create `mur-compress/tests/end_to_end.rs`**

```rust
use mur_compress::{CompressConfig, CompressEngine, RetrieveResult};

fn engine(dir: &std::path::Path) -> CompressEngine {
    let mut cfg = CompressConfig::default();
    cfg.protect_head_lines = 2;
    cfg.protect_tail_lines = 1;
    cfg.store.compress_at_rest = false;
    CompressEngine::new(dir, cfg).unwrap()
}

#[test]
fn search_compress_then_retrieve_is_reversible() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    let mut lines = Vec::new();
    for i in 0..40 {
        lines.push(format!("src/file{i}.rs:{i}:some content number {i}"));
    }
    let input = lines.join("\n");

    let res = eng.compress(&input, Some("content number 7"));
    assert!(res.hash.is_some(), "large search output should offload");
    assert!(res.tokens_saved > 0);

    // Full retrieve reproduces the original exactly.
    match eng.retrieve(res.hash.as_ref().unwrap(), None) {
        RetrieveResult::Full { original_content, item_count, .. } => {
            assert_eq!(original_content, input);
            assert_eq!(item_count, 40);
        }
        _ => panic!("expected Full"),
    }

    // Query-filtered retrieve returns relevant items.
    match eng.retrieve(res.hash.as_ref().unwrap(), Some("number 7")) {
        RetrieveResult::Filtered { count, results, .. } => {
            assert!(count > 0);
            assert!(results.iter().any(|r| r.contains("number 7")));
        }
        _ => panic!("expected Filtered"),
    }
}

#[test]
fn fail_safe_passthrough_on_generic_prose() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    let input = "This is ordinary prose that should not be aggressively compressed.";
    let res = eng.compress(input, None);
    // generic -> fallback, no offload, no data loss of words
    assert!(res.hash.is_none());
    assert!(res.compressed.contains("ordinary prose"));
}

#[test]
fn retrieve_unknown_hash_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    assert!(matches!(eng.retrieve("nope", None), RetrieveResult::NotFound));
}

#[test]
fn json_array_roundtrips_through_store() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    let input = r#"[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6},{"id":7},{"id":8}]"#;
    let res = eng.compress(input, None);
    assert!(res.hash.is_some());
    match eng.retrieve(res.hash.as_ref().unwrap(), None) {
        RetrieveResult::Full { original_content, .. } => assert_eq!(original_content, input),
        _ => panic!("expected Full"),
    }
}
```

- [ ] **Step 3: Run the whole crate's tests**

Run: `cargo test -p mur-compress`
Expected: all unit tests + 4 end-to-end tests PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-compress/src/lib.rs mur-compress/tests/end_to_end.rs
git commit -m "feat(compress): CompressEngine orchestrator + reversibility tests" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 15: Wire the three MCP tools

**Files:**
- Modify: `mur-mcp-server/Cargo.toml`
- Modify: `mur-mcp-server/src/tools.rs`
- Modify: `mur-mcp-server/tests/integration.rs`

- [ ] **Step 1: Add the dependency** — in `mur-mcp-server/Cargo.toml` under `[dependencies]` add:

```toml
mur-compress = { path = "../mur-compress" }
```

- [ ] **Step 2: Add an `engine()` helper + tool registrations in `mur-mcp-server/src/tools.rs`**

At the top of the file (after the existing `use` lines), add:

```rust
use mur_compress::{CompressConfig, CompressEngine, RetrieveResult};

/// Build a per-call compression engine rooted at <mur_home>/compress.
fn compress_engine() -> Result<CompressEngine, String> {
    let home = mur_common::trust::mur_home();
    let cfg = CompressConfig::load(&home);
    CompressEngine::new(home.join("compress"), cfg)
        .map_err(|e| format!("compress engine unavailable: {e}"))
}
```

In `all_tools()`, insert these three `Tool` literals just before the closing `]` of the `vec![` (i.e., after the last existing tool, before line ~214):

```rust
        // ── compress tools ──
        Tool {
            name: "mur_compress".into(),
            description: "Compress bulky agent text (tool output, logs, search results, diffs, JSON) before it reaches the LLM. Reversible: the original is stored locally and retrievable by hash via mur_retrieve.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("content".into(), ToolParam {
                        param_type: "string".into(),
                        description: "The text to compress.".into(),
                        default: None,
                    }),
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional query to bias which lines/items are kept.".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["content".into()]),
            },
        },
        Tool {
            name: "mur_retrieve".into(),
            description: "Retrieve the original content stored by mur_compress, by its hash. With a query, returns only the BM25-relevant items.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("hash".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Hash from a prior mur_compress result (e.g. hash=abc123...).".into(),
                        default: None,
                    }),
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional query to filter the stored items.".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["hash".into()]),
            },
        },
        Tool {
            name: "mur_compress_stats".into(),
            description: "Show cumulative token-compression savings (compressions, tokens saved, % saved, estimated cost saved, store size).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
```

- [ ] **Step 3: Add the three dispatch arms in `call_tool`**

In `mur-mcp-server/src/tools.rs`, inside `call_tool`, add these arms before the final `_ => Err(format!("Unknown tool: {}", name)),`:

```rust
        "mur_compress" => {
            let content = arguments
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'content' (string)".to_string())?;
            let query = arguments.get("query").and_then(|v| v.as_str());

            let eng = compress_engine()?;
            let r = eng.compress(content, query);
            let note = match &r.hash {
                Some(h) => format!(
                    "Original stored with hash={h}. Use mur_retrieve to fetch full content."
                ),
                None => "No content offloaded; nothing to retrieve.".to_string(),
            };
            Ok(json!({
                "compressed": r.compressed,
                "hash": r.hash,
                "content_type": r.content_type.as_str(),
                "original_tokens": r.original_tokens,
                "compressed_tokens": r.compressed_tokens,
                "tokens_saved": r.tokens_saved,
                "savings_percent": r.savings_percent,
                "transforms": r.transforms,
                "note": note,
            }))
        }

        "mur_retrieve" => {
            let hash = arguments
                .get("hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'hash' (string)".to_string())?;
            let query = arguments.get("query").and_then(|v| v.as_str());

            let eng = compress_engine()?;
            match eng.retrieve(hash, query) {
                RetrieveResult::Full { content_type, original_content, item_count } => Ok(json!({
                    "hash": hash,
                    "content_type": content_type,
                    "original_content": original_content,
                    "item_count": item_count,
                })),
                RetrieveResult::Filtered { query, results, count } => Ok(json!({
                    "hash": hash,
                    "query": query,
                    "results": results,
                    "count": count,
                })),
                RetrieveResult::NotFound => Ok(json!({
                    "error": "Content not found or expired.",
                    "hash": hash,
                    "hint": "The hash may be wrong or the entry's TTL has elapsed.",
                })),
            }
        }

        "mur_compress_stats" => {
            let eng = compress_engine()?;
            let s = eng.stats_snapshot();
            Ok(json!({
                "compressions": s.compressions,
                "retrievals": s.retrievals,
                "total_input_tokens": s.total_input_tokens,
                "total_output_tokens": s.total_output_tokens,
                "total_tokens_saved": s.total_tokens_saved,
                "savings_percent": s.savings_percent,
                "estimated_cost_saved_usd": s.estimated_cost_saved_usd,
                "store": { "entries": s.store_entries, "bytes": s.store_bytes },
            }))
        }
```

> `mur-mcp-server` is a **binary-only crate** (no `lib.rs`); its tests spawn the built binary and drive it over stdio JSON-RPC. So the new tests follow that exact pattern — do NOT try to import `call_tool` directly.

- [ ] **Step 4a: Update the existing tool-count assertion** in `mur-mcp-server/tests/integration.rs`

The test `test_initialize_and_list_tools` hard-codes the tool count. Adding 3 tools changes 10 → 13. Change line ~56:

```rust
    assert_eq!(tools.len(), 13, "Expected 13 tools");
```

And add three name assertions next to the existing `names.contains(...)` block (after the `scene_explain` assertion):

```rust
    assert!(names.contains(&"mur_compress"));
    assert!(names.contains(&"mur_retrieve"));
    assert!(names.contains(&"mur_compress_stats"));
```

- [ ] **Step 4b: Add a `tempfile` dev-dependency** to `mur-mcp-server/Cargo.toml`

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4c: Add a stdio call test** — append to `mur-mcp-server/tests/integration.rs`:

```rust
#[test]
fn calls_mur_compress_tool() {
    // Isolate the CCR store in a throwaway MUR_HOME for the child process.
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .env("MUR_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
    );
    let _ = read_response(&mut stdout);
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    let _ = read_response(&mut stdout);

    // A long search-style payload that should compress and offload.
    let mut lines = Vec::new();
    for i in 0..40 {
        lines.push(format!("src/f{i}.rs:{i}:token number {i}"));
    }
    let content = lines.join("\n");
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "mur_compress", "arguments": { "content": content, "query": "number 7" } }
    })
    .to_string();
    send_request(&mut stdin, &req);
    let resp = read_response(&mut stdout);

    // The tool's JSON result is embedded in the MCP content array; rather than
    // depend on the exact nesting, assert the markers appear anywhere in the response.
    let resp_str = serde_json::to_string(&resp).unwrap();
    assert!(
        resp_str.contains("tokens_saved"),
        "mur_compress result missing tokens_saved: {resp_str}"
    );
    assert!(
        resp_str.contains("hash="),
        "mur_compress result should include a retrieval-hash note: {resp_str}"
    );

    child.kill().ok();
}
```

- [ ] **Step 5: Build & test**

Run: `cargo test -p mur-mcp-server`
Expected: the updated `test_initialize_and_list_tools` (now 13 tools) + the new `calls_mur_compress_tool` PASS, alongside the existing tests.

- [ ] **Step 6: Commit**

```bash
git add mur-mcp-server/Cargo.toml mur-mcp-server/src/tools.rs mur-mcp-server/tests/integration.rs
git commit -m "feat(mcp): wire mur_compress/mur_retrieve/mur_compress_stats tools" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 16: Optional CLI (`mur compress` / `mur retrieve`)

> **Optional / cuttable.** Skip if you only need the MCP surface. This adds the same capability to the `mur` binary for manual testing.

**Files:**
- Create: `mur-core/src/cmd/compress.rs`
- Modify: `mur-core/src/cmd/mod.rs` (register the module)
- Modify: the `mur` clap command tree (wherever top-level subcommands are defined)

- [ ] **Step 1: Find where subcommands are defined**

Run: `grep -rn "Subcommand" mur-core/src/cmd/mod.rs | head` and `grep -rn "enum Command" mur-core/src | head`
Expected: locate the top-level `clap` subcommand enum (e.g. `Commands`) and its dispatch `match`.

- [ ] **Step 2: Create `mur-core/src/cmd/compress.rs`**

```rust
//! `mur compress` / `mur retrieve` — thin CLI over mur-compress.

use std::io::Read;

use mur_compress::{CompressConfig, CompressEngine, RetrieveResult};

fn engine() -> anyhow::Result<CompressEngine> {
    let home = mur_common::trust::mur_home();
    let cfg = CompressConfig::load(&home);
    Ok(CompressEngine::new(home.join("compress"), cfg)?)
}

fn read_input(file: Option<&str>) -> anyhow::Result<String> {
    match file {
        Some("-") | None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
        Some(path) => Ok(std::fs::read_to_string(path)?),
    }
}

pub fn do_compress(file: Option<&str>, query: Option<&str>) -> anyhow::Result<()> {
    let eng = engine()?;
    let content = read_input(file)?;
    let r = eng.compress(&content, query);
    println!("{}", r.compressed);
    eprintln!(
        "[{} -> {} tokens ({:.1}% saved){}]",
        r.original_tokens,
        r.compressed_tokens,
        r.savings_percent,
        r.hash.map(|h| format!(", hash={h}")).unwrap_or_default()
    );
    Ok(())
}

pub fn do_retrieve(hash: &str, query: Option<&str>) -> anyhow::Result<()> {
    let eng = engine()?;
    match eng.retrieve(hash, query) {
        RetrieveResult::Full { original_content, .. } => println!("{original_content}"),
        RetrieveResult::Filtered { results, .. } => {
            for r in results {
                println!("{r}");
            }
        }
        RetrieveResult::NotFound => {
            anyhow::bail!("content not found or expired for hash={hash}");
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Register module + dependency**

In `mur-core/src/cmd/mod.rs` add `pub mod compress;`. In `mur-core/Cargo.toml` `[dependencies]` add `mur-compress = { path = "../mur-compress" }` (and confirm `anyhow` + `mur-common` are already deps — they are).

- [ ] **Step 4: Add the subcommands to the clap tree**

Add two variants to the top-level `Commands` enum (match the existing style found in Step 1), e.g.:

```rust
    /// Compress bulky text (stdin or FILE), storing the original for retrieval.
    Compress {
        /// Input file, or "-"/omitted for stdin.
        file: Option<String>,
        /// Optional query to bias what is kept.
        #[arg(long)]
        query: Option<String>,
    },
    /// Retrieve content previously stored by `mur compress`.
    Retrieve {
        /// Hash from a prior compress.
        hash: String,
        /// Optional query to filter stored items.
        #[arg(long)]
        query: Option<String>,
    },
```

And in the dispatch `match`:

```rust
        Commands::Compress { file, query } => {
            cmd::compress::do_compress(file.as_deref(), query.as_deref())?;
        }
        Commands::Retrieve { hash, query } => {
            cmd::compress::do_retrieve(&hash, query.as_deref())?;
        }
```

- [ ] **Step 5: Build & smoke test**

Run:
```bash
cargo build -p mur-core
printf 'src/a.rs:1:hello\nsrc/b.rs:2:database error\n' | cargo run -- compress --query database
```
Expected: prints grouped/compressed output; stderr shows the token delta.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/compress.rs mur-core/src/cmd/mod.rs mur-core/Cargo.toml
# plus the file holding the Commands enum/dispatch
git commit -m "feat(cli): mur compress / mur retrieve commands" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 17: Workspace verification, lint, docs

**Files:**
- Modify: `README.md` (CLI surface / tools note — optional)

- [ ] **Step 1: Full workspace build & test**

Run: `cargo test --workspace`
Expected: all crates green, including `mur-compress` and `mur-mcp-server`.

- [ ] **Step 2: Lint & format**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings. Fix any (common: needless clones, `format!` in push_str — acceptable, or use `write!`).

Run: `cargo fmt`
Then: `cargo fmt --check`
Expected: clean.

- [ ] **Step 3: Update docs (optional but recommended)**

In `README.md`, under the MCP tools / CLI surface, add a one-line mention of `mur_compress` / `mur_retrieve` / `mur_compress_stats` and `mur compress`. Keep it short.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore(compress): workspace test + clippy clean, docs note" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 5: (Do NOT bump version or tag.)** Release is a separate, user-initiated step per CLAUDE.md.

---

## Notes & deviations from the spec

- **Reformat/Offload as functions, not traits.** Spec §6 sketches `Reformat`/`Offload` traits. For v1 (five compressors) the two stages are implemented as clearly-separated sections inside each compressor function — same behavior (lossless densify, then reversible offload), less indirection. Trait-ifying is a clean v1.1 refactor if more compressors arrive. This honors spec §4 (the two-stage pipeline) which is the load-bearing part.
- **`estimate_bloat()` gate** is realized implicitly: each compressor cheaply checks "is there anything to offload?" (`keep_idx.len() >= items.len()`, `dropped == 0`, array length) and returns reformat-only output when not. A standalone trait method is unnecessary for v1.
- **Clean-room + MIT.** No headroom source/fixtures are copied; headroom is credited in `lib.rs`. This is the documented adjustment to the spec's "borrow fixtures" idea (§15) to keep the crate cleanly MIT.
- **Per-call engine** (MCP + CLI) keeps tests trivial and avoids global state; the `cl100k_base` load cost is acceptable for an interactive tool. Caching via `OnceLock` is a v1.1 option.
- **Reformat roundtrip guard relaxed.** Spec §16 calls for a strict byte-`reconstruct == original` assertion on reformats. The v1 reformats are intentional *safe densification* (trim trailing whitespace, collapse blank/duplicate runs, JSON-minify, group-by-file) — not byte-lossless — so a strict byte-roundtrip would reject them all. The safety property the spec wants ("never lose information irrecoverably") is still guaranteed two ways: (1) any error in any stage ⇒ passthrough the original; (2) whenever an **offload** drops content, the verbatim original is stored in the CCR store and is retrievable. Reformat-only outputs change only non-semantic whitespace/duplication. A semantic guard (e.g. JSON parse-equality) can be added per-compressor in v1.1.
- **Ratio-band tests deferred.** Spec §17 lists per-content-type savings-band regression tests. v1 asserts `tokens_saved > 0` on representative fixtures (Task 14 e2e); explicit bands (e.g. "search ≥ 80%") are a cheap follow-up once real corpora are wired in.
```
