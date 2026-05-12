# Cloud LLM Backend P0 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Introduce a `ChatBackend` trait inside `mur-core/src/conversations/backend/` with `OllamaBackend`, `MockBackend`, and `factory::build`, then refactor `ask::rewriter` to use it as the canary call site. **No user-visible behavior change** — all existing tests must pass byte-identically.

**Architecture:** New `backend` module with a trait + 2 impls. `OllamaBackend` wraps the existing `OllamaClient`. `MockBackend` reuses the pattern-matching logic from `ollama.rs::mock_generate`. `factory::build` selects backend from `BackendConfig` (or env var for mock). `ask::rewriter::rewrite` is migrated from `&OllamaClient` to `&dyn ChatBackend` as proof the trait is right; remaining 9+ call sites stay on `OllamaClient` until P1.

**Tech Stack:** Rust 2024 · `async-trait` (already in workspace deps via tonic transitive — verify in Task 0) · `tokio` · `tracing` · `anyhow` for application errors · `thiserror` for `BackendError` at the trait boundary. No new crates if `async-trait` is already available; otherwise +1 dep.

**Spec:** `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` §3 (architecture), §4 (trait), §5.1 (OllamaBackend), §5.3 (MockBackend), §5.4 (factory). P1 (Anthropic backend, streaming, `BackendConfig` schema, doctor enhancements) is out of scope here.

**Out of scope for P0:**
- `AnthropicBackend` — P1
- Streaming method on the trait — P2 (`generate_stream` returns `unimplemented!()` for OllamaBackend in P0; only the non-streaming path is exercised by `ask::rewriter`)
- Retry envelope — P1 (Ollama + Mock don't need it; lift from `extract_llm.rs` when cloud lands)
- Per-stage `BackendConfig` schema in `mur-common/src/config.rs` — P1
- Migrating `compact`, `ask::generate`, `ask::abstractive`, `summarize::*` — P1/P2
- Prompt caching, cost telemetry — P3

---

## Task 0: Verify dependencies and read existing code

**Files:**
- Read: `Cargo.toml` (workspace root), `mur-core/Cargo.toml`
- Read: `mur-core/src/conversations/ollama.rs` (full file — 600+ lines)
- Read: `mur-core/src/conversations/ask/rewriter.rs` (full file — 217 lines)
- Read: `mur-core/src/conversations/mod.rs` (to confirm `ENV_LOCK` and module structure)

**Step 1: Check whether `async-trait` is already a workspace dep**

Run:
```bash
grep -rn "async-trait\|async_trait" /Users/david/Projects/mur/Cargo.toml /Users/david/Projects/mur/mur-core/Cargo.toml /Users/david/Projects/mur/mur-common/Cargo.toml
```

If `async-trait` is missing, add it to `mur-core/Cargo.toml` `[dependencies]` as `async-trait = "0.1"`. If already present (likely via a transitive workspace member), reuse.

**Step 2: Confirm `tokio` is in `mur-core/Cargo.toml`**

It is — tracing, anyhow, thiserror are all there too. Just confirm by `grep` so a missing dep doesn't bite us mid-task.

**Step 3: Read the three files listed above end-to-end**

Specifically note:
- `OllamaClient::new` signature, `generate` request/response types, `mock_from_env` and `mock_generate`
- `mock_generate`'s prompt-pattern dispatch (which prompt prefixes map to which canned responses)
- `rewriter::rewrite` signature and the two existing tests (`empty_prior_turns_returns_identity_without_calling_ollama`, `connection_failure_returns_fallback_to_raw`)

**Step 4: No commit** (this is a read-only context-loading task)

---

## Task 1: Create `backend` module skeleton + data types

**Files:**
- Create: `mur-core/src/conversations/backend/mod.rs`
- Modify: `mur-core/src/conversations/mod.rs` (add `pub mod backend;`)

**Step 1: Write the failing tests in a new test module at the bottom of `mod.rs`**

Add to `mur-core/src/conversations/backend/mod.rs`:

```rust
//! ChatBackend trait and supporting types. See spec
//! `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` §4.

#![allow(dead_code)] // wired progressively across P0 tasks.

use std::pin::Pin;
use std::sync::Arc;
use anyhow::Result;
use futures::stream::Stream;
use serde::Serialize;

/// Per-call request to a chat-completion backend. Borrows where it can —
/// callers typically hold owned strings and pass &str.
#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub user: &'a str,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub stop: Vec<String>,
    /// Anthropic prompt-caching hint for the system prompt. Ignored by
    /// backends where `supports_caching()` is false.
    pub cache_system: bool,
    /// Anthropic prompt-caching hint: split `user` at this byte offset and
    /// place a cache_control breakpoint after the prefix. Ignored when
    /// `supports_caching()` is false. P0 stub only — wiring lands in P3.
    pub cache_user_prefix: Option<usize>,
}

/// Non-streaming response. `text` is the model output; `usage` reports
/// per-call token accounting (cache fields are 0 on non-caching backends).
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: String,
    pub usage: Usage,
}

/// Per-call token accounting. Both Anthropic-specific cache fields are
/// always present and default to 0 on non-Anthropic backends — keeps
/// downstream serialization shape uniform.
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub provider: &'static str,
    pub model: String,
}

/// Streaming chunk. `delta` is the incremental token payload (may be empty
/// on the final chunk). `usage` is `Some` ONLY on the final chunk.
#[derive(Debug, Clone)]
pub struct ChatChunk {
    pub delta: String,
    pub usage: Option<Usage>,
}

/// Type alias for the boxed stream of chunks returned by `generate_stream`.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_builds_with_required_fields() {
        let req = ChatRequest {
            model: "test-model",
            system: Some("you are a tester"),
            user: "hello",
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        assert_eq!(req.user, "hello");
        assert_eq!(req.model, "test-model");
    }

    #[test]
    fn usage_serializes_with_zero_cache_fields_on_non_anthropic() {
        let u = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            provider: "ollama",
            model: "qwen3:14b".into(),
        };
        let json = serde_json::to_string(&u).unwrap();
        assert!(json.contains("\"cache_read_input_tokens\":0"));
        assert!(json.contains("\"provider\":\"ollama\""));
    }
}
```

Modify `mur-core/src/conversations/mod.rs` — add line near other `pub mod` declarations:

```rust
pub mod backend;
```

**Step 2: Run tests to confirm they fail to compile**

Run: `cargo test -p mur-core --lib conversations::backend -- --nocapture`

Expected: FAIL — file/module doesn't exist yet (you wrote tests but nothing compiles past the missing imports). If `futures` isn't already a dep, this fails on the `Stream` import.

**Step 3: Verify `futures` is available**

Run: `grep '^futures' /Users/david/Projects/mur/mur-core/Cargo.toml`

Expected: a line like `futures = "0.3"` (it's already used by `ollama.rs`). If absent, add it.

**Step 4: Re-run tests to verify they pass**

Run: `cargo test -p mur-core --lib conversations::backend -- --nocapture`

Expected: PASS — both `chat_request_builds_with_required_fields` and `usage_serializes_with_zero_cache_fields_on_non_anthropic`.

**Step 5: Lint and format**

Run:
```bash
cargo fmt --check && cargo clippy -p mur-core --lib -- -D warnings
```

Expected: clean (no diff from fmt, no clippy warnings).

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/mod.rs mur-core/src/conversations/mod.rs
git commit -m "feat(backend): scaffold ChatBackend module with request/response types

Introduces the data-only surface (ChatRequest, ChatResponse, Usage,
ChatChunk, ChatStream) for the new conversations::backend module.
Trait and impls land in subsequent commits. No call sites changed.

Refs spec docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md §4.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Define `ChatBackend` trait + `BackendError` enum

**Files:**
- Modify: `mur-core/src/conversations/backend/mod.rs`

**Step 1: Add the failing test for trait-object compatibility**

Append to the `tests` module in `mod.rs`:

```rust
    #[test]
    fn chat_backend_is_object_safe() {
        // Compile-time check: ChatBackend must be usable as `dyn ChatBackend`.
        // If this fails to compile, the trait broke object safety
        // (e.g. someone added a generic method or `Self: Sized` bound).
        fn _accepts(_: &dyn ChatBackend) {}
    }

    #[test]
    fn backend_error_displays_with_provider_name() {
        let e = BackendError::Unauthorized { provider: "anthropic" };
        let msg = format!("{e}");
        assert!(msg.contains("anthropic"));
        assert!(msg.contains("401"));
    }
```

**Step 2: Run tests to confirm compile failure**

Run: `cargo test -p mur-core --lib conversations::backend`

Expected: FAIL — `ChatBackend` and `BackendError` not defined.

**Step 3: Add the trait and error enum to `mod.rs`**

Insert before the `#[cfg(test)]` block:

```rust
/// Backend-agnostic chat-completion interface.
///
/// Backends MUST be object-safe (no generics on methods, no `Self: Sized`).
/// Both methods are async via `async-trait`. `generate_stream` MAY return
/// a single-chunk stream when the backend doesn't natively stream
/// (e.g. P0 OllamaBackend stubs `generate_stream` to `unimplemented!()` —
/// only the non-streaming path is exercised in P0).
#[async_trait::async_trait]
pub trait ChatBackend: Send + Sync {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse>;

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream>;

    fn provider_name(&self) -> &'static str;

    /// True when the backend honors `cache_system` / `cache_user_prefix`
    /// hints. False = hints are silently ignored. Default: false.
    fn supports_caching(&self) -> bool {
        false
    }
}

/// Typed errors at the backend boundary. Backend impls construct these
/// and convert via `anyhow::Error::from(...)` so callers see anyhow
/// chains but can still downcast for retry-policy decisions.
///
/// See spec §4.3.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("provider {provider} returned 401: invalid or missing API key")]
    Unauthorized { provider: &'static str },

    #[error("provider {provider} returned 429: rate limited (retry-after: {retry_after_secs:?}s)")]
    RateLimited {
        provider: &'static str,
        retry_after_secs: Option<u64>,
    },

    #[error("provider {provider} returned {status}: server error")]
    ServerError {
        provider: &'static str,
        status: u16,
    },

    #[error("provider {provider} model {model} not found")]
    ModelNotFound {
        provider: &'static str,
        model: String,
    },

    #[error("provider {provider} timed out after {seconds}s")]
    Timeout {
        provider: &'static str,
        seconds: u64,
    },

    #[error("network error talking to {provider}: {source}")]
    Network {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },

    #[error("malformed response from {provider}: {message}")]
    BadResponse {
        provider: &'static str,
        message: String,
    },
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib conversations::backend`

Expected: PASS — `chat_backend_is_object_safe` compiles, `backend_error_displays_with_provider_name` produces a message containing "anthropic" and "401".

**Step 5: Lint and format**

Run:
```bash
cargo fmt --check && cargo clippy -p mur-core --lib -- -D warnings
```

Expected: clean.

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/mod.rs
git commit -m "feat(backend): add ChatBackend trait and BackendError enum

Object-safe async trait with generate, generate_stream, provider_name,
and supports_caching default-false. BackendError uses thiserror with
named-field variants per project convention (no catch-all Other variant).

Refs spec §4.1 / §4.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Implement `MockBackend`

**Files:**
- Create: `mur-core/src/conversations/backend/mock.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (add `pub mod mock;`)

**Step 1: Read the existing `mock_generate` function**

Run: `sed -n '260,330p' /Users/david/Projects/mur/mur-core/src/conversations/ollama.rs`

This is the pattern dispatcher we're going to adapt — note which prompt prefixes route to which canned responses. **Do not modify** `ollama.rs::mock_generate` — `OllamaBackend` will continue to use it via `OllamaClient` until everything else also moves to `ChatBackend`.

**Step 2: Write the failing tests for MockBackend**

Create `mur-core/src/conversations/backend/mock.rs` with:

```rust
//! Test-only ChatBackend that returns pattern-matched canned responses.
//! Reuses the prompt-dispatch logic from `ollama.rs::mock_generate` —
//! activated by `MUR_LLM_MOCK=1` (preferred) or `MUR_OLLAMA_MOCK=1` (legacy).
//!
//! See spec §5.3.

#![allow(dead_code)] // wired by factory in Task 5.

use anyhow::Result;
use async_trait::async_trait;
use futures::stream;

use super::{ChatBackend, ChatChunk, ChatRequest, ChatResponse, ChatStream, Usage};

pub struct MockBackend;

impl MockBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChatBackend for MockBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        // Reuse the existing pattern dispatcher. ollama::mock_generate takes
        // a GenerateRequest, so build one from our ChatRequest.
        use crate::conversations::ollama::{GenerateOptions, GenerateRequest};
        let g_req = GenerateRequest {
            model: req.model,
            prompt: req.user,
            system: req.system,
            stream: false,
            options: GenerateOptions {
                temperature: req.temperature,
                top_p: None,
                num_predict: Some(req.max_tokens),
                stop: req.stop.clone(),
            },
        };
        let g_resp = crate::conversations::ollama::mock_generate(&g_req);
        Ok(ChatResponse {
            text: g_resp.response,
            usage: Usage {
                input_tokens: g_resp.prompt_eval_count,
                output_tokens: g_resp.eval_count,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                provider: "mock",
                model: req.model.to_string(),
            },
        })
    }

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        // Mock streaming = single-chunk stream containing the full mock response.
        let resp = self.generate(req).await?;
        let final_chunk = ChatChunk {
            delta: resp.text.clone(),
            usage: Some(resp.usage),
        };
        Ok(Box::pin(stream::iter(vec![Ok(final_chunk)])))
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn req<'a>(prompt: &'a str) -> ChatRequest<'a> {
        ChatRequest {
            model: "mock-model",
            system: None,
            user: prompt,
            max_tokens: 100,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[tokio::test]
    async fn generate_returns_text_and_usage() {
        let b = MockBackend::new();
        let r = b.generate(req("hello")).await.unwrap();
        // Mock returns *some* text — exact content depends on prompt patterns
        // in ollama::mock_generate. The contract here is just non-empty.
        assert!(!r.text.is_empty());
        assert_eq!(r.usage.provider, "mock");
        assert_eq!(r.usage.model, "mock-model");
    }

    #[tokio::test]
    async fn generate_stream_emits_single_chunk_with_usage() {
        let b = MockBackend::new();
        let mut stream = b.generate_stream(req("hello")).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert!(!first.delta.is_empty());
        assert!(first.usage.is_some());
        assert!(stream.next().await.is_none(), "should be a single-chunk stream");
    }

    #[test]
    fn provider_name_is_mock() {
        assert_eq!(MockBackend::new().provider_name(), "mock");
    }
}
```

Append to `mur-core/src/conversations/backend/mod.rs`:

```rust
pub mod mock;
```

**Step 3: Check whether `ollama::mock_generate` is `pub(crate)` or private**

Run: `grep -n "fn mock_generate" /Users/david/Projects/mur/mur-core/src/conversations/ollama.rs`

If it's `fn mock_generate` (private), change it to `pub(crate) fn mock_generate` so `backend::mock` can call it. Same for `GenerateRequest` / `GenerateOptions` if they aren't already crate-visible.

**Step 4: Run tests**

Run: `cargo test -p mur-core --lib conversations::backend::mock`

Expected: PASS for all three tests.

**Step 5: Lint and format**

Run:
```bash
cargo fmt --check && cargo clippy -p mur-core --lib -- -D warnings
```

Expected: clean.

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/mock.rs mur-core/src/conversations/backend/mod.rs mur-core/src/conversations/ollama.rs
git commit -m "feat(backend): add MockBackend reusing ollama::mock_generate

Test-only ChatBackend impl. Adapts ChatRequest <-> GenerateRequest and
delegates pattern matching to the existing ollama::mock_generate. Single-
chunk generate_stream that emits the full mock response with usage.

Visibility bump on ollama::{mock_generate, GenerateRequest, GenerateOptions}
from private to pub(crate). Behavior unchanged for existing callers.

Refs spec §5.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Implement `OllamaBackend`

**Files:**
- Create: `mur-core/src/conversations/backend/ollama.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (add `pub mod ollama;`)

**Step 1: Write the failing test**

Create `mur-core/src/conversations/backend/ollama.rs`:

```rust
//! Adapter wrapping the existing OllamaClient as a ChatBackend.
//! See spec §5.1.

#![allow(dead_code)] // wired by factory in Task 5.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::conversations::ollama::{GenerateOptions, GenerateRequest, OllamaClient};

use super::{ChatBackend, ChatChunk, ChatRequest, ChatResponse, ChatStream, Usage};

pub struct OllamaBackend {
    client: OllamaClient,
}

impl OllamaBackend {
    pub fn new(endpoint: &str, timeout: Duration) -> Self {
        Self {
            client: OllamaClient::new(endpoint, timeout),
        }
    }
}

#[async_trait]
impl ChatBackend for OllamaBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let g_req = GenerateRequest {
            model: req.model,
            prompt: req.user,
            system: req.system,
            stream: false,
            options: GenerateOptions {
                temperature: req.temperature,
                top_p: None,
                num_predict: Some(req.max_tokens),
                stop: req.stop.clone(),
            },
        };
        let resp = self.client.generate(g_req).await?;
        Ok(ChatResponse {
            text: resp.response,
            usage: Usage {
                input_tokens: resp.prompt_eval_count,
                output_tokens: resp.eval_count,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                provider: "ollama",
                model: req.model.to_string(),
            },
        })
    }

    async fn generate_stream(&self, _req: ChatRequest<'_>) -> Result<ChatStream> {
        // P0: streaming through the trait is not yet wired. The existing
        // OllamaClient::generate_stream is still used directly by ask::generate.
        // This becomes real in P2 when ask::generate migrates onto the trait.
        anyhow::bail!("OllamaBackend::generate_stream not wired in P0")
    }

    fn provider_name(&self) -> &'static str {
        "ollama"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::backend::ChatBackend;

    #[test]
    fn provider_name_is_ollama() {
        let b = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(100));
        assert_eq!(b.provider_name(), "ollama");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn generate_propagates_connection_failure() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        let b = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(200));
        let req = ChatRequest {
            model: "qwen3:14b",
            system: None,
            user: "hi",
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let r = b.generate(req).await;
        assert!(r.is_err(), "unreachable endpoint should error");
    }

    #[tokio::test]
    async fn generate_stream_returns_unimplemented_error_in_p0() {
        let b = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(100));
        let req = ChatRequest {
            model: "qwen3:14b",
            system: None,
            user: "hi",
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let r = b.generate_stream(req).await;
        assert!(r.is_err());
        assert!(format!("{:#}", r.unwrap_err()).contains("not wired in P0"));
    }
}
```

Append to `mur-core/src/conversations/backend/mod.rs`:

```rust
pub mod ollama;
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-core --lib conversations::backend::ollama`

Expected: FAIL — likely on `ENV_LOCK` not being `pub(crate)`.

**Step 3: Make `ENV_LOCK` crate-visible if needed**

Run: `grep -n "ENV_LOCK" /Users/david/Projects/mur/mur-core/src/conversations/mod.rs`

Adjust visibility to `pub(crate) static ENV_LOCK: ...` if currently private.

**Step 4: Re-run tests**

Run: `cargo test -p mur-core --lib conversations::backend::ollama`

Expected: PASS.

**Step 5: Lint and format**

Run:
```bash
cargo fmt --check && cargo clippy -p mur-core --lib -- -D warnings
```

Expected: clean.

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/ollama.rs mur-core/src/conversations/backend/mod.rs mur-core/src/conversations/mod.rs
git commit -m "feat(backend): add OllamaBackend adapter over OllamaClient

Wraps existing OllamaClient. generate() forwards through; generate_stream()
intentionally bails in P0 (no callers on the trait path yet — ask::generate
still uses OllamaClient::generate_stream directly until P2).

Visibility bump on conversations::ENV_LOCK to pub(crate) so backend tests
can serialize env-var mutation against existing test code.

Refs spec §5.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Implement `factory::build`

**Files:**
- Create: `mur-core/src/conversations/backend/factory.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (add `pub mod factory;`)

**Step 1: Write the failing tests**

Create `mur-core/src/conversations/backend/factory.rs`:

```rust
//! ChatBackend factory. Selects backend from a thin BackendSpec
//! (P0 minimal — full BackendConfig schema lands in P1).
//!
//! See spec §5.4.

#![allow(dead_code)] // wired into ask::rewriter in Task 6.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};

use super::{
    mock::MockBackend, ollama::OllamaBackend, ChatBackend,
};

/// P0 minimal backend specification. Will be replaced by the full
/// BackendConfig struct from `mur-common` in P1, when per-stage
/// schema lands. Kept local here so P0 doesn't touch mur-common.
#[derive(Debug, Clone)]
pub struct BackendSpec {
    pub provider: String,
    pub endpoint: Option<String>,
    pub timeout_secs: Option<u64>,
}

impl BackendSpec {
    pub fn ollama(endpoint: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            provider: "ollama".into(),
            endpoint: Some(endpoint.into()),
            timeout_secs: Some(timeout_secs),
        }
    }
}

/// Build a backend from spec. Honors MUR_LLM_MOCK / MUR_OLLAMA_MOCK
/// env vars: when either is set, returns MockBackend regardless of spec.
pub fn build(spec: &BackendSpec) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok() || std::env::var("MUR_OLLAMA_MOCK").is_ok() {
        tracing::debug!(provider = %spec.provider, "MUR_LLM_MOCK active — using MockBackend");
        return Ok(Arc::new(MockBackend::new()));
    }
    match spec.provider.as_str() {
        "ollama" => {
            let endpoint = spec.endpoint.as_deref().unwrap_or("http://localhost:11434");
            let timeout = Duration::from_secs(spec.timeout_secs.unwrap_or(120));
            Ok(Arc::new(OllamaBackend::new(endpoint, timeout)))
        }
        other => bail!("unsupported provider in P0: {other} (anthropic lands in P1)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_env_var_forces_mock_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_LLM_MOCK", "1") };
        let spec = BackendSpec::ollama("http://localhost:11434", 5);
        let b = build(&spec).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn legacy_mur_ollama_mock_env_var_also_forces_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let spec = BackendSpec::ollama("http://localhost:11434", 5);
        let b = build(&spec).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ollama_provider_returns_ollama_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let spec = BackendSpec::ollama("http://127.0.0.1:1", 1);
        let b = build(&spec).unwrap();
        assert_eq!(b.provider_name(), "ollama");
    }

    #[test]
    fn unsupported_provider_errors() {
        let spec = BackendSpec {
            provider: "openai".into(),
            endpoint: None,
            timeout_secs: None,
        };
        let r = build(&spec);
        assert!(r.is_err());
        assert!(format!("{:#}", r.unwrap_err()).contains("unsupported"));
    }
}
```

Append to `mur-core/src/conversations/backend/mod.rs`:

```rust
pub mod factory;
```

**Step 2: Run tests**

Run: `cargo test -p mur-core --lib conversations::backend::factory -- --test-threads=1`

(`--test-threads=1` because the env-var tests serialize on `ENV_LOCK` and we want deterministic interleaving.)

Expected: PASS for all four tests.

**Step 3: Run the full backend test suite to confirm no cross-test leakage**

Run: `cargo test -p mur-core --lib conversations::backend -- --test-threads=1`

Expected: all backend tests pass.

**Step 4: Lint and format**

Run:
```bash
cargo fmt --check && cargo clippy -p mur-core --lib -- -D warnings
```

Expected: clean.

**Step 5: Commit**

```bash
git add mur-core/src/conversations/backend/factory.rs mur-core/src/conversations/backend/mod.rs
git commit -m "feat(backend): add factory::build with mock env-var detection

BackendSpec is the P0-minimal stand-in for the full BackendConfig schema
(P1). MUR_LLM_MOCK and legacy MUR_OLLAMA_MOCK both activate MockBackend
regardless of provider. Anthropic provider returns 'unsupported in P0' —
slot reserved for P1.

Refs spec §5.4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Refactor `ask::rewriter` to use `ChatBackend`

**Files:**
- Modify: `mur-core/src/conversations/ask/rewriter.rs`
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (only the rewriter call site at line 1160-1163)

**Step 1: Re-read the existing rewriter contract**

Run: `cat /Users/david/Projects/mur/mur-core/src/conversations/ask/rewriter.rs`

Note:
- `pub async fn rewrite(client: &OllamaClient, model: &str, input: RewriteInput<'_>) -> RewriteResult`
- Two existing tests use real `OllamaClient::new(unreachable_url)` to test fallback
- The contract preserves: `RewriteInput { prior_turns, raw_question }` → `RewriteResult { rewritten, status }`

Goal: change the first param to `&dyn ChatBackend` while preserving every other behavior.

**Step 2: Modify the test signatures FIRST (TDD — drive the API change from the tests)**

In `mur-core/src/conversations/ask/rewriter.rs`, replace the two `#[tokio::test]` blocks at lines ~188–215:

```rust
    #[tokio::test]
    async fn empty_prior_turns_returns_identity_without_calling_backend() {
        // Use OllamaBackend pointing at unreachable endpoint — if we
        // accidentally call it, we'd get a connection error. The empty-
        // prior-turns short-circuit should fire before any backend call.
        use crate::conversations::backend::ollama::OllamaBackend;
        let backend = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(100));
        let input = RewriteInput {
            prior_turns: &[],
            raw_question: "what did I ship?",
        };
        let r = rewrite(&backend, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::Skipped);
        assert_eq!(r.rewritten, "what did I ship?");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn connection_failure_returns_fallback_to_raw() {
        use crate::conversations::backend::ollama::OllamaBackend;
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        let backend = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(200));
        let turns = vec![trec(1, "first q", "first a")];
        let input = RewriteInput {
            prior_turns: &turns,
            raw_question: "follow up",
        };
        let r = rewrite(&backend, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::FailedFellBackToRaw);
        assert_eq!(r.rewritten, "follow up");
    }
```

**Step 3: Run the tests to confirm they fail to compile**

Run: `cargo test -p mur-core --lib conversations::ask::rewriter`

Expected: FAIL — `rewrite` still takes `&OllamaClient`, not `&OllamaBackend`.

**Step 4: Update the `rewrite` signature and body**

Replace lines 1-110ish of `rewriter.rs`. Specifically:

- Remove the `use crate::conversations::ollama::{GenerateOptions, GenerateRequest, OllamaClient};` import
- Add `use crate::conversations::backend::{ChatBackend, ChatRequest};`
- Change the function signature from `pub async fn rewrite(client: &OllamaClient, model: &str, input: RewriteInput<'_>) -> RewriteResult` to `pub async fn rewrite(backend: &dyn ChatBackend, model: &str, input: RewriteInput<'_>) -> RewriteResult`
- Change the call body from `client.generate(GenerateRequest { ... }).await` to:

```rust
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &prompt,
            system: None,
            max_tokens: 80,
            temperature: Some(0.1),
            stop: vec!["\n".into()],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await;
```

- Change `Ok(r) => { let trimmed = r.response.trim().to_string(); ... }` to `Ok(r) => { let trimmed = r.text.trim().to_string(); ... }` (field rename `response` → `text`)

**Step 5: Update the production call site**

In `mur-core/src/cmd/conversations_cmd.rs`, find the rewriter setup at lines 1160-1163:

```rust
    let rewriter_client = OllamaClient::new(
        &ask_cfg.ollama_endpoint,
        std::time::Duration::from_secs(ask_cfg.rewriter_timeout_secs as u64),
    );
    let rewrite = ask::rewriter::rewrite(
        &rewriter_client,
        &model,
        ...
    ).await;
```

Replace with:

```rust
    let rewriter_backend = crate::conversations::backend::factory::build(
        &crate::conversations::backend::factory::BackendSpec::ollama(
            &ask_cfg.ollama_endpoint,
            ask_cfg.rewriter_timeout_secs as u64,
        ),
    )?;
    let rewrite = ask::rewriter::rewrite(
        rewriter_backend.as_ref(),
        &model,
        ...
    ).await;
```

(`as_ref()` deref-coerces `Arc<dyn ChatBackend>` to `&dyn ChatBackend`.)

You may need to drop the `use crate::conversations::ollama::OllamaClient;` import in `conversations_cmd.rs` — only if no other call site in this file still uses it. Check: `grep -c OllamaClient /Users/david/Projects/mur/mur-core/src/cmd/conversations_cmd.rs`. If 0 after the edit, remove the import.

**Step 6: Re-run rewriter tests**

Run: `cargo test -p mur-core --lib conversations::ask::rewriter -- --test-threads=1`

Expected: PASS for both `empty_prior_turns_returns_identity_without_calling_backend` and `connection_failure_returns_fallback_to_raw`.

**Step 7: Run the full conversations test suite to catch regressions**

Run: `cargo test -p mur-core --lib conversations -- --test-threads=1`

Expected: PASS — no other test references `rewrite`'s old signature.

**Step 8: Run integration tests**

Run: `cargo test -p mur-core --test cli_conversations -- --test-threads=1`

Expected: PASS — `test_ask_basic` and friends should still work because the rewriter behavior is byte-identical.

**Step 9: Lint and format**

Run:
```bash
cargo fmt --check && cargo clippy --workspace -- -D warnings
```

Expected: clean.

**Step 10: Commit**

```bash
git add mur-core/src/conversations/ask/rewriter.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "refactor(ask): migrate rewriter to ChatBackend trait

ask::rewriter::rewrite now takes &dyn ChatBackend instead of &OllamaClient.
Production call site in cmd_ask uses backend::factory::build to construct
the backend (currently always Ollama in P0; P1 will plumb provider config).

Behavior is byte-identical: same prompt template, same num_predict=80,
same stop="\\n", same fallback-to-raw on error. Existing rewriter tests
preserved with updated imports.

This is the P0 canary for the new trait — remaining call sites
(compact, ask::generate, ask::abstractive, summarize::*) migrate in P1/P2.

Refs spec §3 (call-site refactor list) and plan task 6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: End-to-end smoke verify and final report

**Files:** none modified

**Step 1: Full workspace build**

Run: `cargo build --workspace`

Expected: clean build, no warnings.

**Step 2: Full test suite**

Run: `cargo test --workspace -- --test-threads=1`

Expected: all tests pass. (Use `--test-threads=1` to keep `ENV_LOCK`-serialized tests deterministic.)

**Step 3: Workspace-wide clippy + fmt**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both clean.

**Step 4: Smoke test the actual `ask` command with mock**

Run:
```bash
MUR_LLM_MOCK=1 cargo run --bin mur -- conversations ask "what did I ship today?" 2>&1 | head -20
```

Expected: command exits 0, produces some mock output. (Exact text depends on `mock_generate`'s pattern matching for `qa` prompts — verify it doesn't error.)

**Step 5: Verify the legacy mock env var still works**

Run:
```bash
MUR_OLLAMA_MOCK=1 cargo run --bin mur -- conversations ask "what did I ship today?" 2>&1 | head -20
```

Expected: same behavior as Step 4 — both env vars activate MockBackend.

**Step 6: No new commit** — this is verification only. If anything fails, return to the relevant earlier task and fix.

**Step 7: Report what was done**

Summarize for the human reviewer:
- 6 commits on this branch
- New module: `mur-core/src/conversations/backend/{mod,mock,ollama,factory}.rs` (~400 LOC including tests)
- One refactored call site: `ask::rewriter`
- Test count delta: +12 unit tests, 0 removed
- Behavior: byte-identical for `ask` command (rewriter uses same prompt, same options)
- What's NOT done (deferred to P1+): AnthropicBackend, streaming on the trait, BackendConfig schema in mur-common, doctor enhancements, cost telemetry, migration of remaining 9+ call sites

---

## Out of scope — explicitly deferred

Do **not** do any of these in P0. Each is a P1+ phase per spec §12:

- **`AnthropicBackend`** — P1 (~250 LOC, raw HTTP via reqwest, SSE streaming)
- **`BackendConfig` in `mur-common/src/config.rs`** — P1 (per-stage routing schema)
- **Migrate `compact::extractive` / `compact::abstractive`** — P1 (canary cloud call site)
- **`generate_stream` real impl on `OllamaBackend`** — P2 (when `ask::generate` migrates)
- **Migrate `ask::generate`, `ask::abstractive`, `summarize::rollup`** — P1/P2
- **Retry envelope** — P1 (lift from `extract_llm.rs`)
- **Doctor cloud-provider checks** — P1
- **Prompt caching wiring** — P3
- **Cost telemetry, `cost-report` command** — P3
- **Migrate `learn` / `extract_llm` onto `ChatBackend`, delete `llm.rs`** — P4

If an instruction in this plan tempts you to touch these, **stop and ask** — it means the plan is wrong, not that you should do extra work.
