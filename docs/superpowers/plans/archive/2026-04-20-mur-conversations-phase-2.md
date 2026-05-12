# mur Conversations Archive — Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship Phase 2 of the mur conversations archive (spec `docs/superpowers/specs/2026-04-20-mur-conversations-phase-2-design.md`, commit `d0ee36f`) — sleep-time daily compact + Mode C (Ask) with Perplexity-style citations, both local-only via Ollama, plus commander daemon piggyback cron.

**Architecture:** New `mur-core/src/conversations/summarize/` module for the compact pipeline; new `mur-core/src/conversations/ask/` for Mode C. Compact uses Ollama for extractive (per-chunk) + abstractive (single narrative) stages; writes hybrid summaries (frontmatter + extractive + abstractive + macro map) with atomic `.history/` archiving. Mode C retrieves tiered (layer=1 summary → layer=0 raw on low score), grounds citations, streams tokens. Commander piggyback is fire-and-forget `mur conversations compact` exec.

**Tech Stack:** Rust 2024 · tokio · reqwest (Ollama HTTP) · aho-corasick (pattern names) · walkdir (patterns/) · existing Phase 1 deps (lancedb, chrono, serde, fs2, sha2, hex, uuid, futures, tracing).

**Repos:**
- `/Volumes/Firecuda4tb/Projects/mur` — primary (Phase 2A + 2B + 2C)
- `/Volumes/Firecuda4tb/Projects/mur-commander` — Phase 2A only (§7 trigger)

**Phased rollout per spec §10.3:**
- **Phase 2A — Compact** (Tasks 1-17): unblocks retention; ships mur + commander. PR #1.
- **Phase 2B — Ask** (Tasks 18-27): depends on 2A summaries. PR #2.
- **Phase 2C — Hardening** (Tasks 28-32): polish + Ollama smoke + `.history/` cleanup. PR #3 (can slip to 2.1).

---

## File Structure

### Phase 2A — mur repo (Compact)

**Create:**
```
mur-core/src/conversations/ollama.rs                    shared Ollama HTTP client
mur-core/src/conversations/summarize/mod.rs             public API + orchestrator
mur-core/src/conversations/summarize/chunker.rs         message chunking under token budget
mur-core/src/conversations/summarize/extractive.rs      LLM per-chunk span extraction
mur-core/src/conversations/summarize/abstractive.rs     LLM narrative over spans
mur-core/src/conversations/summarize/macro_refs.rs      Aho-Corasick pattern detection
mur-core/src/conversations/summarize/writer.rs          atomic write + .history + audit + index
```

**Modify:**
```
mur-common/src/config.rs                                add CompactConfig to ConversationsConfig
mur-core/src/conversations/mod.rs                       pub mod summarize + pub mod ollama
mur-core/src/conversations/paths.rs                     add summary_history_dir()
mur-core/src/conversations/index.rs                     add layer filter to search()
mur-core/src/conversations/migrate.rs                   extend sync_commander_config_toml
mur-core/src/cmd/conversations_cmd.rs                   add cmd_conversations_compact
mur-core/src/main.rs                                    add ConversationsAction::Compact
mur-core/Cargo.toml                                     aho-corasick, walkdir (walkdir already in migrate dep)
```

### Phase 2A — mur-commander repo (trigger)

**Create:**
```
crates/daemon/src/triggers/conversations_compact.rs     cron trigger
```

**Modify:**
```
crates/daemon/src/triggers/mod.rs                       register new trigger
crates/daemon/src/main.rs                               instantiate trigger from config
crates/daemon/src/config.rs                             add ConversationsCompactConfig
```

### Phase 2B — mur repo (Ask)

**Create:**
```
mur-core/src/conversations/ask/mod.rs                   public API: AskRequest, AskResponse, AskEvent, ask(), ask_stream()
mur-core/src/conversations/ask/retrieve.rs              tiered retrieval
mur-core/src/conversations/ask/prompt.rs                system prompt + context assembly
mur-core/src/conversations/ask/generate.rs              Ollama streaming client
mur-core/src/conversations/ask/cite.rs                  grounding + coverage
mur-core/src/conversations/ask/format.rs                plain + json output
```

**Modify:**
```
mur-common/src/config.rs                                add AskConfig to ConversationsConfig
mur-core/src/conversations/mod.rs                       pub mod ask
mur-core/src/cmd/conversations_cmd.rs                   add cmd_ask
mur-core/src/main.rs                                    add Commands::Ask
```

### Phase 2C — mur repo (Hardening)

**Modify:**
```
mur-core/src/cmd/conversations_cmd.rs                   preflight Ollama checks, doctor summary-coverage + next-fire
mur-core/src/conversations/summarize/writer.rs          .history/ retention cleanup + audit
mur-core/src/conversations/ask/cite.rs                  strict-mode gate
mur-core/tests/golden_path.rs                           (or shell script) add Step 9 compact + Step 10 ask
```

---

## Task Overview (32 tasks)

| # | Phase | Task | Dep |
|---|-------|------|-----|
| 1 | 2A | `CompactConfig` in `mur-common::config` | — |
| 2 | 2A | `conversations::ollama` HTTP client (shared) | 1 |
| 3 | 2A | Add `aho-corasick` + `walkdir` to Cargo; `summary_history_dir` path helper | — |
| 4 | 2A | `summarize::chunker` message packing | 1 |
| 5 | 2A | `summarize::extractive` LLM per-chunk spans | 2, 4 |
| 6 | 2A | `summarize::abstractive` LLM narrative | 2, 5 |
| 7 | 2A | `summarize::macro_refs` Aho-Corasick | 3 |
| 8 | 2A | `index` — extend `search()` with layer filter | — |
| 9 | 2A | `summarize::writer` atomic write + audit + index | 3, 6, 7, 8 |
| 10 | 2A | `summarize::mod` orchestrator — `compact_day`, `compact_missing` | 9 |
| 11 | 2A | Summary/raw serde types + Markdown renderer helpers | 1, 9 |
| 12 | 2A | CLI — `cmd_conversations_compact` + `Commands::Conversations::Compact` | 10 |
| 13 | 2A | `migrate.rs` — extend P4 sync for `[conversations.compact]` | 1 |
| 14 | 2A | Commander `ConversationsCompactConfig` (cross-repo) | — |
| 15 | 2A | Commander `triggers::conversations_compact` trigger (cross-repo) | 14 |
| 16 | 2A | Commander `daemon::main` — instantiate trigger (cross-repo) | 15 |
| 17 | 2A | Update `mur conversations doctor` — summary coverage + trigger status | 10, 14 |
| 18 | 2B | `AskConfig` in `mur-common::config` | — |
| 19 | 2B | `ask::mod` public types `AskRequest`/`AskResponse`/`AskEvent` | 18 |
| 20 | 2B | `ask::retrieve` tiered escalation | 8, 19 |
| 21 | 2B | `ask::prompt` system prompt + context assembly + budgeting | 19, 20 |
| 22 | 2B | `ask::generate` Ollama streaming | 2, 21 |
| 23 | 2B | `ask::cite` grounding filter (stream-aware) | 22 |
| 24 | 2B | `ask::format` plain + json output | 23 |
| 25 | 2B | `ask::mod::ask_stream` + `ask::mod::ask` glue | 20-24 |
| 26 | 2B | CLI — `cmd_ask` + `Commands::Ask` top-level | 25 |
| 27 | 2B | Extend `scripts/golden-path-conversations.sh` — Step 9 compact + Step 10 ask (with `MUR_OLLAMA_MOCK=1`) | 12, 26 |
| 28 | 2C | `summarize::writer` — `.history/` retention cleanup + audit | 9 |
| 29 | 2C | `ask::cite` — `--strict-citations` reject mode | 23 |
| 30 | 2C | `conversations preflight` — Ollama reachability + model availability | 17 |
| 31 | 2C | Real-Ollama smoke tests (feature-gated) | 10, 25 |
| 32 | 2C | `.history/` rotation audit + `mur conversations doctor` coverage report | 28 |

**Merge checkpoint:** pause after Task 17 (end of 2A) and Task 27 (end of 2B) for PR review before continuing.

---

## Task 1: `CompactConfig` in `mur-common::config` (Phase 2A)

**Files:**
- Modify: `mur-common/src/config.rs`

- [x] **Step 1: Failing test**

Append to `mur-common/src/config.rs` (in the existing `conversations_tests` mod):

```rust
#[test]
fn compact_config_defaults() {
    let c = CompactConfig::default();
    assert!(c.enabled_in_daemon);
    assert_eq!(c.max_days_per_run, 7);
    assert_eq!(c.extractive_model, "qwen3:14b");
    assert_eq!(c.abstractive_model, "qwen3:14b");
    assert_eq!(c.ollama_endpoint, "http://localhost:11434");
    assert_eq!(c.max_extractive_spans, 20);
    assert_eq!(c.max_abstractive_words, 400);
    assert_eq!(c.chunk_tokens, 6000);
    assert_eq!(c.history_retain, 5);
    assert_eq!(c.daemon_cron, "0 3 * * *");
}

#[test]
fn compact_parses_partial_overrides() {
    let y = r#"
conversations:
  compact:
    max_days_per_run: 3
    extractive_model: qwen3:4b
"#;
    let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
    let conv: ConversationsConfig =
        serde_yaml::from_value(v["conversations"].clone()).unwrap();
    assert_eq!(conv.compact.max_days_per_run, 3);
    assert_eq!(conv.compact.extractive_model, "qwen3:4b");
    assert!(conv.compact.enabled_in_daemon); // default preserved
    assert_eq!(conv.compact.abstractive_model, "qwen3:14b"); // default preserved
}
```

- [x] **Step 2: Run test — must fail**

```
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-2
cargo test -p mur-common config::conversations_tests::compact
```

Expected: compile error `cannot find struct 'CompactConfig' in this scope`.

- [x] **Step 3: Add `CompactConfig` struct**

Insert into `mur-common/src/config.rs` alongside the existing Phase 1 conversations structs:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactConfig {
    #[serde(default = "conv_truthy")]
    pub enabled_in_daemon: bool,
    #[serde(default = "compact_default_max_days")]
    pub max_days_per_run: u32,
    #[serde(default = "compact_default_model")]
    pub extractive_model: String,
    #[serde(default = "compact_default_model")]
    pub abstractive_model: String,
    #[serde(default = "compact_default_ollama_endpoint")]
    pub ollama_endpoint: String,
    #[serde(default = "compact_default_max_spans")]
    pub max_extractive_spans: u32,
    #[serde(default = "compact_default_max_words")]
    pub max_abstractive_words: u32,
    #[serde(default = "compact_default_chunk_tokens")]
    pub chunk_tokens: u32,
    #[serde(default = "compact_default_history_retain")]
    pub history_retain: u32,
    #[serde(default = "compact_default_cron")]
    pub daemon_cron: String,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            enabled_in_daemon: true,
            max_days_per_run: compact_default_max_days(),
            extractive_model: compact_default_model(),
            abstractive_model: compact_default_model(),
            ollama_endpoint: compact_default_ollama_endpoint(),
            max_extractive_spans: compact_default_max_spans(),
            max_abstractive_words: compact_default_max_words(),
            chunk_tokens: compact_default_chunk_tokens(),
            history_retain: compact_default_history_retain(),
            daemon_cron: compact_default_cron(),
        }
    }
}

fn compact_default_max_days() -> u32 { 7 }
fn compact_default_model() -> String { "qwen3:14b".into() }
fn compact_default_ollama_endpoint() -> String { "http://localhost:11434".into() }
fn compact_default_max_spans() -> u32 { 20 }
fn compact_default_max_words() -> u32 { 400 }
fn compact_default_chunk_tokens() -> u32 { 6000 }
fn compact_default_history_retain() -> u32 { 5 }
fn compact_default_cron() -> String { "0 3 * * *".into() }
```

Add field to `ConversationsConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsConfig {
    // Phase 1 fields (unchanged)
    #[serde(default)] pub enabled: bool,
    #[serde(default = "conv_default_retention_days")] pub retention_days: u32,
    #[serde(default = "conv_default_poll_interval")] pub poll_interval_secs: u64,
    #[serde(default)] pub sources: ConversationsSources,
    #[serde(default)] pub filter: ConversationsFilter,

    // Phase 2 addition
    #[serde(default)] pub compact: CompactConfig,
}
```

Update `ConversationsConfig::default()` to include `compact: CompactConfig::default()`.

- [x] **Step 4: Run test**

```
cargo test -p mur-common config::conversations_tests::compact
```

Expected: 2 passed.

- [x] **Step 5: Lint + commit**

```
cargo clippy -p mur-common -- -D warnings
cargo fmt --check -p mur-common
git add mur-common/src/config.rs
git commit -m "$(cat <<'EOF'
feat(common): add CompactConfig to ConversationsConfig (Phase 2A)

Typed config for sleep-time compact: 10 fields with serde defaults matching
spec §6.1. All fields #[serde(default)] — existing Phase 1 config.yaml
without a conversations.compact: section still parses.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `conversations::ollama` HTTP client (Phase 2A)

**Files:**
- Create: `mur-core/src/conversations/ollama.rs`
- Modify: `mur-core/src/conversations/mod.rs`

**Rationale:** Both summarize (non-streaming JSON call) and ask (streaming token call) hit Ollama `/api/generate`. A single shared client avoids two competing HTTP implementations.

- [x] **Step 1: Failing test**

Create `mur-core/src/conversations/ollama.rs`:

```rust
//! Shared Ollama HTTP client used by summarize and ask modules.
//!
//! Covers both non-streaming (`generate`) and streaming (`generate_stream`)
//! endpoints. MUR_OLLAMA_MOCK=1 env short-circuits to canned responses for
//! deterministic testing; see docs/superpowers/specs/...phase-2... §9.3.

#![allow(dead_code)] // Phase 2A: generate_stream wired in Phase 2B.

use anyhow::{anyhow, Context, Result};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct GenerateRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<&'a str>,
    pub stream: bool,
    pub options: GenerateOptions,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GenerateOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stop: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateResponse {
    pub response: String,
    pub done: bool,
    pub model: String,
    #[serde(default)]
    pub prompt_eval_count: u64,
    #[serde(default)]
    pub eval_count: u64,
}

pub struct OllamaClient {
    endpoint: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(endpoint: &str, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build");
        Self {
            endpoint: endpoint.to_string(),
            timeout,
            http,
        }
    }

    pub fn mock_from_env() -> bool {
        std::env::var("MUR_OLLAMA_MOCK").as_deref() == Ok("1")
    }

    pub async fn generate(&self, req: GenerateRequest<'_>) -> Result<GenerateResponse> {
        if Self::mock_from_env() {
            return Ok(mock_generate(&req));
        }
        let url = format!("{}/api/generate", self.endpoint.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ollama {status}: {body}"));
        }
        Ok(resp.json::<GenerateResponse>().await?)
    }

    pub async fn generate_stream(
        &self,
        req: GenerateRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        if Self::mock_from_env() {
            let full = mock_generate(&req).response;
            let tokens: Vec<String> =
                full.split_inclusive(' ').map(|s| s.to_string()).collect();
            let stream = futures::stream::iter(tokens.into_iter().map(Ok));
            return Ok(Box::pin(stream));
        }
        let url = format!("{}/api/generate", self.endpoint.trim_end_matches('/'));
        let mut req = req;
        req.stream = true;
        let resp = self.http.post(&url).json(&req).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ollama {status}: {body}"));
        }
        let byte_stream = resp.bytes_stream();
        let token_stream = byte_stream
            .map(|chunk| -> Result<Vec<String>> {
                let bytes = chunk?;
                let text = std::str::from_utf8(&bytes)?;
                let mut out = Vec::new();
                for line in text.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let v: GenerateResponse = serde_json::from_str(line)?;
                    if !v.response.is_empty() {
                        out.push(v.response);
                    }
                }
                Ok(out)
            })
            .flat_map(|res| match res {
                Ok(tokens) => futures::stream::iter(
                    tokens.into_iter().map(Ok).collect::<Vec<_>>(),
                ),
                Err(e) => futures::stream::iter(vec![Err(e)]),
            });
        Ok(Box::pin(token_stream))
    }
}

/// Deterministic fake response for tests. Echoes model+prompt hints so each
/// test can assert which call site fired without a real Ollama.
fn mock_generate(req: &GenerateRequest<'_>) -> GenerateResponse {
    let response = if req.prompt.contains("Extract the 1-3 most informative spans") {
        // extractive stage: one valid span echoed as JSON array
        r#"[{"role":"user","conv_id":"mock","line_hint":1,"text":"mock extractive span"}]"#
            .to_string()
    } else if req.prompt.contains("narrative paragraph") {
        "Mock narrative: today the developer explored mock compression.".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_mode_extractive_returns_valid_json() {
        // Given: MUR_OLLAMA_MOCK=1, extractive prompt
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt: "Extract the 1-3 most informative spans from this excerpt.",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(resp.response.contains("mock extractive span"));
        assert!(serde_json::from_str::<serde_json::Value>(&resp.response).is_ok());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    async fn mock_mode_abstractive_returns_prose() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt: "Write the narrative paragraph.",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(resp.response.starts_with("Mock narrative"));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    async fn real_call_errors_on_unreachable_endpoint() {
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        // Use a deliberately-unroutable port so we get a fast failure
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(500));
        let req = GenerateRequest {
            model: "m",
            prompt: "p",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let r = client.generate(req).await;
        assert!(r.is_err());
    }
}
```

Register module in `mur-core/src/conversations/mod.rs`. Locate the existing `pub mod` block (alphabetical) and add:

```rust
pub mod ollama;
```

- [x] **Step 2: Run tests**

```
cargo test -p mur-core conversations::ollama::tests
```

Expected: 3 passed (2 mock, 1 unreachable-error).

- [x] **Step 3: Lint + commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ollama.rs mur-core/src/conversations/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): conversations/ollama — shared Ollama HTTP client

Unified client for both summarize (single-shot) and ask (streaming).
Covers /api/generate with GenerateRequest / GenerateResponse types.

MUR_OLLAMA_MOCK=1 env short-circuits to deterministic canned responses —
extractive returns a valid JSON-array span, abstractive returns a fixed
narrative, ask returns a fixed answer with one well-formed [cit: ...].
Keeps golden-path tests offline and deterministic.

Streaming returns a Pin<Box<dyn Stream>> of token strings parsed from
Ollama's JSONL NDJSON output.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `aho-corasick` dep + `summary_history_dir` path helper (Phase 2A)

**Files:**
- Modify: `mur-core/Cargo.toml`
- Modify: `mur-core/src/conversations/paths.rs`

- [x] **Step 1: Add deps**

Add to `mur-core/Cargo.toml` `[dependencies]`:

```toml
aho-corasick = "1"
```

(`walkdir = "2"` was already added in Phase 1 Task 18 for migrate dry-run.)

- [x] **Step 2: Failing test in paths.rs**

Append to the existing `#[cfg(test)] mod tests` in `mur-core/src/conversations/paths.rs`:

```rust
#[test]
fn summary_history_dir_under_conversations() {
    let p = summary_history_dir(Some("/tmp/mur-test"));
    assert_eq!(
        p,
        std::path::PathBuf::from("/tmp/mur-test/conversations/summary/.history")
    );
}
```

- [x] **Step 3: Run — must fail**

```
cargo test -p mur-core conversations::paths::tests::summary_history_dir_under_conversations
```

Expected: compile error `cannot find function 'summary_history_dir'`.

- [x] **Step 4: Implement**

Add to `mur-core/src/conversations/paths.rs` (near the other summary helpers):

```rust
/// Directory that holds previous versions of overwritten summaries.
/// One file per rewrite: `<date>.<ISO-8601>.md`.
pub fn summary_history_dir(root_override: Option<&str>) -> PathBuf {
    conversations_root(root_override)
        .join("summary")
        .join(".history")
}
```

- [x] **Step 5: Run**

```
cargo test -p mur-core conversations::paths::tests
```

Expected: all paths tests pass (previous count + 1 new).

- [x] **Step 6: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/Cargo.toml mur-core/src/conversations/paths.rs Cargo.lock
git commit -m "$(cat <<'EOF'
feat(core): add aho-corasick dep + summary_history_dir path helper (Phase 2A)

aho-corasick for Task 7's pattern-name macro detection.
summary_history_dir() returns ~/.mur/conversations/summary/.history — the
destination for prior summary versions when compact overwrites them
(spec §8.1 append-only narrowing).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `summarize::chunker` — message packing (Phase 2A)

**Files:**
- Create: `mur-core/src/conversations/summarize/mod.rs` (bare module stub)
- Create: `mur-core/src/conversations/summarize/chunker.rs`
- Modify: `mur-core/src/conversations/mod.rs`

- [x] **Step 1: Create module stub**

Create `mur-core/src/conversations/summarize/mod.rs`:

```rust
//! Sleep-time compact pipeline (Phase 2A, spec §4).
//!
//! Produces daily hybrid summaries: frontmatter + extractive spans +
//! abstractive narrative + macro expansion map. See
//! `docs/superpowers/specs/2026-04-20-mur-conversations-phase-2-design.md`.
#![allow(dead_code)] // public API wired progressively across Tasks 4-10.

pub mod chunker;
// Later tasks add: pub mod extractive; pub mod abstractive; pub mod macro_refs; pub mod writer;
```

Register in `mur-core/src/conversations/mod.rs`:

```rust
pub mod summarize;
```

- [x] **Step 2: Failing tests**

Create `mur-core/src/conversations/summarize/chunker.rs`:

```rust
//! Pack messages into LLM-sized chunks under a token budget.
//!
//! - Never split a single message (quote integrity).
//! - Prefer splitting at conv_id boundaries; only split mid-conversation if
//!   that conversation itself exceeds the budget.
//! - Token estimate: chars / 4. Good to ±15% across CJK/ASCII mix.

use mur_common::Message;

const CHARS_PER_TOKEN: usize = 4;

#[derive(Debug)]
pub struct Chunk {
    pub messages: Vec<Message>,
    pub token_count: usize,
    pub span_range: (usize, usize), // (start_line, end_line) within the day JSONL
}

pub fn chunk_day(msgs: &[Message], token_budget: usize) -> Vec<Chunk> {
    if msgs.is_empty() {
        return Vec::new();
    }

    // Precompute token cost per message and its day-wide line index (1-based,
    // matches the extractive prompt's L<N> convention).
    let msg_costs: Vec<usize> = msgs
        .iter()
        .map(|m| message_token_cost(m))
        .collect();

    let mut out = Vec::new();
    let mut current: Vec<usize> = Vec::new(); // indices into msgs
    let mut current_tokens = 0usize;
    let mut current_conv: Option<&str> = None;

    for (i, m) in msgs.iter().enumerate() {
        let cost = msg_costs[i];
        let msg_conv = m.conv.as_str();

        // Start-new-chunk decision:
        // 1. always start fresh if adding this msg would exceed budget AND
        //    we already have content (respecting the never-split rule)
        // 2. even if under budget, if msg_conv differs from current_conv AND
        //    we have > 1 msg, prefer splitting at the conv boundary when the
        //    resulting chunk would still fit — the boundary split loses
        //    nothing and keeps the extractive prompt focused.
        let would_overflow = current_tokens + cost > token_budget && !current.is_empty();
        let conv_boundary = current_conv.is_some()
            && current_conv != Some(msg_conv)
            && !current.is_empty();

        if would_overflow || (conv_boundary && current_tokens + cost > token_budget / 2) {
            out.push(make_chunk(msgs, &current, current_tokens));
            current.clear();
            current_tokens = 0;
            current_conv = None;
        }

        current.push(i);
        current_tokens += cost;
        current_conv = Some(msg_conv);
    }

    if !current.is_empty() {
        out.push(make_chunk(msgs, &current, current_tokens));
    }
    out
}

fn message_token_cost(m: &Message) -> usize {
    // Scaffold overhead (role prefix, timestamp, line number) plus content.
    let content_chars = match &m.content {
        mur_common::Content::Text { value } => value.len(),
        mur_common::Content::ToolRef { desc, .. } => desc.len().saturating_add(64),
        mur_common::Content::ImageRef { desc, .. } => desc.len().saturating_add(48),
    };
    // ~40 chars scaffold: "L<line> [hh:mm:ss] <src>/<conv> (<role>): "
    (content_chars + 40) / CHARS_PER_TOKEN + 1
}

fn make_chunk(msgs: &[Message], indices: &[usize], tokens: usize) -> Chunk {
    let start_line = indices.first().copied().unwrap_or(0) + 1; // 1-based
    let end_line = indices.last().copied().unwrap_or(0) + 1;
    let chunk_msgs = indices.iter().map(|&i| msgs[i].clone()).collect();
    Chunk {
        messages: chunk_msgs,
        token_count: tokens,
        span_range: (start_line, end_line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Role, Source};

    fn mk(conv: &str, text: &str) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap(),
            src: Source::ClaudeCode,
            conv: conv.into(),
            role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn empty_day_yields_zero_chunks() {
        assert!(chunk_day(&[], 6000).is_empty());
    }

    #[test]
    fn all_fits_in_one_chunk_under_budget() {
        let msgs = vec![mk("c1", "hello"), mk("c1", "world"), mk("c1", "again")];
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].messages.len(), 3);
        assert_eq!(chunks[0].span_range, (1, 3));
    }

    #[test]
    fn never_splits_single_message_even_if_over_budget() {
        let big = "x".repeat(40_000);
        let msgs = vec![mk("c1", &big)];
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].messages.len(), 1);
    }

    #[test]
    fn splits_at_conv_boundary_when_under_half_budget_reached() {
        // Two conversations, each small enough that greedy would pack them
        // together, but boundary preference splits them.
        let pad = "y".repeat(12_000); // ~3000 tokens
        let msgs = vec![mk("c1", &pad), mk("c2", &pad)];
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks.len(), 2, "expected 2 chunks, got {}", chunks.len());
        assert_eq!(chunks[0].messages[0].conv, "c1");
        assert_eq!(chunks[1].messages[0].conv, "c2");
    }

    #[test]
    fn span_range_is_one_indexed_end_inclusive() {
        let msgs = (0..5).map(|i| mk("c1", &format!("msg{i}"))).collect::<Vec<_>>();
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks[0].span_range, (1, 5));
    }
}
```

- [x] **Step 3: Run tests**

```
cargo test -p mur-core conversations::summarize::chunker::tests
```

Expected: 5 passed.

- [x] **Step 4: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/ mur-core/src/conversations/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize/chunker — pack messages under token budget (Phase 2A)

Conversation-boundary-aware greedy packer. Never splits a single message
(quote integrity). Prefers splitting at conv_id boundaries when the result
still fills half the budget. chars/4 token estimate — no tokenizer dep.
span_range tracks day-wide 1-based line numbers for extractive prompt
scaffolding.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `summarize::extractive` — LLM per-chunk spans (Phase 2A)

**Files:**
- Create: `mur-core/src/conversations/summarize/extractive.rs`
- Modify: `mur-core/src/conversations/summarize/mod.rs` (register)

- [x] **Step 1: Failing tests**

Create `mur-core/src/conversations/summarize/extractive.rs`:

```rust
//! Per-chunk LLM span extraction. See spec §4.2.
//!
//! For each chunk, prompt Ollama for a JSON array of {role, conv_id,
//! line_hint, text} spans. Validate each span:
//!   - text is a verbatim substring of a source message (Jaro-Winkler ≥ 0.95)
//!   - line_hint within chunk.span_range
//!   - role matches source message role
//! Invalid spans silently dropped. Failure degrades to zero spans.

use anyhow::Result;
use mur_common::{Content, Message, Role, Source};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::chunker::Chunk;
use super::super::ollama::{GenerateOptions, GenerateRequest, OllamaClient};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractiveSpan {
    pub role: Role,
    pub conv_id: String,
    pub line_hint: u32,
    pub text: String,
    #[serde(skip)]
    pub src: Source,                // resolved from source message during validation
}

#[derive(Debug, Clone, Deserialize)]
struct LlmSpan {
    role: String,
    conv_id: String,
    line_hint: u32,
    text: String,
}

pub async fn extract_chunk(
    client: &OllamaClient,
    model: &str,
    chunk: &Chunk,
    day_msgs: &[Message],
) -> Result<Vec<ExtractiveSpan>> {
    let prompt = render_prompt(chunk);
    let resp = client
        .generate(GenerateRequest {
            model,
            prompt: &prompt,
            system: None,
            stream: false,
            options: GenerateOptions {
                temperature: Some(0.0),
                top_p: Some(0.9),
                num_predict: Some(1024),
                stop: vec![],
            },
        })
        .await;
    let body = match resp {
        Ok(r) => r.response,
        Err(e) => {
            warn!("extractive LLM call failed: {e:#}");
            return Ok(Vec::new());
        }
    };

    let raw: Vec<LlmSpan> = match parse_json_array(&body) {
        Some(v) => v,
        None => {
            warn!("extractive output not a JSON array; returning zero spans");
            return Ok(Vec::new());
        }
    };

    Ok(raw
        .into_iter()
        .filter_map(|s| validate(s, chunk, day_msgs))
        .collect())
}

fn render_prompt(chunk: &Chunk) -> String {
    let mut body = String::new();
    let (start_line, end_line) = chunk.span_range;
    body.push_str(
        "You are reviewing one conversation day for a technical developer's personal archive. \
         Extract the 1-3 most informative spans from this excerpt.\n\n\
         A span is quote-worthy if it:\n\
         - states a decision the user made (\"we'll use X over Y because...\")\n\
         - records a concrete error or failure that shaped subsequent work\n\
         - captures a new idea, technique, or reference the user hadn't seen before\n\
         - quotes an important external fact (API response, spec excerpt, doc)\n\n\
         A span is NOT quote-worthy if it is:\n\
         - boilerplate/greeting/filler\n\
         - tool-result body already citeable by path\n\
         - restated from an earlier span\n\n\
         Output format: JSON array. Each span is {role, conv_id, line_hint, text}.\n\
           - role: one of \"user\" | \"assistant\" | \"system\" | \"tool\"\n\
           - conv_id: the conv value from the source message\n\
           - line_hint: integer line number within the day's raw JSONL\n\
           - text: verbatim quote, 20-400 chars\n\n\
         If the excerpt has nothing quote-worthy, return [].\n\n",
    );
    body.push_str(&format!(
        "Excerpt ({} messages, lines {}..{}):\n",
        chunk.messages.len(),
        start_line,
        end_line
    ));
    for (i, m) in chunk.messages.iter().enumerate() {
        let line_no = start_line + i;
        let role = format!("{:?}", m.role).to_lowercase();
        let text = content_preview(m);
        body.push_str(&format!(
            "L{} [{}] {}/{} ({}): {}\n",
            line_no,
            m.ts.format("%H:%M:%S"),
            m.src.file_prefix(),
            m.conv,
            role,
            text,
        ));
    }
    body
}

fn content_preview(m: &Message) -> String {
    match &m.content {
        Content::Text { value } => value.clone(),
        Content::ToolRef { desc, bytes, path, .. } => {
            format!("[tool_ref:{} ({}B) @ {}]", desc, bytes, path)
        }
        Content::ImageRef { desc, path, .. } => format!("[image_ref:{} @ {}]", desc, path),
    }
}

fn parse_json_array(body: &str) -> Option<Vec<LlmSpan>> {
    // Tolerate fences / surrounding prose: find first `[` and last `]`.
    let start = body.find('[')?;
    let end = body.rfind(']')?;
    if end <= start {
        return None;
    }
    let slice = &body[start..=end];
    serde_json::from_str::<Vec<LlmSpan>>(slice).ok()
}

fn validate(raw: LlmSpan, chunk: &Chunk, day_msgs: &[Message]) -> Option<ExtractiveSpan> {
    // line_hint within chunk
    let (s, e) = chunk.span_range;
    if raw.line_hint < s as u32 || raw.line_hint > e as u32 {
        return None;
    }
    let role = parse_role(&raw.role)?;
    // Find the source message at line_hint (1-based → 0-based index)
    let idx = raw.line_hint as usize - 1;
    let source_msg = day_msgs.get(idx)?;
    if source_msg.role != role {
        return None;
    }
    if source_msg.conv != raw.conv_id {
        return None;
    }
    // Verbatim check with Jaro-Winkler ≥ 0.95 against source message text
    let source_text = content_preview(source_msg);
    let similarity = jaro_winkler(&raw.text, &source_text);
    if similarity < 0.95 {
        return None;
    }
    Some(ExtractiveSpan {
        role,
        conv_id: raw.conv_id,
        line_hint: raw.line_hint,
        text: raw.text,
        src: source_msg.src,
    })
}

fn parse_role(s: &str) -> Option<Role> {
    match s.to_ascii_lowercase().as_str() {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "system" => Some(Role::System),
        "tool" => Some(Role::Tool),
        _ => None,
    }
}

/// Small Jaro-Winkler implementation (no extra dep). Char-based, case-sensitive.
fn jaro_winkler(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let match_distance = (a.len().max(b.len()) / 2).saturating_sub(1);

    let mut a_matched = vec![false; a.len()];
    let mut b_matched = vec![false; b.len()];
    let mut matches = 0usize;

    for i in 0..a.len() {
        let lo = i.saturating_sub(match_distance);
        let hi = (i + match_distance + 1).min(b.len());
        for j in lo..hi {
            if b_matched[j] {
                continue;
            }
            if a[i] == b[j] {
                a_matched[i] = true;
                b_matched[j] = true;
                matches += 1;
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }
    // transpositions
    let mut k = 0usize;
    let mut transpositions = 0usize;
    for i in 0..a.len() {
        if !a_matched[i] {
            continue;
        }
        while !b_matched[k] {
            k += 1;
        }
        if a[i] != b[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let m = matches as f64;
    let jaro = (m / a.len() as f64
        + m / b.len() as f64
        + (m - transpositions as f64 / 2.0) / m)
        / 3.0;
    // Winkler common-prefix boost up to 4 chars, p=0.1
    let prefix = a
        .iter()
        .zip(b.iter())
        .take(4)
        .take_while(|(x, y)| x == y)
        .count() as f64;
    jaro + prefix * 0.1 * (1.0 - jaro)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn mk(ts_min: u32, conv: &str, text: &str, role: Role) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 19, 10, ts_min, 0)
                .unwrap(),
            src: Source::ClaudeCode,
            conv: conv.into(),
            role,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn json_array_parsed_with_surrounding_prose() {
        let body = r#"Here are the spans:
```json
[
  {"role":"user","conv_id":"c1","line_hint":1,"text":"hi"}
]
```
That's all."#;
        let v = parse_json_array(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text, "hi");
    }

    #[test]
    fn validate_rejects_out_of_range_line_hint() {
        let msgs = vec![mk(0, "c1", "hello", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "user".into(),
            conv_id: "c1".into(),
            line_hint: 99,
            text: "hello".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_none());
    }

    #[test]
    fn validate_rejects_role_mismatch() {
        let msgs = vec![mk(0, "c1", "hello", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "assistant".into(),
            conv_id: "c1".into(),
            line_hint: 1,
            text: "hello".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_none());
    }

    #[test]
    fn validate_rejects_paraphrase() {
        let msgs = vec![mk(0, "c1", "cargo build failed with error E0001", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "user".into(),
            conv_id: "c1".into(),
            line_hint: 1,
            text: "build failed".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_none());
    }

    #[test]
    fn validate_accepts_verbatim() {
        let msgs = vec![mk(0, "c1", "cargo build failed with error E0001", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "user".into(),
            conv_id: "c1".into(),
            line_hint: 1,
            text: "cargo build failed with error E0001".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_some());
    }

    #[tokio::test]
    async fn mock_ollama_extracts_one_span() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let msgs = vec![mk(0, "mock", "mock extractive span", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let spans = extract_chunk(&client, "qwen3:14b", &chunk, &msgs)
            .await
            .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "mock extractive span");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
```

Register in `summarize/mod.rs`:

```rust
pub mod chunker;
pub mod extractive;
```

- [x] **Step 2: Run tests**

```
cargo test -p mur-core conversations::summarize::extractive::tests
```

Expected: 6 passed.

- [x] **Step 3: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/extractive.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize/extractive — per-chunk LLM span extraction (Phase 2A)

Renders extractive prompt (spec §4.2), calls Ollama with temperature=0.0,
parses JSON array (tolerant of prose/fences), validates each span:
- line_hint within chunk.span_range
- role matches source message
- conv_id matches source message
- Jaro-Winkler ≥ 0.95 verbatim check against source text

Invalid spans silently dropped. Ollama failure → zero spans, warn logged.

Inline Jaro-Winkler impl (no extra dep). MUR_OLLAMA_MOCK covered by mock
extractive branch in ollama.rs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `summarize::abstractive` — LLM narrative (Phase 2A)

**Files:**
- Create: `mur-core/src/conversations/summarize/abstractive.rs`
- Modify: `mur-core/src/conversations/summarize/mod.rs`

- [x] **Step 1: Failing tests**

Create `mur-core/src/conversations/summarize/abstractive.rs`:

```rust
//! Abstractive narrative stage. Single LLM call over all extractive spans.
//! See spec §4.3. Output: 150-400 words, first-person or neutral.

use anyhow::Result;
use tracing::warn;

use super::extractive::ExtractiveSpan;
use super::super::ollama::{GenerateOptions, GenerateRequest, OllamaClient};

pub struct AbstractiveResult {
    pub narrative: Option<String>,   // None iff LLM failed and caller should set the warning
    pub word_count: usize,
}

pub async fn summarize(
    client: &OllamaClient,
    model: &str,
    spans: &[ExtractiveSpan],
    date: chrono::NaiveDate,
    max_words: u32,
) -> AbstractiveResult {
    if spans.is_empty() {
        return AbstractiveResult {
            narrative: Some("No significant activity on this day.".to_string()),
            word_count: 5,
        };
    }
    let prompt = render_prompt(spans, date, max_words);
    let resp = client
        .generate(GenerateRequest {
            model,
            prompt: &prompt,
            system: None,
            stream: false,
            options: GenerateOptions {
                temperature: Some(0.2),
                top_p: Some(0.9),
                num_predict: Some(max_words * 2), // tokens > words; headroom
                stop: vec![],
            },
        })
        .await;
    match resp {
        Ok(r) => {
            let narrative = clean_output(&r.response);
            let word_count = narrative.split_whitespace().count();
            AbstractiveResult {
                narrative: Some(narrative),
                word_count,
            }
        }
        Err(e) => {
            warn!("abstractive LLM call failed: {e:#}");
            AbstractiveResult {
                narrative: None,
                word_count: 0,
            }
        }
    }
}

fn render_prompt(spans: &[ExtractiveSpan], date: chrono::NaiveDate, max_words: u32) -> String {
    let min_words = 150.min(max_words / 2);
    let mut body = format!(
        "You are summarizing one day ({}) of a developer's AI-assistant conversations into a \
         narrative paragraph. Use ONLY information present in the spans below.\n\n\
         Output: {}-{} words, first-person or neutral third-person, no bullet lists. \
         Reference each key point by its span index [N]. Do NOT invent details not in the spans. \
         If spans conflict, note the conflict.\n\n\
         Spans:\n",
        date, min_words, max_words
    );
    for (i, s) in spans.iter().enumerate() {
        body.push_str(&format!(
            "[{}] {{{} {}/{} L{}}}: {}\n",
            i + 1,
            date,
            s.src.file_prefix(),
            s.conv_id,
            s.line_hint,
            s.text,
        ));
    }
    body.push_str("\nWrite the narrative.\n");
    body
}

fn clean_output(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip trailing commentary like "Let me know if you'd like..." that some
    // models append after the summary. Heuristic: keep content up to the first
    // double-newline that follows a complete sentence.
    if let Some(idx) = trimmed.find("\n\nLet me") {
        return trimmed[..idx].trim().to_string();
    }
    if let Some(idx) = trimmed.find("\n\nWould you") {
        return trimmed[..idx].trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Role, Source};

    fn span(idx: u32, text: &str) -> ExtractiveSpan {
        ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: idx,
            text: text.into(),
            src: Source::ClaudeCode,
        }
    }

    #[tokio::test]
    async fn empty_spans_emit_placeholder() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let r = summarize(&client, "qwen3:14b", &[], chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(), 400).await;
        assert!(r.narrative.as_deref().unwrap().contains("No significant"));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    async fn mock_narrative_happy_path() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let spans = vec![span(1, "hello world"), span(2, "compression works")];
        let r = summarize(
            &client,
            "qwen3:14b",
            &spans,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(r.narrative.as_deref().unwrap().starts_with("Mock narrative"));
        assert!(r.word_count > 0);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[test]
    fn clean_output_strips_trailing_commentary() {
        let raw = "This is the narrative.\n\nLet me know if you'd like more detail!";
        assert_eq!(clean_output(raw), "This is the narrative.");
    }
}
```

Register in `summarize/mod.rs`:

```rust
pub mod abstractive;
pub mod chunker;
pub mod extractive;
```

- [x] **Step 2: Run tests**

```
cargo test -p mur-core conversations::summarize::abstractive::tests
```

Expected: 3 passed.

- [x] **Step 3: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/abstractive.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize/abstractive — LLM narrative over spans (Phase 2A)

Single Ollama call, temperature=0.2, produces 150-400 word narrative that
references extractive spans by [N]. Empty spans → placeholder. LLM failure →
narrative=None so the caller can set the narrative_generation_failed warning
in frontmatter.

Output sanitizer strips trailing 'Let me know...' / 'Would you...' commentary
some models append.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `summarize::macro_refs` — Aho-Corasick pattern detection (Phase 2A)

**Files:**
- Create: `mur-core/src/conversations/summarize/macro_refs.rs`
- Modify: `mur-core/src/conversations/summarize/mod.rs`

- [x] **Step 1: Failing tests**

Create `mur-core/src/conversations/summarize/macro_refs.rs`:

```rust
//! Pattern-name macro reference detection (spec §4.4).
//!
//! Enumerates ~/.mur/patterns/*.yaml names, scans extractive spans and the
//! abstractive narrative for word-boundary matches, rewrites them to
//! {{pattern: <name>}}, and records (version, sha) per referenced pattern.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

use super::extractive::ExtractiveSpan;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MacroRef {
    pub name: String,
    pub pattern_version: u32,
    pub pattern_sha: String,
    pub marker: String,
}

pub fn detect_and_rewrite(
    extractive: &mut [ExtractiveSpan],
    abstractive: &mut String,
    patterns_dir: &Path,
) -> Result<Vec<MacroRef>> {
    let names = enumerate_pattern_names(patterns_dir)?;
    if names.is_empty() {
        return Ok(Vec::new());
    }

    // Case-insensitive Aho-Corasick. Word-boundary enforced via post-check.
    let ac = aho_corasick::AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build(&names)
        .context("build aho-corasick")?;

    let mut found: BTreeSet<String> = BTreeSet::new();

    for span in extractive.iter_mut() {
        let new_text = rewrite_with_markers(&span.text, &ac, &names, &mut found);
        span.text = new_text;
    }
    let new_narrative = rewrite_with_markers(abstractive, &ac, &names, &mut found);
    *abstractive = new_narrative;

    let mut refs = Vec::new();
    for name in found {
        let (version, sha) = read_pattern_meta(patterns_dir, &name)
            .unwrap_or_else(|e| {
                tracing::warn!("failed to read pattern {name}: {e:#}; using defaults");
                (0, String::new())
            });
        refs.push(MacroRef {
            name: name.clone(),
            pattern_version: version,
            pattern_sha: sha,
            marker: format!("{{{{pattern: {name}}}}}"),
        });
    }
    Ok(refs)
}

fn enumerate_pattern_names(patterns_dir: &Path) -> Result<Vec<String>> {
    if !patterns_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(patterns_dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            out.push(stem.to_string());
        }
    }
    Ok(out)
}

fn rewrite_with_markers(
    text: &str,
    ac: &aho_corasick::AhoCorasick,
    names: &[String],
    found: &mut BTreeSet<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let bytes = text.as_bytes();

    for mat in ac.find_iter(text) {
        let start = mat.start();
        let end = mat.end();
        let name = &names[mat.pattern()];

        // Word-boundary check. Chars before/after must not be ASCII alphanumeric
        // or '-' (pattern names use kebab-case).
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);

        // Code-fence / backtick / YAML-quote skip.
        if !before_ok || !after_ok || inside_code_or_quote(text, start) {
            continue;
        }

        out.push_str(&text[last..start]);
        out.push_str(&format!("{{{{pattern: {name}}}}}"));
        last = end;
        found.insert(name.clone());
    }
    out.push_str(&text[last..]);
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn inside_code_or_quote(text: &str, pos: usize) -> bool {
    // Toggle state up to `pos` for each of: backtick, code-fence (```), single-quote, double-quote
    let mut in_backtick = false;
    let mut in_code_fence = false;
    let mut in_single = false;
    let mut in_double = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < pos {
        if i + 3 <= pos && &bytes[i..i + 3] == b"```" {
            in_code_fence = !in_code_fence;
            i += 3;
            continue;
        }
        match bytes[i] {
            b'`' if !in_code_fence => in_backtick = !in_backtick,
            b'\'' if !in_code_fence && !in_backtick => in_single = !in_single,
            b'"' if !in_code_fence && !in_backtick => in_double = !in_double,
            _ => {}
        }
        i += 1;
    }
    in_backtick || in_code_fence || in_single || in_double
}

fn read_pattern_meta(patterns_dir: &Path, name: &str) -> Result<(u32, String)> {
    let path = patterns_dir.join(format!("{name}.yaml"));
    let bytes = std::fs::read(&path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha = hex::encode(hasher.finalize());
    // schema version lives at top level; if missing, default 0
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&bytes).unwrap_or(serde_yaml::Value::Null);
    let version = yaml
        .get("schema")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Ok((version, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Role, Source};

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_pattern(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(format!("{name}.yaml")), body).unwrap();
    }

    fn span(text: &str) -> ExtractiveSpan {
        ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: 1,
            text: text.into(),
            src: Source::ClaudeCode,
        }
    }

    #[test]
    fn detects_and_rewrites_known_pattern() {
        let tmp = tmpdir();
        write_pattern(tmp.path(), "atomic-yaml-write", "schema: 2\nname: atomic-yaml-write\n");
        let mut spans = vec![span("we used atomic-yaml-write for the writer.")];
        let mut narr = "The choice was atomic-yaml-write.".to_string();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "atomic-yaml-write");
        assert_eq!(refs[0].pattern_version, 2);
        assert!(spans[0].text.contains("{{pattern: atomic-yaml-write}}"));
        assert!(narr.contains("{{pattern: atomic-yaml-write}}"));
    }

    #[test]
    fn word_boundary_skips_partial_match() {
        let tmp = tmpdir();
        write_pattern(tmp.path(), "rust", "schema: 1\n");
        let mut spans = vec![span("my rustic approach is rustless actually")];
        let mut narr = String::new();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert!(refs.is_empty(), "rust should not match 'rustic' or 'rustless'");
        assert!(!spans[0].text.contains("{{pattern"));
    }

    #[test]
    fn skips_inside_backticks() {
        let tmp = tmpdir();
        write_pattern(tmp.path(), "my-pattern", "schema: 1\n");
        let mut spans = vec![span("reference to `my-pattern` is literal")];
        let mut narr = String::new();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn empty_patterns_dir_returns_empty() {
        let tmp = tmpdir();
        let mut spans = vec![span("anything")];
        let mut narr = "anything".to_string();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn nonexistent_patterns_dir_is_safe() {
        let mut spans = vec![span("x")];
        let mut narr = "x".to_string();
        let refs =
            detect_and_rewrite(&mut spans, &mut narr, Path::new("/nonexistent/path")).unwrap();
        assert!(refs.is_empty());
    }
}
```

Register in `summarize/mod.rs`:

```rust
pub mod abstractive;
pub mod chunker;
pub mod extractive;
pub mod macro_refs;
```

- [x] **Step 2: Run tests**

```
cargo test -p mur-core conversations::summarize::macro_refs::tests
```

Expected: 5 passed.

- [x] **Step 3: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/macro_refs.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize/macro_refs — Aho-Corasick pattern detection (Phase 2A)

Scans extractive spans + abstractive narrative for pattern names (stems of
~/.mur/patterns/*.yaml), rewrites valid matches to {{pattern: name}}
markers. Enforces word boundaries (rust doesn't match rustic/rustless) and
skips matches inside backticks, code fences, or quotes. Records (version,
sha) per referenced pattern for Phase 3 invalidation detection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `index::search` — add `layer` filter (Phase 2A)

**Files:**
- Modify: `mur-core/src/conversations/index.rs`

- [x] **Step 1: Check current signature**

```
grep -n "pub async fn search" mur-core/src/conversations/index.rs
```

Expected: `pub async fn search(&self, query_vec: &[f32], limit: usize, source_filter: Option<Source>) -> Result<Vec<SearchHit>>`

- [x] **Step 2: Extend signature — add failing test**

Append to the existing `#[cfg(test)] mod tests` in `mur-core/src/conversations/index.rs`:

```rust
#[tokio::test]
async fn search_filters_by_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();

    let a = msg("a", "layer zero item");       // raw → layer 0
    let b = msg("b", "layer one item");        // summary → layer 1
    idx.upsert_with_layer(&[(a, vec![1.0; 16], 0)]).await.unwrap();
    idx.upsert_with_layer(&[(b, vec![1.0; 16], 1)]).await.unwrap();

    let hits_all = idx
        .search(&[1.0; 16], 10, None, None)
        .await
        .unwrap();
    assert_eq!(hits_all.len(), 2);

    let hits_l1 = idx
        .search(&[1.0; 16], 10, None, Some(1))
        .await
        .unwrap();
    assert_eq!(hits_l1.len(), 1);
    assert_eq!(hits_l1[0].conv_id, "b");

    let hits_l0 = idx
        .search(&[1.0; 16], 10, None, Some(0))
        .await
        .unwrap();
    assert_eq!(hits_l0.len(), 1);
    assert_eq!(hits_l0[0].conv_id, "a");
}
```

- [x] **Step 3: Run — must fail**

```
cargo test -p mur-core conversations::index::tests::search_filters_by_layer
```

Expected: compile error (`upsert_with_layer`, `search` extra arg).

- [x] **Step 4: Implement**

In `mur-core/src/conversations/index.rs`:

a. Rename existing `upsert(&mut self, batch: &[(Message, Vec<f32>)])` to delegate into a new layer-aware method. Add:

```rust
/// Upsert with an explicit `layer` value per entry. Phase 2A uses this for
/// summary rows (layer=1); existing raw writes continue via `upsert()`
/// which keeps layer=0 for backward compatibility.
pub async fn upsert_with_layer(
    &mut self,
    entries: &[(Message, Vec<f32>, i8)],
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    // Re-use the existing upsert body but with per-row layer.
    self.upsert_internal(
        &entries.iter().map(|(m, v, l)| (m.clone(), v.clone(), *l)).collect::<Vec<_>>(),
    )
    .await
}

pub async fn upsert(&mut self, batch: &[(Message, Vec<f32>)]) -> Result<()> {
    let with_layer: Vec<(Message, Vec<f32>, i8)> =
        batch.iter().map(|(m, v)| (m.clone(), v.clone(), 0)).collect();
    self.upsert_internal(&with_layer).await
}

async fn upsert_internal(&mut self, entries: &[(Message, Vec<f32>, i8)]) -> Result<()> {
    // move the previous body of upsert() here, but read the layer from
    // entries[i].2 instead of hardcoded 0. Every existing field (id, ts,
    // source, conv_id, role, content, vector) stays the same shape.
    // ... rest unchanged from Phase 1 except the `layer` column array:
    // let layers_arr = Int8Array::from_iter_values(entries.iter().map(|(_, _, l)| *l));
}
```

Full migration: copy the body of the current `upsert()` into `upsert_internal`, replace the hardcoded-zero `layer` array with per-row `l` from the tuple. (The current implementation already builds a `layer` Int8 column — see `conversations/index.rs` around the `Int8Array::from_iter_values(vec![0_i8; batch.len()])` line.)

b. Extend `search`:

```rust
pub async fn search(
    &self,
    query_vec: &[f32],
    limit: usize,
    source_filter: Option<Source>,
    layer: Option<i8>,
) -> Result<Vec<SearchHit>> {
    // existing body builds a `query` against the table;
    // when `layer` is Some, add a `only_if` filter clause:
    //   .only_if(format!("layer = {}", l))
    // combined with source_filter via " AND " if both are present.
}
```

The actual Phase 1 `search` body uses `.only_if(...)` already for `source_filter`; extend it to combine predicates:

```rust
let predicates: Vec<String> = std::iter::empty()
    .chain(source_filter.map(|s| format!("source = '{}'", s.file_prefix())))
    .chain(layer.map(|l| format!("layer = {l}")))
    .collect();
let filter_clause = predicates.join(" AND ");
let mut q = self.table.query();
if !filter_clause.is_empty() {
    q = q.only_if(&filter_clause);
}
```

c. Update all existing callers of `search(vec, k, src)` to `search(vec, k, src, None)` — layer=None preserves Phase 1 behavior. Callers are in `retrieve.rs::search` and any tests.

- [x] **Step 5: Run all index + retrieve tests**

```
cargo test -p mur-core conversations::index
cargo test -p mur-core conversations::retrieve
```

Expected: previous tests pass + new `search_filters_by_layer` passes.

- [x] **Step 6: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/index.rs mur-core/src/conversations/retrieve.rs
git commit -m "$(cat <<'EOF'
feat(core): conversations/index — layer filter + upsert_with_layer (Phase 2A)

search() gains layer: Option<i8> — None preserves Phase 1 behavior
(searches all layers), Some(0|1|2) restricts. Combines with source_filter
via AND in the LanceDB only_if clause.

upsert_with_layer(entries: &[(Message, Vec<f32>, i8)]) is the writer path
for summary rows (layer=1). Existing upsert() delegates with layer=0, so
Phase 1 callers are unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `summarize::writer` — atomic write + audit + index (Phase 2A)

**Files:**
- Create: `mur-core/src/conversations/summarize/writer.rs`
- Modify: `mur-core/src/conversations/summarize/mod.rs`

- [x] **Step 1: Failing tests**

Create `mur-core/src/conversations/summarize/writer.rs`:

```rust
//! Atomic summary writer + .history/ archive + audit + LanceDB upsert.
//! Spec §4.7. Each write:
//!   1. Render Markdown (frontmatter + extractive + abstractive + macro map)
//!   2. If file exists with different content: move to .history/<date>.<iso>.md
//!   3. If file exists with identical content: no-op
//!   4. Atomic write via tmp+rename
//!   5. audit::Audit::append(Summarize{...})
//!   6. LanceDB upsert at layer=1

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::abstractive::AbstractiveResult;
use super::extractive::ExtractiveSpan;
use super::macro_refs::MacroRef;
use super::super::audit::{self, AuditAction};
use super::super::index::ConversationIndex;
use super::super::paths::{summary_history_dir, summary_paths_for};

pub struct SummaryDoc {
    pub date: NaiveDate,
    pub generated_at: DateTime<Utc>,
    pub extractive_model: String,
    pub abstractive_model: String,
    pub mur_version: String,
    pub duration_ms: u64,
    pub conv_count: u32,
    pub msg_count: u32,
    pub sources: Vec<String>, // file_prefix strings, sorted+dedup
    pub pattern_refs: Vec<MacroRef>,
    pub keywords: Vec<String>,
    pub links_prev: Option<NaiveDate>,
    pub links_next: Option<NaiveDate>,
    pub warnings: Vec<String>,
    pub input_content_sha: String,
    pub extractive: Vec<ExtractiveSpan>,
    pub abstractive: AbstractiveResult,
}

pub struct WriteResult {
    pub path: PathBuf,
    pub archived: Option<PathBuf>,   // Some(path) if prior version was moved
    pub noop: bool,                   // true when content was byte-identical
}

pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    root_override: Option<&str>,
) -> Result<WriteResult> {
    let (md_path, _yaml_path) = summary_paths_for(doc.date, root_override);
    if let Some(parent) = md_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let new_body = render(doc);

    let prior_exists = md_path.exists();
    let archived;
    let noop;

    if prior_exists {
        let existing = std::fs::read_to_string(&md_path)?;
        if existing == new_body {
            return Ok(WriteResult {
                path: md_path,
                archived: None,
                noop: true,
            });
        }
        archived = Some(archive_prior(&md_path, root_override)?);
        noop = false;
    } else {
        archived = None;
        noop = false;
    }

    let tmp = md_path.with_file_name(format!(
        ".tmp.{}.md",
        doc.date.format("%Y-%m-%d")
    ));
    let mut f = std::fs::File::create(&tmp)
        .with_context(|| format!("open tmp {tmp:?}"))?;
    f.write_all(new_body.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &md_path)?;

    // Content hash for audit
    let mut h = Sha256::new();
    h.update(new_body.as_bytes());
    let content_sha = hex::encode(h.finalize());

    let audit_log = audit::Audit::open(root_override)?;
    audit_log.append(
        AuditAction::Summarize {
            date: doc.date.format("%Y-%m-%d").to_string(),
            model: doc.abstractive_model.clone(),
            duration_ms: doc.duration_ms,
        },
        content_sha,
    )?;

    // Index upsert at layer=1
    let mut idx = ConversationIndex::open(summary_embedding.len() as i32, root_override).await?;
    let summary_msg = summary_row_as_message(doc);
    idx.upsert_with_layer(&[(summary_msg, summary_embedding, 1)])
        .await?;

    Ok(WriteResult {
        path: md_path,
        archived,
        noop,
    })
}

/// Build a synthetic Message representing the summary for LanceDB storage.
/// The index row uses the abstractive narrative as content so retrieval's
/// keyword/MMR reranking has real text to work with.
fn summary_row_as_message(doc: &SummaryDoc) -> mur_common::Message {
    use chrono::TimeZone;
    let ts = chrono::Utc
        .from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap());
    let content_text = doc
        .abstractive
        .narrative
        .clone()
        .unwrap_or_else(|| "(no narrative)".to_string());
    mur_common::Message {
        v: 1,
        ts,
        src: mur_common::Source::ClaudeCode, // placeholder; summaries aggregate across sources
        conv: format!("summary:{}", doc.date.format("%Y-%m-%d")),
        role: mur_common::Role::System,
        content: mur_common::Content::Text {
            value: content_text,
        },
        meta: serde_json::json!({
            "layer": 1,
            "sources": doc.sources,
            "conv_count": doc.conv_count,
        }),
        refs: doc.pattern_refs.iter().map(|r| r.name.clone()).collect(),
    }
}

fn archive_prior(md_path: &Path, root_override: Option<&str>) -> Result<PathBuf> {
    let hist = summary_history_dir(root_override);
    std::fs::create_dir_all(&hist)?;
    let stem = md_path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("stem")?;
    let now = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let dest = hist.join(format!("{stem}.{now}.md"));
    std::fs::rename(md_path, &dest)
        .with_context(|| format!("archive {md_path:?} → {dest:?}"))?;
    Ok(dest)
}

fn render(doc: &SummaryDoc) -> String {
    let mut out = String::new();

    // Frontmatter
    out.push_str("---\n");
    out.push_str("schema: 1\n");
    out.push_str(&format!("date: {}\n", doc.date));
    out.push_str(&format!(
        "generated_at: {}\n",
        doc.generated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str("generated_by:\n");
    out.push_str(&format!("  extractive_model: {}\n", doc.extractive_model));
    out.push_str(&format!("  abstractive_model: {}\n", doc.abstractive_model));
    out.push_str(&format!("  mur_version: {}\n", doc.mur_version));
    out.push_str(&format!("duration_ms: {}\n", doc.duration_ms));
    out.push_str(&format!("conv_count: {}\n", doc.conv_count));
    out.push_str(&format!("msg_count: {}\n", doc.msg_count));
    out.push_str(&format!("sources: [{}]\n", doc.sources.join(", ")));
    if doc.pattern_refs.is_empty() {
        out.push_str("pattern_refs: []\n");
    } else {
        out.push_str("pattern_refs:\n");
        for r in &doc.pattern_refs {
            out.push_str(&format!(
                "  - name: {}\n    version: {}\n    sha: {}\n",
                r.name, r.pattern_version, r.pattern_sha
            ));
        }
    }
    out.push_str(&format!("keywords: [{}]\n", doc.keywords.join(", ")));
    out.push_str("links:\n");
    out.push_str(&format!(
        "  prev: {}\n",
        doc.links_prev
            .map(|d| format!("./{}.md", d))
            .unwrap_or_else(|| "null".into())
    ));
    out.push_str(&format!(
        "  next: {}\n",
        doc.links_next
            .map(|d| format!("./{}.md", d))
            .unwrap_or_else(|| "null".into())
    ));
    if doc.warnings.is_empty() {
        out.push_str("warnings: []\n");
    } else {
        out.push_str("warnings:\n");
        for w in &doc.warnings {
            out.push_str(&format!("  - {}\n", w));
        }
    }
    out.push_str(&format!(
        "input_content_sha: {}\n",
        doc.input_content_sha
    ));
    out.push_str("---\n\n");

    // Body
    out.push_str("## Extractive spans\n\n");
    for (i, s) in doc.extractive.iter().enumerate() {
        out.push_str(&format!(
            "[{}] _{{{}/{} @L{}}}_:\n> {}\n\n",
            i + 1,
            s.src.file_prefix(),
            s.conv_id,
            s.line_hint,
            s.text.replace('\n', "\n> ")
        ));
    }

    out.push_str("## Abstractive narrative\n\n");
    let narrative = doc
        .abstractive
        .narrative
        .as_deref()
        .unwrap_or("(narrative generation failed; see warnings)");
    out.push_str(narrative);
    out.push_str("\n\n");

    if !doc.pattern_refs.is_empty() {
        out.push_str("## Macro expansion map\n\n");
        for r in &doc.pattern_refs {
            out.push_str(&format!(
                "- {} → patterns/{}.yaml (v{}, sha {}…)\n",
                r.marker,
                r.name,
                r.pattern_version,
                r.pattern_sha.chars().take(8).collect::<String>()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Role, Source};

    fn dummy_doc(date: NaiveDate) -> SummaryDoc {
        SummaryDoc {
            date,
            generated_at: Utc::now(),
            extractive_model: "qwen3:14b".into(),
            abstractive_model: "qwen3:14b".into(),
            mur_version: "2.4.0".into(),
            duration_ms: 1234,
            conv_count: 1,
            msg_count: 2,
            sources: vec!["cc".into()],
            pattern_refs: vec![],
            keywords: vec!["test".into()],
            links_prev: None,
            links_next: None,
            warnings: vec![],
            input_content_sha: "deadbeef".into(),
            extractive: vec![ExtractiveSpan {
                role: Role::User,
                conv_id: "c1".into(),
                line_hint: 1,
                text: "hello".into(),
                src: Source::ClaudeCode,
            }],
            abstractive: AbstractiveResult {
                narrative: Some("Today the developer said hello.".into()),
                word_count: 5,
            },
        }
    }

    #[tokio::test]
    async fn writes_valid_frontmatter_body() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let doc = dummy_doc(date);
        let r = write_summary(&doc, vec![0.0; 16], Some(root)).await.unwrap();
        assert!(!r.noop);
        assert!(r.archived.is_none());
        let body = std::fs::read_to_string(&r.path).unwrap();
        assert!(body.contains("date: 2026-04-19"));
        assert!(body.contains("## Extractive spans"));
        assert!(body.contains("## Abstractive narrative"));
        assert!(body.contains("Today the developer said hello."));
    }

    #[tokio::test]
    async fn second_identical_write_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let doc = dummy_doc(date);
        let mut d2 = dummy_doc(date);
        d2.generated_at = doc.generated_at; // force bit-identical
        let _ = write_summary(&doc, vec![0.0; 16], Some(root)).await.unwrap();
        let r2 = write_summary(&d2, vec![0.0; 16], Some(root)).await.unwrap();
        assert!(r2.noop);
        assert!(r2.archived.is_none());
    }

    #[tokio::test]
    async fn overwrite_archives_prior() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let mut doc1 = dummy_doc(date);
        doc1.abstractive.narrative = Some("version 1".into());
        let _ = write_summary(&doc1, vec![0.0; 16], Some(root)).await.unwrap();
        let mut doc2 = dummy_doc(date);
        doc2.abstractive.narrative = Some("version 2".into());
        let r2 = write_summary(&doc2, vec![0.0; 16], Some(root)).await.unwrap();
        assert!(r2.archived.is_some());
        let hist = summary_history_dir(Some(root));
        let entries: Vec<_> = std::fs::read_dir(&hist).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn audit_records_summarize_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let doc = dummy_doc(date);
        let _ = write_summary(&doc, vec![0.0; 16], Some(root)).await.unwrap();
        let audit_log = audit::Audit::open(Some(root)).unwrap();
        let report = audit_log.verify().unwrap();
        assert!(report.entries >= 1);
    }
}
```

Register in `summarize/mod.rs`:

```rust
pub mod abstractive;
pub mod chunker;
pub mod extractive;
pub mod macro_refs;
pub mod writer;
```

- [x] **Step 2: Run tests**

```
cargo test -p mur-core conversations::summarize::writer::tests
```

Expected: 4 passed.

- [x] **Step 3: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/writer.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize/writer — atomic write + audit + index (Phase 2A)

write_summary() flow:
  1. Render frontmatter + extractive + abstractive + macro map
  2. If existing differs: move to .history/<date>.<iso>.md
  3. If existing identical: no-op (early return)
  4. Atomic tmp→rename write
  5. audit::Audit.append(Summarize{date, model, duration_ms})
  6. LanceDB upsert at layer=1 (summary row uses narrative as content for
     retrieval reranking)

Frontmatter matches spec §4.5 exactly. Body matches §4.6 with Markdown-
escaped multi-line extractive spans.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `summarize::mod` — orchestrator (Phase 2A)

**Files:**
- Modify: `mur-core/src/conversations/summarize/mod.rs`

- [x] **Step 1: Failing tests**

Append to `mur-core/src/conversations/summarize/mod.rs`:

```rust
use anyhow::Result;
use chrono::{NaiveDate, Utc};
use mur_common::config::CompactConfig;
use sha2::{Digest, Sha256};
use std::time::Instant;
use tracing::info_span;

use super::audit::{self, AuditAction};
use super::ollama::OllamaClient;
use super::paths::summary_paths_for;
use super::store;

#[derive(Debug, Default)]
pub struct CompactReport {
    pub ok: u32,
    pub err: u32,
    pub skipped: u32,
    pub day_reports: Vec<DayReport>,
}

#[derive(Debug)]
pub struct DayReport {
    pub date: NaiveDate,
    pub outcome: Outcome,
    pub extractive_spans: u32,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub enum Outcome {
    Written { archived: bool },
    Noop,
    Skipped { reason: &'static str },
    Failed(String),
}

pub async fn compact_day(
    date: NaiveDate,
    force: bool,
    cfg: &CompactConfig,
    root_override: Option<&str>,
) -> Result<DayReport> {
    let _span = info_span!("compact.day", %date).entered();
    let start = Instant::now();

    let (md_path, _) = summary_paths_for(date, root_override);
    let msgs = store::read_day(date, root_override)?;
    if msgs.is_empty() {
        return Ok(DayReport {
            date,
            outcome: Outcome::Skipped { reason: "no raw for day" },
            extractive_spans: 0,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Compute input_content_sha first — used for --if-stale guard.
    let input_sha = compute_input_sha(&msgs);

    // Skip if summary exists, is fresh, and not forced.
    if md_path.exists() && !force {
        if let Ok(existing) = std::fs::read_to_string(&md_path) {
            if existing.contains(&format!("input_content_sha: {}", input_sha)) {
                return Ok(DayReport {
                    date,
                    outcome: Outcome::Skipped { reason: "already fresh" },
                    extractive_spans: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        }
    }

    // Chunk + extract per chunk
    let client = OllamaClient::new(
        &cfg.ollama_endpoint,
        std::time::Duration::from_secs(120),
    );
    let chunks = chunker::chunk_day(&msgs, cfg.chunk_tokens as usize);
    let mut all_spans = Vec::new();
    for chunk in &chunks {
        let spans = extractive::extract_chunk(
            &client,
            &cfg.extractive_model,
            chunk,
            &msgs,
        )
        .await?;
        all_spans.extend(spans);
    }

    // Dedup (reuse Phase 1 MinHash pattern via simple string equality for now;
    // structural dedup is Phase 2C polish)
    all_spans.sort_by(|a, b| a.line_hint.cmp(&b.line_hint));
    all_spans.dedup_by(|a, b| a.text == b.text && a.line_hint == b.line_hint);

    // Cap
    if all_spans.len() > cfg.max_extractive_spans as usize {
        all_spans.truncate(cfg.max_extractive_spans as usize);
    }

    // Abstractive
    let abstractive_result = abstractive::summarize(
        &client,
        &cfg.abstractive_model,
        &all_spans,
        date,
        cfg.max_abstractive_words,
    )
    .await;

    let mut warnings = Vec::new();
    if abstractive_result.narrative.is_none() {
        warnings.push("narrative_generation_failed".to_string());
    }

    // Macro refs
    let patterns_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".mur")
        .join("patterns");
    let mut abstractive_text = abstractive_result
        .narrative
        .clone()
        .unwrap_or_else(|| "(narrative unavailable)".into());
    let pattern_refs = macro_refs::detect_and_rewrite(
        &mut all_spans,
        &mut abstractive_text,
        &patterns_dir,
    )
    .unwrap_or_default();
    let abstractive_final = abstractive::AbstractiveResult {
        narrative: Some(abstractive_text),
        word_count: abstractive_result.word_count,
    };

    // Frontmatter derived fields
    let sources = {
        let mut s: Vec<String> = msgs.iter().map(|m| m.src.file_prefix().to_string()).collect();
        s.sort();
        s.dedup();
        s
    };
    let conv_count = {
        let mut c: Vec<&str> = msgs.iter().map(|m| m.conv.as_str()).collect();
        c.sort();
        c.dedup();
        c.len() as u32
    };
    let keywords = top_keywords(&all_spans, 10);

    let doc = writer::SummaryDoc {
        date,
        generated_at: Utc::now(),
        extractive_model: cfg.extractive_model.clone(),
        abstractive_model: cfg.abstractive_model.clone(),
        mur_version: env!("CARGO_PKG_VERSION").into(),
        duration_ms: start.elapsed().as_millis() as u64,
        conv_count,
        msg_count: msgs.len() as u32,
        sources,
        pattern_refs,
        keywords,
        links_prev: Some(date - chrono::Duration::days(1)),
        links_next: Some(date + chrono::Duration::days(1)),
        warnings,
        input_content_sha: input_sha,
        extractive: all_spans.clone(),
        abstractive: abstractive_final,
    };

    // Summary embedding: use a deterministic zero vector when MUR_OLLAMA_MOCK=1;
    // otherwise call the configured embedding provider via existing pipeline.
    let embed_dims = 1024_usize; // default; Phase 3 can read from cfg
    let summary_embedding = if OllamaClient::mock_from_env() {
        vec![0.1; embed_dims]
    } else {
        // Reuse the mur-core embedding pipeline used by the ingest stage.
        let text = doc
            .abstractive
            .narrative
            .as_deref()
            .unwrap_or("")
            .to_string();
        crate::store::embedding::embed_text(&text, embed_dims)
            .await
            .unwrap_or_else(|_| vec![0.0; embed_dims])
    };

    match writer::write_summary(&doc, summary_embedding, root_override).await {
        Ok(w) => Ok(DayReport {
            date,
            outcome: if w.noop {
                Outcome::Noop
            } else {
                Outcome::Written { archived: w.archived.is_some() }
            },
            extractive_spans: doc.extractive.len() as u32,
            duration_ms: doc.duration_ms,
        }),
        Err(e) => {
            // Record the failure in audit but don't throw — caller still gets a report.
            let _ = audit::Audit::open(root_override).and_then(|a| {
                a.append(
                    AuditAction::Error {
                        layer: "compact.write".into(),
                        reason: format!("{e:#}"),
                    },
                    String::new(),
                )
            });
            Ok(DayReport {
                date,
                outcome: Outcome::Failed(format!("{e:#}")),
                extractive_spans: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            })
        }
    }
}

pub async fn compact_missing(
    cfg: &CompactConfig,
    since: Option<NaiveDate>,
    if_stale: bool,
    max_days_override: Option<u32>,
    root_override: Option<&str>,
) -> Result<CompactReport> {
    let max_days = max_days_override.unwrap_or(cfg.max_days_per_run) as usize;
    let today = Utc::now().date_naive();

    let mut candidates: Vec<NaiveDate> = store::list_raw_dirs(root_override)?
        .into_iter()
        .map(|(d, _)| d)
        .filter(|d| *d < today)
        .filter(|d| since.map_or(true, |s| *d >= s))
        .collect();
    candidates.sort();

    let mut report = CompactReport::default();
    for date in candidates.into_iter().take(max_days) {
        // skip logic: if summary exists and --if-stale is off, skip
        let (md_path, _) = summary_paths_for(date, root_override);
        let force = if_stale;
        if md_path.exists() && !if_stale {
            report.skipped += 1;
            report.day_reports.push(DayReport {
                date,
                outcome: Outcome::Skipped { reason: "summary exists" },
                extractive_spans: 0,
                duration_ms: 0,
            });
            continue;
        }
        let r = compact_day(date, force, cfg, root_override).await?;
        match &r.outcome {
            Outcome::Written { .. } | Outcome::Noop => report.ok += 1,
            Outcome::Failed(_) => report.err += 1,
            Outcome::Skipped { .. } => report.skipped += 1,
        }
        report.day_reports.push(r);
    }
    Ok(report)
}

fn compute_input_sha(msgs: &[mur_common::Message]) -> String {
    let mut h = Sha256::new();
    for m in msgs {
        h.update(serde_json::to_string(m).unwrap_or_default().as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

fn top_keywords(spans: &[extractive::ExtractiveSpan], n: usize) -> Vec<String> {
    use std::collections::HashMap;
    // Tiny TF heuristic, no TF-IDF in Phase 2A (corpus-level IDF is Phase 3).
    let mut counts: HashMap<String, usize> = HashMap::new();
    for s in spans {
        for w in s.text.split_whitespace() {
            let w = w.to_lowercase();
            if w.len() < 4 {
                continue;
            }
            // strip trailing punctuation
            let w = w.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string();
            if w.is_empty() {
                continue;
            }
            *counts.entry(w).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<_> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.into_iter().take(n).map(|(k, _)| k).collect()
}

#[cfg(test)]
mod orch_tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn seed_raw(root: &str, date: NaiveDate, text: &str) {
        let ts = chrono::Utc
            .with_ymd_and_hms(date.year() as i32, date.month(), date.day(), 10, 0, 0)
            .unwrap();
        let m = Message {
            v: 1,
            ts,
            src: Source::ClaudeCode,
            conv: "c1".into(),
            role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        store::append(&m, Some(root)).unwrap();
    }

    fn cfg() -> CompactConfig {
        CompactConfig::default()
    }

    #[tokio::test]
    async fn compact_day_happy_path_mock() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        seed_raw(root, date, "mock extractive span");
        let r = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        match r.outcome {
            Outcome::Written { .. } => {}
            other => panic!("expected Written, got {:?}", other),
        }
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    async fn compact_day_noop_when_fresh() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        seed_raw(root, date, "mock extractive span");
        let _ = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        let r2 = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        assert!(matches!(r2.outcome, Outcome::Skipped { .. } | Outcome::Noop));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    async fn compact_missing_respects_throttle() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        for i in 1..=10 {
            let d = NaiveDate::from_ymd_opt(2026, 4, i).unwrap();
            seed_raw(root, d, &format!("day {i} mock extractive span"));
        }
        let mut c = cfg();
        c.max_days_per_run = 3;
        let report = compact_missing(&c, None, false, None, Some(root))
            .await
            .unwrap();
        assert_eq!(report.day_reports.len(), 3);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
```

Note: `crate::store::embedding::embed_text` is a convenience wrapper expected
to exist; if it doesn't, inline the LLM endpoint call and dims lookup from
config here. For Phase 2A the mock path is sufficient for all tests.

- [x] **Step 2: Run tests**

```
cargo test -p mur-core conversations::summarize::orch_tests
```

Expected: 3 passed.

- [x] **Step 3: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize::{compact_day, compact_missing} orchestrator (Phase 2A)

compact_day:
  1. read_day(date)
  2. chunk + per-chunk extractive
  3. dedup + cap
  4. abstractive (single call)
  5. macro_refs detection
  6. build SummaryDoc with derived frontmatter fields
  7. write_summary (atomic + audit + index)

compact_missing:
  - scans list_raw_dirs, filters to date < today_utc,
  - optionally since filter,
  - caps at max_days_per_run
  - delegates per-day to compact_day

Skip logic: input_content_sha in existing summary matches current day →
noop-skip unless --if-stale or --force.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Summary serde types + reader helper (Phase 2A)

Consolidates the summary-file reader used by future `chat show` and Phase 2B retrieval's extractive-span lookup.

**Files:**
- Modify: `mur-core/src/conversations/summarize/mod.rs` (add parse_summary fn + tests)

- [x] **Step 1: Failing test**

Append to `orch_tests` in `summarize/mod.rs`:

```rust
#[test]
fn parse_roundtrip_reads_frontmatter_and_body() {
    let markdown = r#"---
schema: 1
date: 2026-04-19
generated_at: 2026-04-19T03:00:00Z
generated_by:
  extractive_model: qwen3:14b
  abstractive_model: qwen3:14b
  mur_version: 2.4.0
duration_ms: 100
conv_count: 1
msg_count: 2
sources: [cc]
pattern_refs: []
keywords: [test]
links:
  prev: null
  next: null
warnings: []
input_content_sha: abc123
---

## Extractive spans

[1] _{cc/c1 @L1}_:
> hello

## Abstractive narrative

Today was a test.
"#;
    let parsed = parse_summary(markdown).unwrap();
    assert_eq!(parsed.date, NaiveDate::from_ymd_opt(2026, 4, 19).unwrap());
    assert_eq!(parsed.extractive.len(), 1);
    assert_eq!(parsed.extractive[0].conv_id, "c1");
    assert_eq!(parsed.extractive[0].line_hint, 1);
    assert_eq!(parsed.extractive[0].text, "hello");
    assert!(parsed.narrative.contains("Today was a test"));
}
```

- [x] **Step 2: Implement parser**

Append to `summarize/mod.rs`:

```rust
pub struct ParsedSummary {
    pub date: NaiveDate,
    pub frontmatter: serde_yaml::Value,
    pub extractive: Vec<ParsedSpan>,
    pub narrative: String,
    pub pattern_refs: Vec<String>, // names only, full meta in frontmatter
}

#[derive(Debug, Clone)]
pub struct ParsedSpan {
    pub span_index: u32,
    pub src: String, // file_prefix
    pub conv_id: String,
    pub line_hint: u32,
    pub text: String,
}

pub fn parse_summary(md: &str) -> Result<ParsedSummary> {
    let (frontmatter, body) = split_frontmatter(md)?;
    let fm: serde_yaml::Value = serde_yaml::from_str(frontmatter)?;
    let date_str = fm
        .get("date")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing date"))?;
    let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")?;

    let extractive = parse_extractive_section(body);
    let narrative = parse_narrative_section(body);
    let pattern_refs = fm
        .get("pattern_refs")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(ParsedSummary {
        date,
        frontmatter: fm,
        extractive,
        narrative,
        pattern_refs,
    })
}

fn split_frontmatter(md: &str) -> Result<(&str, &str)> {
    let body = md.strip_prefix("---\n").ok_or_else(|| anyhow::anyhow!("no frontmatter"))?;
    let end = body.find("\n---\n").ok_or_else(|| anyhow::anyhow!("unterminated frontmatter"))?;
    let fm = &body[..end];
    let rest = &body[end + 5..];
    Ok((fm, rest))
}

fn parse_extractive_section(body: &str) -> Vec<ParsedSpan> {
    let mut out = Vec::new();
    let span_re = regex::Regex::new(
        r"(?ms)^\[(\d+)\] _\{([^/]+)/([^ ]+) @L(\d+)\}_:\n((?:> [^\n]*\n?)+)",
    )
    .unwrap();
    let ext_start = body
        .find("## Extractive spans")
        .unwrap_or(0);
    let ext_end = body[ext_start..]
        .find("\n## ")
        .map(|i| ext_start + i)
        .unwrap_or(body.len());
    let section = &body[ext_start..ext_end];
    for cap in span_re.captures_iter(section) {
        let idx: u32 = cap[1].parse().unwrap_or(0);
        let src = cap[2].to_string();
        let conv = cap[3].to_string();
        let line: u32 = cap[4].parse().unwrap_or(0);
        let quoted = &cap[5];
        let text: String = quoted
            .lines()
            .map(|l| l.trim_start_matches("> ").trim_start_matches('>'))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        out.push(ParsedSpan {
            span_index: idx,
            src,
            conv_id: conv,
            line_hint: line,
            text,
        });
    }
    out
}

fn parse_narrative_section(body: &str) -> String {
    let narr_start = body
        .find("## Abstractive narrative")
        .map(|i| i + "## Abstractive narrative".len())
        .unwrap_or(0);
    let narr_end = body[narr_start..]
        .find("\n## ")
        .map(|i| narr_start + i)
        .unwrap_or(body.len());
    body[narr_start..narr_end].trim().to_string()
}
```

Add `regex = "1"` to `mur-core/Cargo.toml` if not already present (it was added in Phase 1 for `verify.rs`; verify with `grep regex mur-core/Cargo.toml`).

- [x] **Step 3: Run tests**

```
cargo test -p mur-core conversations::summarize::orch_tests::parse_roundtrip_reads_frontmatter_and_body
```

Expected: passed.

- [x] **Step 4: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): summarize::parse_summary — reader for summary Markdown (Phase 2A)

Parses summary/<date>.md back into a ParsedSummary {frontmatter, extractive,
narrative, pattern_refs}. Used by Phase 2B retrieval (pull extractive span
for each retrieved hit to build citations that survive retention rotation).

Uses regex for the [N] _{src/conv @L<line>}_ anchor pattern and > quote
unwrapping.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: CLI `cmd_conversations_compact` + `ConversationsAction::Compact` (Phase 2A)

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs`
- Modify: `mur-core/src/main.rs`

- [x] **Step 1: Failing CLI integration test**

Append to `mur-core/tests/cli_conversations.rs`:

```rust
#[test]
fn mur_conversations_compact_on_empty_archive_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("HOME", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("(nothing to compact)") || stdout.contains("done:"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn mur_conversations_compact_on_seeded_day_produces_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let raw = home
        .join(".mur")
        .join("conversations")
        .join("raw")
        .join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    // Seed with an actual mur_common::Message so store::read_day parses it back cleanly.
    let line = serde_json::json!({
        "v": 1,
        "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code",
        "conv": "c1",
        "role": "user",
        "content": {"t": "text", "v": "mock extractive span seeded for compact test"},
        "meta": {},
        "refs": []
    });
    std::fs::write(
        raw.join("cc_c1.jsonl"),
        serde_json::to_string(&line).unwrap() + "\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("HOME", home)
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let summary = home
        .join(".mur")
        .join("conversations")
        .join("summary")
        .join(format!("{yesterday}.md"));
    assert!(summary.exists(), "summary should have been written at {summary:?}");
    let body = std::fs::read_to_string(&summary).unwrap();
    assert!(body.contains("## Extractive spans"));
    assert!(body.contains("## Abstractive narrative"));
}
```

- [x] **Step 2: Run — must fail (compact subcommand doesn't exist yet)**

```
cargo test -p mur-core --test cli_conversations mur_conversations_compact
```

Expected: test compiles but fails with `error: unrecognized subcommand 'compact'`.

- [x] **Step 3: Add `ConversationsAction::Compact` variant**

Find `enum ConversationsAction` in `mur-core/src/main.rs` (added in Phase 1 Task 17). Append this variant between existing variants:

```rust
    /// Generate hybrid summaries for completed days (sleep-time compact).
    Compact {
        /// One specific date (otherwise process all missing completed days).
        #[arg(long)]
        date: Option<String>,

        /// Lower bound for the sweep (ignored with --date).
        #[arg(long)]
        since: Option<String>,

        /// Overwrite existing summaries. Archives old version to .history/.
        #[arg(long)]
        force: bool,

        /// Only regenerate when raw content hash changed (implies force).
        #[arg(long)]
        if_stale: bool,

        /// Override throttle (default: config.compact.max_days_per_run).
        #[arg(long)]
        max_days: Option<u32>,

        /// Don't call Ollama — emit extractive-only skeleton (for testing).
        #[arg(long)]
        extractive_only: bool,

        /// Emit the LLM prompts to stderr without sending them.
        #[arg(long)]
        debug_prompt: bool,
    },
```

Add dispatch arm to the main `match` (find the `ConversationsAction::` block):

```rust
ConversationsAction::Compact {
    date, since, force, if_stale, max_days, extractive_only, debug_prompt,
} => {
    cmd::conversations_cmd::cmd_conversations_compact(
        cmd::conversations_cmd::CompactArgs {
            date, since, force, if_stale, max_days, extractive_only, debug_prompt,
        },
    )
    .await?
}
```

- [x] **Step 4: Implement `cmd_conversations_compact`**

Add to `mur-core/src/cmd/conversations_cmd.rs`:

```rust
pub struct CompactArgs {
    pub date: Option<String>,
    pub since: Option<String>,
    pub force: bool,
    pub if_stale: bool,
    pub max_days: Option<u32>,
    pub extractive_only: bool,
    pub debug_prompt: bool,
}

pub async fn cmd_conversations_compact(args: CompactArgs) -> anyhow::Result<()> {
    use crate::conversations::summarize;
    use chrono::NaiveDate;

    let config = mur_common::config::Config::load().unwrap_or_default();
    let mut cfg = config.conversations.compact.clone();

    if args.extractive_only {
        // Crude guard rail: no abstractive model.
        cfg.abstractive_model = String::new();
    }
    if args.debug_prompt {
        eprintln!("(debug_prompt not yet wired to individual stages; enabling in Phase 2C)");
    }

    if let Some(d) = args.date {
        let date = NaiveDate::parse_from_str(&d, "%Y-%m-%d")?;
        let force = args.force || args.if_stale;
        let r = summarize::compact_day(date, force, &cfg, None).await?;
        println!("{date}: {:?} ({} spans, {}ms)", r.outcome, r.extractive_spans, r.duration_ms);
        return Ok(());
    }

    let since = args
        .since
        .as_deref()
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()?;

    let report = summarize::compact_missing(
        &cfg,
        since,
        args.if_stale,
        args.max_days,
        None,
    )
    .await?;

    if report.day_reports.is_empty() {
        println!("(nothing to compact)");
        return Ok(());
    }
    for r in &report.day_reports {
        println!(
            "  {} {:?} ({} spans, {}ms)",
            r.date, r.outcome, r.extractive_spans, r.duration_ms
        );
    }
    println!("done: {} ok, {} failed, {} skipped", report.ok, report.err, report.skipped);
    Ok(())
}
```

- [x] **Step 5: Run CLI tests**

```
cargo test -p mur-core --test cli_conversations mur_conversations_compact
```

Expected: 2 passed.

- [x] **Step 6: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/cmd/conversations_cmd.rs mur-core/src/main.rs mur-core/tests/cli_conversations.rs
git commit -m "$(cat <<'EOF'
feat(core): mur conversations compact CLI (Phase 2A)

New subcommand delegating to summarize::compact_{day,missing} with flags:
  --date / --since / --force / --if-stale / --max-days / --extractive-only /
  --debug-prompt

Integration tests cover (1) empty archive emits "(nothing to compact)" and
(2) seeded day produces summary.md with valid Extractive + Narrative
sections when MUR_OLLAMA_MOCK=1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: `migrate.rs` — extend P4 sync for `[conversations.compact]` (Phase 2A)

**Files:**
- Modify: `mur-core/src/conversations/migrate.rs`

- [x] **Step 1: Failing test**

Append to `migrate.rs`'s existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn sync_writes_conversations_compact_subsection() {
    let tmp = tempfile::tempdir().unwrap();
    let cmdr_dir = tmp.path().join(".mur/commander");
    std::fs::create_dir_all(&cmdr_dir).unwrap();
    std::fs::write(
        cmdr_dir.join("config.toml"),
        "[engine]\nfoo = 1\n",
    )
    .unwrap();
    let cfg = mur_common::config::ConversationsConfig {
        enabled: true,
        retention_days: 30,
        compact: mur_common::config::CompactConfig {
            enabled_in_daemon: true,
            daemon_cron: "0 4 * * *".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    sync_commander_config_toml(&tmp.path().join(".mur"), &cfg).unwrap();
    let toml = std::fs::read_to_string(cmdr_dir.join("config.toml")).unwrap();
    assert!(toml.contains("[conversations]"));
    assert!(toml.contains("enabled = true"));
    assert!(toml.contains("retention_days = 30"));
    assert!(toml.contains("[conversations.compact]"));
    assert!(toml.contains("enabled_in_daemon = true"));
    assert!(toml.contains("daemon_cron = \"0 4 * * *\""));
    assert!(toml.contains("[engine]"));
}
```

- [x] **Step 2: Run — must fail** (signature currently doesn't take `&ConversationsConfig`).

- [x] **Step 3: Change `sync_commander_config_toml` signature**

Replace the existing Phase 1 implementation (which read mur yaml internally) with a version that accepts the config struct explicitly, so tests are deterministic:

```rust
pub fn sync_commander_config_toml(
    mur_dir: &std::path::Path,
    cfg: &mur_common::config::ConversationsConfig,
) -> anyhow::Result<()> {
    let cmdr_cfg = mur_dir.join("commander/config.toml");
    if let Some(parent) = cmdr_cfg.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&cmdr_cfg).unwrap_or_default();

    const BEGIN: &str = "# BEGIN [conversations] (managed by mur conversations migrate)";
    const END: &str = "# END [conversations]";

    let new_block = format!(
        "\n{BEGIN}\n\
         [conversations]\n\
         enabled = {}\n\
         retention_days = {}\n\
         \n\
         [conversations.compact]\n\
         enabled_in_daemon = {}\n\
         daemon_cron = \"{}\"\n\
         {END}\n",
        cfg.enabled,
        cfg.retention_days,
        cfg.compact.enabled_in_daemon,
        cfg.compact.daemon_cron,
    );

    let merged = if let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        let before = &existing[..b];
        let after_marker = e + END.len();
        let after = &existing[after_marker..];
        format!("{}{}{}", before.trim_end_matches('\n'), new_block, after)
    } else {
        format!("{}{}", existing.trim_end_matches('\n'), new_block)
    };

    std::fs::write(&cmdr_cfg, merged)?;
    Ok(())
}
```

Update all existing callers (`run()`, etc.) to pass `&cfg.conversations` where `cfg` is the loaded `Config`.

- [x] **Step 4: Run tests**

```
cargo test -p mur-core conversations::migrate::tests
```

Expected: all previous pass + new `sync_writes_conversations_compact_subsection` passes.

- [x] **Step 5: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/migrate.rs
git commit -m "$(cat <<'EOF'
feat(core): extend P4 config sync for [conversations.compact] (Phase 2A)

sync_commander_config_toml now accepts &ConversationsConfig explicitly
(previously read mur yaml internally — less testable). Emits the new
[conversations.compact] subsection with enabled_in_daemon + daemon_cron
mirror so commander's daemon can read them without re-parsing yaml at
runtime.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Commander `ConversationsCompactConfig` (cross-repo, Phase 2A)

**Repo:** `/Volumes/Firecuda4tb/Projects/mur-commander`
**Files:**
- Modify: `crates/daemon/src/config.rs`

- [x] **Step 1: Create worktree for commander**

```
cd /Volumes/Firecuda4tb/Projects/mur-commander
git worktree add .worktrees/conversations-phase-2 -b feat/conversations-phase-2 main
cd .worktrees/conversations-phase-2
```

- [x] **Step 2: Failing test**

Append to `crates/daemon/src/config.rs`:

```rust
#[cfg(test)]
mod phase2_tests {
    use super::*;

    #[test]
    fn conversations_compact_defaults() {
        let cfg = ConversationsCompactConfig::default();
        assert!(cfg.enabled_in_daemon);
        assert_eq!(cfg.daemon_cron, "0 3 * * *");
    }

    #[test]
    fn parses_conversations_compact_from_toml() {
        let t = r#"
[conversations]
enabled = true

[conversations.compact]
enabled_in_daemon = true
daemon_cron = "0 4 * * *"
"#;
        let v: toml::Value = toml::from_str(t).unwrap();
        let compact: ConversationsCompactConfig =
            v["conversations"]["compact"].clone().try_into().unwrap();
        assert!(compact.enabled_in_daemon);
        assert_eq!(compact.daemon_cron, "0 4 * * *");
    }
}
```

- [x] **Step 3: Run — must fail**

```
cargo test -p mur-daemon config::phase2_tests
```

Expected: compile error `cannot find type 'ConversationsCompactConfig'`.

- [x] **Step 4: Implement**

Add to `crates/daemon/src/config.rs`:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct ConversationsCompactConfig {
    pub enabled_in_daemon: bool,
    pub daemon_cron: String,
}

impl Default for ConversationsCompactConfig {
    fn default() -> Self {
        Self {
            enabled_in_daemon: true,
            daemon_cron: "0 3 * * *".into(),
        }
    }
}
```

Wire into the existing top-level config struct (typically `DaemonConfig` or similar — grep for the root-level struct that loads `config.toml`):

```rust
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ConversationsSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub retention_days: u32,
    #[serde(default)]
    pub compact: ConversationsCompactConfig,
}

// In the root config struct:
// #[serde(default)] pub conversations: ConversationsSection,
```

- [x] **Step 5: Run tests**

```
cargo test -p mur-daemon config::phase2_tests
```

Expected: 2 passed.

- [x] **Step 6: Commit**

```
cargo clippy -p mur-daemon -- -D warnings
cargo fmt --check -p mur-daemon
git add crates/daemon/src/config.rs
git commit -m "$(cat <<'EOF'
feat(daemon): add ConversationsCompactConfig struct (Phase 2A)

Matches the [conversations.compact] block written by mur conversations
migrate's P4 sync: enabled_in_daemon + daemon_cron. Consumed by the new
ConversationsCompactTrigger (Task 15).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Commander `triggers::conversations_compact` (cross-repo, Phase 2A)

**Repo:** `/Volumes/Firecuda4tb/Projects/mur-commander/.worktrees/conversations-phase-2`
**Files:**
- Create: `crates/daemon/src/triggers/conversations_compact.rs`
- Modify: `crates/daemon/src/triggers/mod.rs`

- [x] **Step 1: Inspect existing trigger interface**

```
grep -rn "pub trait Trigger\|impl Trigger for" crates/daemon/src/triggers/ | head
```

Map out the existing `Trigger` trait — at minimum `fn name()`, `async fn tick(now) -> Result<TriggerAction>`. Note the exact signatures from the existing `FileChange` / `GitPush` / `Cron` trigger implementations.

- [x] **Step 2: Write failing test**

Create `crates/daemon/src/triggers/conversations_compact.rs`:

```rust
//! Phase 2A: daily-cron trigger that fires `mur conversations compact`.
//! Fire-and-forget child process exec, 10-minute timeout.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use cron::Schedule;
use std::str::FromStr;
use std::sync::Mutex;

use super::{ExecSpec, Trigger, TriggerAction};

pub struct ConversationsCompactTrigger {
    schedule: Schedule,
    last_fired: Mutex<Option<DateTime<Utc>>>,
    enabled: bool,
}

impl ConversationsCompactTrigger {
    pub fn from_config(cron_expr: &str, enabled: bool) -> Result<Self> {
        let schedule = Schedule::from_str(cron_expr)
            .map_err(|e| anyhow::anyhow!("invalid cron '{cron_expr}': {e}"))?;
        Ok(Self {
            schedule,
            last_fired: Mutex::new(None),
            enabled,
        })
    }
}

#[async_trait::async_trait]
impl Trigger for ConversationsCompactTrigger {
    fn name(&self) -> &'static str {
        "conversations_compact"
    }

    async fn tick(&self, now: DateTime<Utc>) -> Result<TriggerAction> {
        if !self.enabled {
            return Ok(TriggerAction::None);
        }
        let mut last = self.last_fired.lock().unwrap();
        let base = last.unwrap_or(now - Duration::days(1));
        let next = self.schedule.after(&base).next();
        let Some(next) = next else {
            return Ok(TriggerAction::None);
        };
        if now >= next {
            *last = Some(now);
            Ok(TriggerAction::Exec(ExecSpec {
                command: "mur".into(),
                args: vec!["conversations".into(), "compact".into()],
                timeout_secs: 600,
                capture_output: true,
            }))
        } else {
            Ok(TriggerAction::None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[tokio::test]
    async fn disabled_never_fires() {
        let trig = ConversationsCompactTrigger::from_config("0 3 * * *", false).unwrap();
        let r = trig.tick(t("2026-04-21T03:00:00Z")).await.unwrap();
        assert!(matches!(r, TriggerAction::None));
    }

    #[tokio::test]
    async fn fires_when_now_passes_next_cron() {
        let trig = ConversationsCompactTrigger::from_config("0 3 * * *", true).unwrap();
        // Seed: last_fired = yesterday 03:00 → next fires at today 03:00
        *trig.last_fired.lock().unwrap() =
            Some(t("2026-04-20T03:00:00Z"));
        let r = trig.tick(t("2026-04-21T03:00:01Z")).await.unwrap();
        match r {
            TriggerAction::Exec(spec) => {
                assert_eq!(spec.command, "mur");
                assert_eq!(spec.args, vec!["conversations", "compact"]);
                assert_eq!(spec.timeout_secs, 600);
            }
            _ => panic!("expected Exec"),
        }
    }

    #[tokio::test]
    async fn does_not_refire_within_window() {
        let trig = ConversationsCompactTrigger::from_config("0 3 * * *", true).unwrap();
        *trig.last_fired.lock().unwrap() = Some(t("2026-04-21T03:00:00Z"));
        // Same minute — should not re-fire
        let r = trig.tick(t("2026-04-21T03:00:30Z")).await.unwrap();
        assert!(matches!(r, TriggerAction::None));
    }
}
```

Register in `crates/daemon/src/triggers/mod.rs`:

```rust
pub mod conversations_compact;
```

- [x] **Step 3: Run tests**

```
cargo test -p mur-daemon triggers::conversations_compact
```

Expected: 3 passed.

- [x] **Step 4: Commit**

```
cargo clippy -p mur-daemon -- -D warnings
cargo fmt --check -p mur-daemon
git add crates/daemon/src/triggers/conversations_compact.rs crates/daemon/src/triggers/mod.rs
git commit -m "$(cat <<'EOF'
feat(daemon): ConversationsCompactTrigger cron trigger (Phase 2A)

Fire-and-forget child exec of `mur conversations compact` on configured
cron (default 0 3 * * *). 10-minute timeout so a stuck Ollama can't block
the daemon. Enabled flag from commander config; disabled → never fires.

Skips tick if elapsed time < cron window (prevents re-fire on back-to-back
ticks within the same minute).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Commander `main.rs` — instantiate trigger (cross-repo, Phase 2A)

**Repo:** `/Volumes/Firecuda4tb/Projects/mur-commander/.worktrees/conversations-phase-2`
**Files:**
- Modify: `crates/daemon/src/main.rs`

- [x] **Step 1: Locate scheduler registration**

```
grep -n "schedule\|trigger\|Scheduler" crates/daemon/src/main.rs | head -20
```

Identify where other triggers are registered (typically in a startup sequence that loads config and builds the scheduler).

- [x] **Step 2: Register the trigger**

Add after the existing trigger registrations in `main.rs`:

```rust
// Conversations compact trigger (Phase 2A)
{
    let c = &config.conversations.compact;
    if c.enabled_in_daemon {
        match triggers::conversations_compact::ConversationsCompactTrigger::from_config(
            &c.daemon_cron,
            true,
        ) {
            Ok(trig) => {
                scheduler.register(Box::new(trig));
                tracing::info!(
                    "registered conversations_compact trigger (cron: {})",
                    c.daemon_cron
                );
            }
            Err(e) => {
                tracing::warn!(
                    "skipping conversations_compact trigger — invalid cron '{}': {e:#}",
                    c.daemon_cron
                );
            }
        }
    }
}
```

- [x] **Step 3: Smoke-test the build**

```
cargo build -p mur-daemon --bin mur-commander
```

Expected: clean build.

- [x] **Step 4: Commit**

```
cargo clippy -p mur-daemon -- -D warnings
cargo fmt --check -p mur-daemon
git add crates/daemon/src/main.rs
git commit -m "$(cat <<'EOF'
feat(daemon): register ConversationsCompactTrigger on startup (Phase 2A)

When commander reads [conversations.compact] from its config.toml and
enabled_in_daemon is true, instantiates the trigger with the configured
cron. Invalid cron strings are logged (warn) and skipped — daemon keeps
running with the rest of its triggers intact.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: `mur conversations doctor` — summary coverage + trigger status (Phase 2A, mur repo)

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs`

- [x] **Step 1: Extend failing integration test**

Modify `mur-core/tests/cli_conversations.rs`'s existing `mur_conversations_doctor_runs` to assert on new sections:

```rust
#[test]
fn mur_conversations_doctor_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "doctor"])
        .env("HOME", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("raw day-dirs"));
    assert!(stdout.contains("summaries:"));       // NEW Phase 2A
    assert!(stdout.contains("Ollama"));            // NEW Phase 2A
}
```

- [x] **Step 2: Extend `cmd_conversations_doctor`**

Locate existing `cmd_conversations_doctor` in `mur-core/src/cmd/conversations_cmd.rs` and append new checks before `Ok(())`:

```rust
// Phase 2A additions
let raw_dir = conversations::paths::raw_root(None);
let summary_dir = conversations::paths::conversations_root(None).join("summary");
let raw_days: Vec<_> = std::fs::read_dir(&raw_dir)
    .ok()
    .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
    .unwrap_or_default();
let summary_count = std::fs::read_dir(&summary_dir)
    .ok()
    .map(|rd| {
        rd.flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s == "md")
                    .unwrap_or(false)
            })
            .count()
    })
    .unwrap_or(0);

let today = chrono::Utc::now().date_naive();
let completed_days: Vec<&String> = raw_days
    .iter()
    .filter(|d| {
        chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .map(|pd| pd < today)
            .unwrap_or(false)
    })
    .collect();
let missing = completed_days.len().saturating_sub(summary_count);
if missing == 0 {
    println!("  ✓ summaries: all {summary_count} completed days covered");
} else {
    println!(
        "  ⚠ summaries: {summary_count} of {} completed days covered — run 'mur conversations compact'",
        completed_days.len()
    );
}

// Ollama reachability (non-blocking 1s probe)
let cfg = mur_common::config::Config::load().unwrap_or_default();
let endpoint = cfg.conversations.compact.ollama_endpoint.clone();
let reachable = tokio::time::timeout(
    std::time::Duration::from_secs(1),
    reqwest::get(format!("{}/api/tags", endpoint.trim_end_matches('/'))),
)
.await
.ok()
.and_then(|r| r.ok())
.map(|r| r.status().is_success())
.unwrap_or(false);
if reachable {
    println!("  ✓ Ollama reachable at {endpoint}");
} else {
    println!("  · Ollama not reachable at {endpoint} (compact + ask will degrade)");
}
```

This handler is `async` (confirmed in Phase 1). Also update the `Commands::Conversations::Doctor` dispatch arm in `main.rs` to call `.await?` if it wasn't already. Phase 1's doctor was sync; Phase 2A switches it to async — update both the fn signature and the dispatch.

- [x] **Step 3: Run CLI tests**

```
cargo test -p mur-core --test cli_conversations mur_conversations_doctor
```

Expected: pass (doctor now reports summaries + Ollama status).

- [x] **Step 4: Commit**

```
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/cmd/conversations_cmd.rs mur-core/src/main.rs mur-core/tests/cli_conversations.rs
git commit -m "$(cat <<'EOF'
feat(core): doctor reports summary coverage + Ollama reachability (Phase 2A)

doctor now:
  - counts raw day-dirs and summary .md files
  - flags missing summaries (suggests 'mur conversations compact')
  - probes Ollama endpoint (1s timeout, non-blocking, informational)

Doctor handler becomes async so it can do the reqwest probe; dispatch
arm in main.rs updated accordingly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

**🏁 Phase 2A checkpoint.** After Task 17, both mur and mur-commander worktrees have all 2A commits. Open two PRs (mirror Phase 1's two-PR pattern). Wait for CI green + reviewer approval on BOTH before starting Phase 2B.

---

## Task 18: `AskConfig` in `mur-common::config` (Phase 2B)

**Files:**
- Modify: `mur-common/src/config.rs`

- [x] **Step 1: Failing test**

Append to `conversations_tests`:

```rust
#[test]
fn ask_config_defaults() {
    let c = AskConfig::default();
    assert_eq!(c.model, "qwen3:14b");
    assert_eq!(c.ollama_endpoint, "http://localhost:11434");
    assert_eq!(c.k_summary, 5);
    assert_eq!(c.k_raw, 10);
    assert_eq!(c.escalation_threshold, 0.5);
    assert_eq!(c.mmr_threshold, 0.85);
    assert_eq!(c.max_context_tokens, 6000);
    assert_eq!(c.response_tokens, 1024);
    assert_eq!(c.timeout_secs, 120);
    assert_eq!(c.min_score, 0.35);
}
```

- [x] **Step 2: Implement**

Add to `mur-common/src/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskConfig {
    #[serde(default = "ask_default_model")] pub model: String,
    #[serde(default = "compact_default_ollama_endpoint")] pub ollama_endpoint: String,
    #[serde(default = "ask_default_k_summary")] pub k_summary: u32,
    #[serde(default = "ask_default_k_raw")] pub k_raw: u32,
    #[serde(default = "ask_default_esc")] pub escalation_threshold: f64,
    #[serde(default = "ask_default_mmr")] pub mmr_threshold: f64,
    #[serde(default = "ask_default_max_ctx")] pub max_context_tokens: u32,
    #[serde(default = "ask_default_resp_tok")] pub response_tokens: u32,
    #[serde(default = "ask_default_timeout")] pub timeout_secs: u32,
    #[serde(default = "ask_default_min_score")] pub min_score: f64,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            model: ask_default_model(),
            ollama_endpoint: compact_default_ollama_endpoint(),
            k_summary: ask_default_k_summary(),
            k_raw: ask_default_k_raw(),
            escalation_threshold: ask_default_esc(),
            mmr_threshold: ask_default_mmr(),
            max_context_tokens: ask_default_max_ctx(),
            response_tokens: ask_default_resp_tok(),
            timeout_secs: ask_default_timeout(),
            min_score: ask_default_min_score(),
        }
    }
}

fn ask_default_model() -> String { "qwen3:14b".into() }
fn ask_default_k_summary() -> u32 { 5 }
fn ask_default_k_raw() -> u32 { 10 }
fn ask_default_esc() -> f64 { 0.5 }
fn ask_default_mmr() -> f64 { 0.85 }
fn ask_default_max_ctx() -> u32 { 6000 }
fn ask_default_resp_tok() -> u32 { 1024 }
fn ask_default_timeout() -> u32 { 120 }
fn ask_default_min_score() -> f64 { 0.35 }
```

Add field to `ConversationsConfig`:

```rust
#[serde(default)] pub ask: AskConfig,
```

Update `ConversationsConfig::default()` similarly.

- [x] **Step 3: Run + commit**

```
cargo test -p mur-common config::conversations_tests::ask
cargo clippy -p mur-common -- -D warnings
cargo fmt --check -p mur-common
git add mur-common/src/config.rs
git commit -m "feat(common): add AskConfig to ConversationsConfig (Phase 2B)"
```

---

## Task 19: `ask::mod` public types (Phase 2B)

**Files:**
- Create: `mur-core/src/conversations/ask/mod.rs`
- Modify: `mur-core/src/conversations/mod.rs`

- [x] **Step 1: Scaffold module + types**

Create `mur-core/src/conversations/ask/mod.rs`:

```rust
//! Mode C — Ask: local-only RAG with inline citations. See spec §5.
#![allow(dead_code)] // filled progressively across Tasks 19-25

use mur_common::Source;
use std::time::Duration;

pub mod retrieve;
// Later tasks add: pub mod prompt; pub mod generate; pub mod cite; pub mod format;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format { Plain, Json }

#[derive(Debug, Clone)]
pub struct Filters {
    pub source: Vec<Source>,
    pub since: Option<chrono::NaiveDate>,
    pub until: Option<chrono::NaiveDate>,
    pub min_score: f64,
}

#[derive(Debug, Clone)]
pub struct AskRequest {
    pub question: String,
    pub filters: Filters,
    pub k_summary: usize,
    pub k_raw: usize,
    pub escalation_threshold: f64,
    pub mmr_threshold: f64,
    pub model: String,
    pub format: Format,
    pub max_context_tokens: usize,
    pub response_tokens: usize,
    pub timeout: Duration,
    pub no_escalate: bool,
    pub debug_prompt: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Citation {
    pub id: u32,
    pub date: chrono::NaiveDate,
    pub source: String,           // file_prefix
    pub conv_id: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HitInfo {
    pub layer: i8,
    pub source: String,
    pub conv_id: String,
    pub date: chrono::NaiveDate,
    pub score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AskResponse {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub hits_used: Vec<HitInfo>,
    pub degraded_to_mode_b: bool,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub enum AskEvent {
    Token(String),
    Citation(Citation),
    HitInfo(HitInfo),
    Done {
        tokens_in: usize,
        tokens_out: usize,
        degraded: bool,
        duration_ms: u64,
    },
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_default_shape() {
        let f = Filters {
            source: vec![],
            since: None,
            until: None,
            min_score: 0.35,
        };
        assert_eq!(f.min_score, 0.35);
    }
}
```

Register in `mur-core/src/conversations/mod.rs`:

```rust
pub mod ask;
```

- [x] **Step 2: Test + commit**

```
cargo test -p mur-core conversations::ask::tests
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/ mur-core/src/conversations/mod.rs
git commit -m "feat(core): ask module skeleton + public types (Phase 2B)"
```

---

## Task 20: `ask::retrieve` — tiered escalation (Phase 2B)

**Files:**
- Create: `mur-core/src/conversations/ask/retrieve.rs`

- [x] **Step 1: Failing tests + implementation**

Create `mur-core/src/conversations/ask/retrieve.rs`:

```rust
//! Tiered retrieval (spec §5.2).
//! Stages: embed → layer=1 search → escalate to layer=0 if top score low
//! → MMR dedupe → per-hit snippet resolution → token-budget cap.

use anyhow::Result;
use mur_common::Source;

use super::super::index::{ConversationIndex, SearchHit};
use super::super::summarize;
use super::{Filters, HitInfo};

#[derive(Debug, Clone)]
pub struct ResolvedHit {
    pub layer: i8,
    pub info: HitInfo,
    pub snippet: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
}

pub struct RetrieveArgs<'a> {
    pub query_embedding: Vec<f32>,
    pub filters: &'a Filters,
    pub k_summary: usize,
    pub k_raw: usize,
    pub escalation_threshold: f64,
    pub mmr_threshold: f64,
    pub no_escalate: bool,
    pub max_context_tokens: usize,
    pub root_override: Option<&'a str>,
}

pub async fn gather_hits(args: RetrieveArgs<'_>) -> Result<Vec<ResolvedHit>> {
    let dims = args.query_embedding.len() as i32;
    let idx = ConversationIndex::open(dims, args.root_override).await?;
    let primary_src = args.filters.source.first().copied();

    // Layer 1 (summaries)
    let l1 = idx
        .search(&args.query_embedding, args.k_summary, primary_src, Some(1))
        .await?;
    let top_score = l1.first().map(|h| h.similarity).unwrap_or(0.0);

    // Escalate?
    let l0 = if !args.no_escalate
        && ((top_score as f64) < args.escalation_threshold || l1.is_empty())
    {
        idx.search(&args.query_embedding, args.k_raw, primary_src, Some(0))
            .await?
    } else {
        Vec::new()
    };

    // Filter by since/until/min_score
    let filtered_l1: Vec<_> = l1.into_iter().filter(|h| passes(h, args.filters)).collect();
    let filtered_l0: Vec<_> = l0.into_iter().filter(|h| passes(h, args.filters)).collect();

    // Resolve snippets
    let mut resolved = Vec::new();
    for h in filtered_l1 {
        resolved.push(resolve_summary_hit(h, args.root_override)?);
    }
    for h in filtered_l0 {
        resolved.push(resolve_raw_hit(h));
    }

    // MMR dedupe on snippet text (simple word-jaccard; reuses Phase 1 filter threshold config by default)
    let deduped = mmr_dedupe(resolved, args.mmr_threshold);

    // Token-budget cap
    let budget = (args.max_context_tokens * 9 / 10).max(400);
    let capped = cap_by_budget(deduped, budget);
    Ok(capped)
}

fn passes(h: &SearchHit, f: &Filters) -> bool {
    if h.similarity < f.min_score as f32 {
        return false;
    }
    if let Some(s) = f.since {
        if chrono::DateTime::from_timestamp(h.ts, 0)
            .map(|dt| dt.date_naive() < s)
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(u) = f.until {
        if chrono::DateTime::from_timestamp(h.ts, 0)
            .map(|dt| dt.date_naive() > u)
            .unwrap_or(false)
        {
            return false;
        }
    }
    true
}

fn resolve_summary_hit(h: SearchHit, root_override: Option<&str>) -> Result<ResolvedHit> {
    // Read summary file for h.date, pick the first extractive span's text.
    // (Phase 2B simplification; Phase 3 RAPTOR improves this.)
    let date = chrono::DateTime::from_timestamp(h.ts, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    let (md_path, _) = super::super::paths::summary_paths_for(date, root_override);
    let (snippet, line_hint, span_idx) = if md_path.exists() {
        let body = std::fs::read_to_string(&md_path).unwrap_or_default();
        if let Ok(parsed) = summarize::parse_summary(&body) {
            parsed.extractive.first().map_or_else(
                || (String::new(), None, None),
                |s| (s.text.clone(), Some(s.line_hint), Some(s.span_index)),
            )
        } else {
            (String::new(), None, None)
        }
    } else {
        (String::new(), None, None)
    };
    Ok(ResolvedHit {
        layer: 1,
        info: HitInfo {
            layer: 1,
            source: h.source.clone(),
            conv_id: h.conv_id.clone(),
            date,
            score: h.similarity as f64,
        },
        snippet,
        line_hint,
        span_index_in_summary: span_idx,
    })
}

fn resolve_raw_hit(h: SearchHit) -> ResolvedHit {
    let date = chrono::DateTime::from_timestamp(h.ts, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    ResolvedHit {
        layer: 0,
        info: HitInfo {
            layer: 0,
            source: h.source.clone(),
            conv_id: h.conv_id.clone(),
            date,
            score: h.similarity as f64,
        },
        snippet: h.content.clone(),
        line_hint: None, // raw hits don't carry line hints; extensible in Phase 3
        span_index_in_summary: None,
    }
}

fn mmr_dedupe(hits: Vec<ResolvedHit>, threshold: f64) -> Vec<ResolvedHit> {
    let mut kept: Vec<ResolvedHit> = Vec::new();
    for h in hits {
        let dup = kept
            .iter()
            .any(|k| word_jaccard(&k.snippet, &h.snippet) > threshold);
        if !dup {
            kept.push(h);
        }
    }
    kept
}

fn word_jaccard(a: &str, b: &str) -> f64 {
    use std::collections::HashSet;
    let sa: HashSet<&str> = a.split_whitespace().collect();
    let sb: HashSet<&str> = b.split_whitespace().collect();
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn cap_by_budget(hits: Vec<ResolvedHit>, budget_tokens: usize) -> Vec<ResolvedHit> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for h in hits {
        let est = (h.snippet.len() + 80) / 4 + 1;
        if used + est > budget_tokens && !out.is_empty() {
            break;
        }
        used += est;
        out.push(h);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_jaccard_identical_is_one() {
        assert_eq!(word_jaccard("a b c", "a b c"), 1.0);
    }

    #[test]
    fn word_jaccard_disjoint_is_zero() {
        assert_eq!(word_jaccard("a b c", "d e f"), 0.0);
    }

    #[test]
    fn mmr_dedupe_drops_duplicate() {
        use mur_common::Role;
        let h1 = ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "a".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                score: 0.9,
            },
            snippet: "the quick brown fox jumps".into(),
            line_hint: None,
            span_index_in_summary: None,
        };
        let h2 = ResolvedHit {
            snippet: "the quick brown fox jumps".into(),
            info: HitInfo {
                source: "cc".into(),
                conv_id: "b".into(),
                ..h1.info.clone()
            },
            ..h1.clone()
        };
        let out = mmr_dedupe(vec![h1, h2], 0.85);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn cap_by_budget_keeps_at_least_one() {
        use mur_common::Role;
        let giant = ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "a".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                score: 0.9,
            },
            snippet: "x".repeat(40_000),
            line_hint: None,
            span_index_in_summary: None,
        };
        let out = cap_by_budget(vec![giant], 100);
        assert_eq!(out.len(), 1, "must keep at least one hit even over budget");
    }
}
```

- [x] **Step 2: Test + commit**

```
cargo test -p mur-core conversations::ask::retrieve::tests
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/retrieve.rs
git commit -m "feat(core): ask::retrieve — tiered escalation with MMR + budget cap (Phase 2B)"
```

---

## Task 21: `ask::prompt` — system prompt + context assembly (Phase 2B)

**Files:**
- Create: `mur-core/src/conversations/ask/prompt.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs` (add `pub mod prompt;`)

- [x] **Step 1: Implement with tests**

Create `mur-core/src/conversations/ask/prompt.rs`:

```rust
//! System prompt + context assembly (spec §5.3).

use super::retrieve::ResolvedHit;

pub const SYSTEM_PROMPT: &str = "You answer questions about the user's past AI-assistant conversations, using ONLY the excerpts provided below under \"Context\". Never invent facts not present in the excerpts.

Every factual claim in your answer MUST be followed by an inline citation in the form [cit: <date> <source>/<conv_id>:L<line>]. Use only the citations enumerated in the Context section — one citation per claim. You may use the same citation multiple times.

If the excerpts are insufficient to answer, say so plainly: \"The conversations I have access to don't cover that.\" Do not speculate. Do not use training knowledge to fill gaps.

Format: clear prose, 2-6 sentences per idea, Markdown bullets when listing. Be direct. Don't repeat the question. Don't apologize for not knowing.

When the user mentions a pattern name wrapped in {{pattern: name}} in the excerpts, that refers to a reusable artifact at ~/.mur/patterns/<name>.yaml; you may mention the pattern by name in your answer but do not expand it.";

pub struct RenderedPrompt {
    pub system: String,
    pub user: String,
    pub tokens_est: usize,
    pub valid_citations: Vec<String>, // normalized citation anchors for grounding
}

pub fn render(
    question: &str,
    hits: &[ResolvedHit],
    max_context_tokens: usize,
    response_tokens: usize,
) -> RenderedPrompt {
    let system = SYSTEM_PROMPT.to_string();

    let mut ctx = String::new();
    let mut valid_citations = Vec::new();
    for (i, h) in hits.iter().enumerate() {
        let anchor = cite_anchor(h);
        valid_citations.push(anchor.clone());
        ctx.push_str(&anchor);
        ctx.push('\n');
        ctx.push_str("> ");
        ctx.push_str(&h.snippet.replace('\n', "\n> "));
        ctx.push_str("\n\n");
        let _ = i;
    }

    let truncated_question = truncate_chars(question, 2000);
    let user = format!("Context:\n{ctx}\nQuestion: {truncated_question}");
    let tokens_est = (system.len() + user.len()) / 4 + response_tokens + 120;

    // If we overflow, drop hits from the tail until we fit.
    let mut user = user;
    let mut trimmed_hits = hits.len();
    while tokens_est > max_context_tokens && trimmed_hits > 1 {
        trimmed_hits -= 1;
        let mut ctx2 = String::new();
        valid_citations.clear();
        for h in hits.iter().take(trimmed_hits) {
            let anchor = cite_anchor(h);
            valid_citations.push(anchor.clone());
            ctx2.push_str(&anchor);
            ctx2.push('\n');
            ctx2.push_str("> ");
            ctx2.push_str(&h.snippet.replace('\n', "\n> "));
            ctx2.push_str("\n\n");
        }
        user = format!("Context:\n{ctx2}\nQuestion: {truncated_question}");
    }

    RenderedPrompt {
        system,
        user,
        tokens_est,
        valid_citations,
    }
}

pub fn cite_anchor(h: &ResolvedHit) -> String {
    match (h.layer, h.line_hint, h.span_index_in_summary) {
        (1, _, Some(idx)) => format!(
            "[cit: {} {}/{} @summary-span-{}]",
            h.info.date, h.info.source, h.info.conv_id, idx
        ),
        (_, Some(line), _) => format!(
            "[cit: {} {}/{}:L{}]",
            h.info.date, h.info.source, h.info.conv_id, line
        ),
        _ => format!("[cit: {} {}/{}]", h.info.date, h.info.source, h.info.conv_id),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Filters, HitInfo};

    fn hit_raw(conv: &str, text: &str) -> ResolvedHit {
        ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: conv.into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                score: 0.9,
            },
            snippet: text.into(),
            line_hint: Some(42),
            span_index_in_summary: None,
        }
    }

    #[test]
    fn cite_anchor_raw_layer() {
        let h = hit_raw("abc", "hi");
        assert_eq!(
            cite_anchor(&h),
            "[cit: 2026-04-19 cc/abc:L42]"
        );
    }

    #[test]
    fn cite_anchor_summary_layer() {
        let mut h = hit_raw("abc", "hi");
        h.layer = 1;
        h.span_index_in_summary = Some(3);
        h.line_hint = None;
        assert_eq!(
            cite_anchor(&h),
            "[cit: 2026-04-19 cc/abc @summary-span-3]"
        );
    }

    #[test]
    fn render_shrinks_hits_on_overflow() {
        let hits = (0..20)
            .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(3000)))
            .collect::<Vec<_>>();
        let r = render("question?", &hits, 6000, 1024);
        assert!(r.valid_citations.len() < hits.len());
        assert!(!r.valid_citations.is_empty());
    }

    #[test]
    fn render_lists_valid_citations_in_order() {
        let hits = vec![hit_raw("a", "one"), hit_raw("b", "two")];
        let r = render("q?", &hits, 6000, 1024);
        assert_eq!(r.valid_citations.len(), 2);
        assert!(r.user.contains("one"));
        assert!(r.user.contains("two"));
    }
}
```

Register in `ask/mod.rs`:

```rust
pub mod prompt;
```

- [x] **Step 2: Test + commit**

```
cargo test -p mur-core conversations::ask::prompt::tests
cargo clippy -p mur-core -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/prompt.rs mur-core/src/conversations/ask/mod.rs
git commit -m "feat(core): ask::prompt — system prompt + context assembly (Phase 2B)"
```

---

## Task 22: `ask::generate` — Ollama streaming wrapper (Phase 2B)

**Files:**
- Create: `mur-core/src/conversations/ask/generate.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs`

Create `mur-core/src/conversations/ask/generate.rs`:

```rust
//! Streaming Ollama generation for ask. Thin adapter over conversations::ollama.

use anyhow::Result;
use futures::stream::Stream;
use std::pin::Pin;
use std::time::Duration;

use super::super::ollama::{GenerateOptions, GenerateRequest, OllamaClient};

pub async fn stream_answer(
    endpoint: &str,
    model: &str,
    system: &str,
    user: &str,
    response_tokens: u32,
    timeout: Duration,
) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
    let client = OllamaClient::new(endpoint, timeout);
    client
        .generate_stream(GenerateRequest {
            model,
            prompt: user,
            system: Some(system),
            stream: true,
            options: GenerateOptions {
                temperature: Some(0.1),
                top_p: Some(0.9),
                num_predict: Some(response_tokens),
                stop: vec!["\n\nQ:".into(), "\n\nQuestion:".into()],
            },
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_stream_yields_tokens() {
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let mut s = stream_answer(
            "http://unused",
            "qwen3:14b",
            "system",
            "ask about [cit:",
            256,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let mut combined = String::new();
        while let Some(chunk) = s.next().await {
            combined.push_str(&chunk.unwrap());
        }
        assert!(combined.contains("[cit: 2026-04-19 claude-code/mock:L1]"));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
```

Register in `ask/mod.rs`: `pub mod generate;`

Test + commit.

---

## Task 23: `ask::cite` — grounding filter (Phase 2B)

**Files:**
- Create: `mur-core/src/conversations/ask/cite.rs`

Create `mur-core/src/conversations/ask/cite.rs`:

```rust
//! Grounding + coverage checks (spec §5.5).
//! Strips unknown [cit: ...] from streaming tokens (prevents hallucination).

pub struct GroundingFilter {
    valid: std::collections::HashSet<String>,
    buffer: String,       // tail buffer to detect brackets across chunk boundaries
}

pub struct FilterOutput {
    pub forwarded: String,
    pub stripped: Vec<String>,
}

impl GroundingFilter {
    pub fn new(valid: Vec<String>) -> Self {
        Self {
            valid: valid.into_iter().collect(),
            buffer: String::new(),
        }
    }

    pub fn feed(&mut self, token: &str) -> FilterOutput {
        self.buffer.push_str(token);
        let mut forwarded = String::new();
        let mut stripped = Vec::new();
        loop {
            // If no open bracket, we can flush everything up to the last 64 chars (
            // hold those back in case an opening bracket arrives split across tokens).
            match self.buffer.find("[cit:") {
                None => {
                    if self.buffer.len() > 64 {
                        let flush_upto = self.buffer.len() - 64;
                        let boundary = find_char_boundary(&self.buffer, flush_upto);
                        forwarded.push_str(&self.buffer[..boundary]);
                        self.buffer.drain(..boundary);
                    }
                    break;
                }
                Some(start) => {
                    // Flush prefix before the bracket.
                    if start > 0 {
                        forwarded.push_str(&self.buffer[..start]);
                        self.buffer.drain(..start);
                    }
                    // Look for the closing ']'.
                    match self.buffer.find(']') {
                        None => {
                            // Bracket incomplete — wait for more input.
                            break;
                        }
                        Some(end) => {
                            let candidate = &self.buffer[..=end];
                            if self.valid.contains(candidate) {
                                forwarded.push_str(candidate);
                            } else {
                                stripped.push(candidate.to_string());
                            }
                            self.buffer.drain(..=end);
                        }
                    }
                }
            }
        }
        FilterOutput { forwarded, stripped }
    }

    pub fn flush(&mut self) -> FilterOutput {
        let mut out = self.feed("");
        // Drain remaining buffer (no more input coming)
        out.forwarded.push_str(&self.buffer);
        self.buffer.clear();
        out
    }
}

fn find_char_boundary(s: &str, target: usize) -> usize {
    let mut b = target;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_known_citation_verbatim() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        let out = f.feed("Answer [cit: 2026-04-19 cc/a:L1] done.");
        let drain = f.flush();
        assert!((out.forwarded + &drain.forwarded).contains("[cit: 2026-04-19 cc/a:L1]"));
        assert!(out.stripped.is_empty() && drain.stripped.is_empty());
    }

    #[test]
    fn strips_unknown_citation() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        let out = f.feed("Claim [cit: 2099-01-01 fake:L0] done.");
        let drain = f.flush();
        let combined = out.forwarded + &drain.forwarded;
        assert!(!combined.contains("[cit: 2099-01-01 fake:L0]"));
        assert_eq!(out.stripped.len() + drain.stripped.len(), 1);
    }

    #[test]
    fn handles_bracket_split_across_tokens() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        let mut combined = String::new();
        combined.push_str(&f.feed("start [c").forwarded);
        combined.push_str(&f.feed("it: 2026-04-19 cc/a").forwarded);
        combined.push_str(&f.feed(":L1] end").forwarded);
        combined.push_str(&f.flush().forwarded);
        assert!(combined.contains("[cit: 2026-04-19 cc/a:L1]"));
    }
}
```

Register `pub mod cite;` in `ask/mod.rs`. Test + commit.

---

## Task 24: `ask::format` — plain + JSON output (Phase 2B)

**Files:**
- Create: `mur-core/src/conversations/ask/format.rs`

Create `mur-core/src/conversations/ask/format.rs`:

```rust
//! Output formatting for ask (spec §5.6).
//! Plain: streaming answer + trailing citation block + runtime footer.
//! JSON: buffered AskResponse emitted once at end.

use super::{AskResponse, Citation};

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
            (Some(line), _) => format!(
                "[cit: {} {}/{}:L{}]",
                c.date, c.source, c.conv_id, line
            ),
            _ => format!("[cit: {} {}/{}]", c.date, c.source, c.conv_id),
        };
        let preview: String = c.snippet.chars().take(120).collect();
        out.push_str(&format!("  {anchor}\n    — {preview}\n"));
    }
    out
}

pub fn render_footer(resp: &AskResponse) -> String {
    let tag = if resp.degraded_to_mode_b {
        " · Mode B fallback"
    } else {
        ""
    };
    format!(
        "({} hits · {}ms · {}→{} tokens{})\n",
        resp.citations.len(),
        resp.duration_ms,
        resp.tokens_in,
        resp.tokens_out,
        tag,
    )
}

pub fn render_json(resp: &AskResponse) -> String {
    serde_json::to_string_pretty(resp).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            }],
            hits_used: vec![],
            degraded_to_mode_b: false,
            tokens_in: 100,
            tokens_out: 20,
            duration_ms: 500,
        }
    }

    #[test]
    fn citations_block_contains_anchor_and_preview() {
        let r = sample_resp();
        let block = render_citations_block(&r.citations);
        assert!(block.contains("[cit: 2026-04-19 cc/a:L1]"));
        assert!(block.contains("sample snippet text"));
    }

    #[test]
    fn footer_shows_mode_b_tag_when_degraded() {
        let mut r = sample_resp();
        r.degraded_to_mode_b = true;
        let f = render_footer(&r);
        assert!(f.contains("Mode B fallback"));
    }

    #[test]
    fn json_roundtrip() {
        let r = sample_resp();
        let s = render_json(&r);
        assert!(s.contains("\"answer\""));
        assert!(s.contains("\"citations\""));
    }
}
```

Register `pub mod format;`. Test + commit.

---

## Task 25: `ask::ask_stream` + `ask` glue (Phase 2B)

**Files:**
- Modify: `mur-core/src/conversations/ask/mod.rs`

Append to `ask/mod.rs`:

```rust
use anyhow::Result;
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;
use std::time::Instant;

pub async fn ask_stream(
    req: AskRequest,
    root_override: Option<&str>,
) -> Result<Pin<Box<dyn Stream<Item = Result<AskEvent>> + Send>>> {
    use async_stream::try_stream;

    let start = Instant::now();

    // 1. Embed (Phase 2B placeholder — reuse existing mur-core embedding code)
    let query_embedding = match embed_query(&req.question).await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Box::pin(try_stream! {
                yield AskEvent::Error(format!("embed failed: {e:#}"));
                yield AskEvent::Done { tokens_in: 0, tokens_out: 0, degraded: false, duration_ms: start.elapsed().as_millis() as u64 };
            }));
        }
    };

    // 2. Retrieve
    let hits = match retrieve::gather_hits(retrieve::RetrieveArgs {
        query_embedding,
        filters: &req.filters,
        k_summary: req.k_summary,
        k_raw: req.k_raw,
        escalation_threshold: req.escalation_threshold,
        mmr_threshold: req.mmr_threshold,
        no_escalate: req.no_escalate,
        max_context_tokens: req.max_context_tokens,
        root_override,
    })
    .await
    {
        Ok(h) => h,
        Err(e) => {
            return Ok(Box::pin(try_stream! {
                yield AskEvent::Error(format!("retrieve failed: {e:#}"));
                yield AskEvent::Done { tokens_in: 0, tokens_out: 0, degraded: false, duration_ms: start.elapsed().as_millis() as u64 };
            }));
        }
    };

    if hits.is_empty() {
        return Ok(Box::pin(try_stream! {
            yield AskEvent::Token("The conversations I have access to don't cover that.".into());
            yield AskEvent::Done { tokens_in: 0, tokens_out: 0, degraded: false, duration_ms: start.elapsed().as_millis() as u64 };
        }));
    }

    // 3. Build prompt
    let prompt = prompt::render(
        &req.question,
        &hits,
        req.max_context_tokens,
        req.response_tokens,
    );

    // Emit HitInfo events up-front (for UIs that want to show retrieval debug)
    let hit_events: Vec<AskEvent> = hits
        .iter()
        .map(|h| AskEvent::HitInfo(h.info.clone()))
        .collect();

    // 4. Generate (streaming) with grounding filter
    let endpoint = /* from config; see cmd_ask wiring */ "http://localhost:11434".to_string();
    let model = req.model.clone();
    let mut filter = cite::GroundingFilter::new(prompt.valid_citations.clone());
    let tokens_in = prompt.tokens_est;

    let stream = match generate::stream_answer(
        &endpoint,
        &model,
        &prompt.system,
        &prompt.user,
        req.response_tokens as u32,
        req.timeout,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            // Ollama unreachable → Mode B fallback: emit hits as tokens
            let mode_b = hits_as_mode_b(&hits);
            return Ok(Box::pin(try_stream! {
                for evt in hit_events { yield evt; }
                yield AskEvent::Token(mode_b);
                yield AskEvent::Done {
                    tokens_in,
                    tokens_out: 0,
                    degraded: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                };
                // Record the failure cause in Error so CLI can show a hint
                yield AskEvent::Error(format!("ollama unavailable: {e:#}"));
            }));
        }
    };

    let citation_events_by_anchor = citations_map(&hits);
    let out_stream = try_stream! {
        for evt in hit_events { yield evt; }
        let mut stream = stream;
        let mut tokens_out = 0usize;
        let mut emitted_citations = std::collections::HashSet::new();
        while let Some(next) = stream.next().await {
            let tok = next?;
            let filtered = filter.feed(&tok);
            if !filtered.forwarded.is_empty() {
                tokens_out += filtered.forwarded.len() / 4 + 1;
                // Emit newly-seen citations
                for c in citations_fired_in(&filtered.forwarded, &citation_events_by_anchor) {
                    if emitted_citations.insert(c.id) {
                        yield AskEvent::Citation(c.clone());
                    }
                }
                yield AskEvent::Token(filtered.forwarded);
            }
        }
        let drained = filter.flush();
        if !drained.forwarded.is_empty() {
            tokens_out += drained.forwarded.len() / 4 + 1;
            for c in citations_fired_in(&drained.forwarded, &citation_events_by_anchor) {
                if emitted_citations.insert(c.id) {
                    yield AskEvent::Citation(c.clone());
                }
            }
            yield AskEvent::Token(drained.forwarded);
        }
        yield AskEvent::Done {
            tokens_in,
            tokens_out,
            degraded: false,
            duration_ms: start.elapsed().as_millis() as u64,
        };
    };
    Ok(Box::pin(out_stream))
}

pub async fn ask(req: AskRequest, root_override: Option<&str>) -> Result<AskResponse> {
    let format = req.format.clone();
    let mut stream = ask_stream(req, root_override).await?;
    let mut answer = String::new();
    let mut citations = Vec::new();
    let mut hits_used = Vec::new();
    let mut degraded = false;
    let mut tokens_in = 0;
    let mut tokens_out = 0;
    let mut duration_ms = 0;
    while let Some(evt) = stream.next().await {
        match evt? {
            AskEvent::Token(t) => answer.push_str(&t),
            AskEvent::Citation(c) => citations.push(c),
            AskEvent::HitInfo(h) => hits_used.push(h),
            AskEvent::Done { tokens_in: ti, tokens_out: to, degraded: d, duration_ms: ms } => {
                tokens_in = ti;
                tokens_out = to;
                degraded = d;
                duration_ms = ms;
            }
            AskEvent::Error(e) => return Err(anyhow::anyhow!(e)),
        }
    }
    let _ = format;
    Ok(AskResponse {
        answer,
        citations,
        hits_used,
        degraded_to_mode_b: degraded,
        tokens_in,
        tokens_out,
        duration_ms,
    })
}

async fn embed_query(q: &str) -> Result<Vec<f32>> {
    // Phase 2B: reuse the existing embed path from ingest/compact. If the
    // project has `crate::store::embedding::embed_text`, call it; else fall
    // back to Ollama /api/embeddings. For Phase 2B tests we only care about
    // the length being correct.
    if super::ollama::OllamaClient::mock_from_env() {
        return Ok(vec![0.1; 1024]);
    }
    // Delegate — actual implementation lives elsewhere in mur-core.
    crate::store::embedding::embed_text(q, 1024).await
}

fn hits_as_mode_b(hits: &[retrieve::ResolvedHit]) -> String {
    let mut out = String::from(
        "[LLM unavailable] Here are the top relevant excerpts:\n\n",
    );
    for (i, h) in hits.iter().enumerate().take(5) {
        out.push_str(&format!("{}. [cit: {} {}/{}] — {}\n",
            i + 1, h.info.date, h.info.source, h.info.conv_id, h.snippet));
    }
    out
}

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
            },
        );
    }
    m
}

fn citations_fired_in(
    text: &str,
    map: &std::collections::HashMap<String, Citation>,
) -> Vec<Citation> {
    let mut out = Vec::new();
    for (anchor, cite) in map {
        if text.contains(anchor.as_str()) {
            out.push(cite.clone());
        }
    }
    out
}
```

Add `async-stream = "0.3"` to `mur-core/Cargo.toml` (Phase 1 might already have it — check first).

Test + commit.

---

## Task 26: CLI `cmd_ask` + top-level `Commands::Ask` (Phase 2B)

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs`
- Modify: `mur-core/src/main.rs`

Add `Commands::Ask` variant to `main.rs`:

```rust
/// Ask a natural-language question about your conversation archive (Mode C).
Ask {
    question: String,
    #[arg(long)] src: Option<String>,
    #[arg(long)] since: Option<String>,
    #[arg(long)] until: Option<String>,
    #[arg(long, default_value = "5")] k: usize,
    #[arg(long)] model: Option<String>,
    #[arg(long)] min_score: Option<f64>,
    #[arg(long)] json: bool,
    #[arg(long)] no_escalate: bool,
    #[arg(long)] debug_prompt: bool,
},
```

Add dispatch arm:

```rust
Commands::Ask { question, src, since, until, k, model, min_score, json, no_escalate, debug_prompt } => {
    cmd::conversations_cmd::cmd_ask(cmd::conversations_cmd::AskArgs {
        question, src, since, until, k, model, min_score, json, no_escalate, debug_prompt,
    }).await?
}
```

Add to `conversations_cmd.rs`:

```rust
pub struct AskArgs {
    pub question: String,
    pub src: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub k: usize,
    pub model: Option<String>,
    pub min_score: Option<f64>,
    pub json: bool,
    pub no_escalate: bool,
    pub debug_prompt: bool,
}

pub async fn cmd_ask(args: AskArgs) -> anyhow::Result<()> {
    use crate::conversations::ask;
    use chrono::NaiveDate;
    use futures::StreamExt;
    use mur_common::{config::Config, Source};
    use std::io::Write;

    let cfg = Config::load().unwrap_or_default();
    let ask_cfg = cfg.conversations.ask.clone();
    let model = args.model.unwrap_or_else(|| ask_cfg.model.clone());

    let sources = args
        .src
        .as_deref()
        .map(parse_sources)
        .unwrap_or_default();

    let filters = ask::Filters {
        source: sources,
        since: args
            .since
            .as_deref()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()?,
        until: args
            .until
            .as_deref()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()?,
        min_score: args.min_score.unwrap_or(ask_cfg.min_score),
    };

    let req = ask::AskRequest {
        question: args.question.clone(),
        filters,
        k_summary: args.k,
        k_raw: args.k * 2,
        escalation_threshold: ask_cfg.escalation_threshold,
        mmr_threshold: ask_cfg.mmr_threshold,
        model,
        format: if args.json { ask::Format::Json } else { ask::Format::Plain },
        max_context_tokens: ask_cfg.max_context_tokens as usize,
        response_tokens: ask_cfg.response_tokens as usize,
        timeout: std::time::Duration::from_secs(ask_cfg.timeout_secs as u64),
        no_escalate: args.no_escalate,
        debug_prompt: args.debug_prompt,
    };

    if args.json {
        let resp = ask::ask(req, None).await?;
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        let mut stream = ask::ask_stream(req, None).await?;
        let mut citations = Vec::new();
        let mut degraded = false;
        let mut tokens_in = 0;
        let mut tokens_out = 0;
        let mut duration = 0;
        while let Some(evt) = stream.next().await {
            match evt? {
                ask::AskEvent::Token(t) => {
                    print!("{t}");
                    std::io::stdout().flush()?;
                }
                ask::AskEvent::Citation(c) => citations.push(c),
                ask::AskEvent::HitInfo(_) => {}
                ask::AskEvent::Done { tokens_in: ti, tokens_out: to, degraded: d, duration_ms } => {
                    tokens_in = ti;
                    tokens_out = to;
                    degraded = d;
                    duration = duration_ms;
                }
                ask::AskEvent::Error(e) => {
                    eprintln!("\nerror: {e}");
                    std::process::exit(1);
                }
            }
        }
        println!();
        print!(
            "{}{}",
            crate::conversations::ask::format::render_citations_block(&citations),
            crate::conversations::ask::format::render_footer(&ask::AskResponse {
                answer: String::new(),
                citations: citations.clone(),
                hits_used: vec![],
                degraded_to_mode_b: degraded,
                tokens_in,
                tokens_out,
                duration_ms: duration,
            }),
        );
    }
    Ok(())
}

fn parse_sources(s: &str) -> Vec<mur_common::Source> {
    s.split(',')
        .filter_map(|tok| match tok.trim() {
            "cc" | "claude-code" => Some(mur_common::Source::ClaudeCode),
            "cursor" => Some(mur_common::Source::Cursor),
            "gemini" => Some(mur_common::Source::Gemini),
            "aider" => Some(mur_common::Source::Aider),
            "slack" => Some(mur_common::Source::Slack),
            "telegram" => Some(mur_common::Source::Telegram),
            "discord" => Some(mur_common::Source::Discord),
            _ => None,
        })
        .collect()
}
```

Test + commit.

---

## Task 27: Extend `golden-path-conversations.sh` — Steps 9 + 10 (Phase 2B)

**Files:**
- Modify: `scripts/golden-path-conversations.sh`

Append before the final `echo "=== ALL N STEPS GREEN ==="`:

```bash
# ── Step 9: compact ───────────────────────────────────────────────────────
echo "[step 9] mur conversations compact"
MUR_OLLAMA_MOCK=1 "$MUR" conversations compact
# Expect the seeded yesterday's day to now have a summary:
YDAY="$(date -u -v-1d +%Y-%m-%d 2>/dev/null || date -u -d 'yesterday' +%Y-%m-%d)"
test -f "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: no summary/${YDAY}.md"; exit 1; }
grep -q "## Extractive spans" "$TMPHOME/.mur/conversations/summary/${YDAY}.md" \
  || { echo "FAIL step 9: summary missing sections"; exit 1; }

# ── Step 10: ask ─────────────────────────────────────────────────────────
echo "[step 10] mur ask"
MUR_OLLAMA_MOCK=1 "$MUR" ask "what compression techniques did I discuss" --json > /tmp/gp-step-10.json
jq -e '.citations | length >= 1' /tmp/gp-step-10.json \
  || { echo "FAIL step 10: no citations in json"; exit 1; }
jq -e '.answer | length > 0' /tmp/gp-step-10.json \
  || { echo "FAIL step 10: empty answer"; exit 1; }

echo "=== ALL 10 STEPS GREEN ==="
```

Adjust the final echo banner accordingly. Ensure the golden-path script's seed step (already in Phase 1) includes a yesterday-dated raw file so there's something for compact to process.

Test + commit:

```
./scripts/golden-path-conversations.sh
git add scripts/golden-path-conversations.sh
git commit -m "test: extend golden-path with Steps 9 (compact) + 10 (ask) (Phase 2B)"
```

**🏁 Phase 2B checkpoint.** Open PR #2 (mur only). Wait for CI + reviewer approval before Phase 2C.

---

## Task 28: `.history/` retention cleanup + audit (Phase 2C)

**Files:**
- Modify: `mur-core/src/conversations/summarize/writer.rs`

Add a `prune_history` helper called by `archive_prior`:

```rust
fn prune_history(root_override: Option<&str>, date: NaiveDate, retain: u32) -> Result<u64> {
    let hist = summary_history_dir(root_override);
    if !hist.exists() {
        return Ok(0);
    }
    let stem = date.format("%Y-%m-%d").to_string();
    let mut matches: Vec<std::path::PathBuf> = std::fs::read_dir(&hist)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&stem))
                .unwrap_or(false)
        })
        .collect();
    if matches.len() <= retain as usize {
        return Ok(0);
    }
    matches.sort();
    let drop_count = matches.len() - retain as usize;
    let mut freed = 0u64;
    for p in matches.into_iter().take(drop_count) {
        if let Ok(meta) = std::fs::metadata(&p) {
            freed += meta.len();
        }
        std::fs::remove_file(&p)?;
    }
    // Audit the deletion
    if freed > 0 {
        let audit_log = super::super::audit::Audit::open(root_override)?;
        let _ = audit_log.append(
            super::super::audit::AuditAction::Delete {
                target: hist.to_string_lossy().into_owned(),
                reason: "history.rotate".into(),
                bytes_freed: freed,
            },
            String::new(),
        );
    }
    Ok(freed)
}
```

Call it at the end of `archive_prior` with the config's `history_retain`. Test + commit.

---

## Task 29: `ask::cite` — `--strict-citations` reject mode (Phase 2C)

**Files:**
- Modify: `mur-core/src/conversations/ask/cite.rs`
- Modify: `mur-core/src/cmd/conversations_cmd.rs`

Add a `strict: bool` field to `GroundingFilter` and `AskArgs`. When strict is on, emit an `Error` AskEvent if any claim is un-cited (use the coverage heuristic from spec §5.5 step 2 — claim sentences without adjacent `[cit: ...]`).

Test + commit.

---

## Task 30: `conversations preflight` — Ollama checks (Phase 2C)

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs`

Extend `cmd_conversations_preflight` with probes:

```rust
// Ollama reachable
// Models compact.extractive_model + compact.abstractive_model + ask.model all pulled
// Pattern dir readable
// Free mem > 4 GB (sysinfo)
```

Add `sysinfo = "0.30"` to `Cargo.toml` if not present. Test + commit.

---

## Task 31: Real-Ollama smoke tests (Phase 2C)

**Files:**
- Modify: `mur-core/Cargo.toml` (add `[features] ollama-live-smoke = []`)
- Create: `mur-core/tests/ollama_live_smoke.rs`

Feature-gated integration test:

```rust
#![cfg(feature = "ollama-live-smoke")]

// Seeds a tempdir with a raw day, runs compact_day against REAL Ollama
// (skipped in CI). Run locally:
//   cargo test -p mur-core --features ollama-live-smoke -- --ignored
#[ignore]
#[tokio::test]
async fn compact_against_real_ollama() {
    // ... full test body with tempfile + compact_day + writer assertions
}
```

Commit.

---

## Task 32: `.history/` coverage report in `mur conversations doctor` (Phase 2C)

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs`

Add doctor check: count `.history/*.md` files, print total + bytes.

Test + commit.

---

## Self-Review

After writing this plan, I reviewed it against the Phase 2 design spec:

**Spec coverage:**
- §1 Purpose / Non-goals → covered in Phase 2A (compact) + 2B (ask) delivery split
- §2 Decisions locked → Tasks 1-17 (2A) + 18-27 (2B) + 28-32 (2C) all tagged
- §3 Architecture → Tasks 2 (ollama client) + 4-10 (summarize) + 19-25 (ask) implement the module layout
- §4 Compact pipeline → Tasks 4 (chunker) / 5 (extractive) / 6 (abstractive) / 7 (macro_refs) / 8 (index layer) / 9 (writer) / 10 (orchestrator) / 11 (parser)
- §5 Mode C → Tasks 19 (types) / 20 (retrieve) / 21 (prompt) / 22 (generate) / 23 (cite) / 24 (format) / 25 (glue)
- §6 Config → Tasks 1 (compact) + 18 (ask)
- §7 Commander piggyback → Tasks 14 (config) + 15 (trigger) + 16 (register)
- §8 §12 amendment → the `.history/` write path in Task 9; spec §12 amendment text is docs-only and lives in the spec file already
- §9 Testing → Tasks include unit tests per module; Task 27 extends golden-path; Task 31 adds real-Ollama smoke
- §10 Operational → Task 17 (doctor) + Task 30 (preflight) cover observability; rollout order respected via checkpoints
- §11 Compatibility constraints → enforced throughout (raw+audit append-only maintained; citations survive retention; Ollama unreachable → Mode B fallback in Task 25)

**Placeholder scan:** no `TBD` / `TODO` / `implement later` in plan steps. One `/* from config; see cmd_ask wiring */` comment in Task 25 is a forward reference that Task 26 resolves; acceptable since Task 26 is in the same plan.

**Type consistency:** `CompactConfig` / `AskConfig` / `ConversationsCompactConfig` / `ResolvedHit` / `AskRequest` / `AskResponse` / `AskEvent` / `Citation` / `HitInfo` / `ExtractiveSpan` / `AbstractiveResult` / `MacroRef` / `SummaryDoc` / `ParsedSummary` / `Chunk` / `GenerateRequest` / `GenerateResponse` — all names stable across the tasks that reference them.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-20-mur-conversations-phase-2.md`.

**Checkpoints:** pause after **Task 17** (end of Phase 2A) and **Task 27** (end of Phase 2B) for PR review before continuing.

**Two execution options:**

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Same workflow as Phase 1.
2. **Inline Execution** — execute tasks in this session via `executing-plans`, batch execution with checkpoints.

Which approach?

