# mur Conversations — Cloud LLM Backend Design

**Status:** Draft 2026-05-01
**Depends on:** Phase 3.5.1 shipped (current `compact` + `ask` pipeline using Ollama only).
**Replaces:** Hardcoded Ollama-only paths in `mur-core/src/conversations/{summarize,ask}/`.

---

## 1. Purpose

`mur conversations compact` and `mur conversations ask` currently call only `OllamaClient` (`mur-core/src/conversations/ollama.rs`). When `qwen3:14b` (the configured default) is not pulled locally, both commands fail with an Ollama 404 — there is no fallback. Users without 12+ GB of RAM available for a local 14B model are effectively locked out of these features.

This design adds a `ChatBackend` trait and a Claude API (Anthropic) implementation alongside the existing Ollama backend, with **per-stage routing** so the user can mix providers (e.g. Haiku for extractive, Sonnet for ask, local Ollama for the cheap rewriter step). It does **not** remove the Ollama path — local-first remains the default, cloud is opt-in.

mur already has `mur-core/src/llm.rs::llm_complete()` for `learn`/`extract_llm` (Anthropic / OpenAI / Gemini / OpenRouter / Ollama, non-streaming). That code is the seed; this design generalizes it into a trait that supports streaming and prompt caching, then migrates `compact`/`ask` to use it. A follow-up phase (P4 below) migrates `learn`/`extract_llm` onto the same trait and deletes `llm.rs`.

## 2. Non-goals

Explicitly deferred or declined:

- **Adopting a third-party multi-provider crate** (`rig`, `multi-llm`, `edgequake-llm`, `flyllm`, `llmao`). mur's existing `llm.rs` covers ~80% of the surface; the missing pieces (streaming + prompt caching + per-stage routing) are ~400 LOC of focused work. Pulling a crate adds transitive deps, version-pin churn, and audit surface for marginal payoff. Revisit only if Bedrock / Vertex / Foundry support is requested.
- **Bedrock / Vertex AI / Foundry support.** Not needed today; the trait surface is provider-agnostic so adding them later is local.
- **Streaming for `compact`.** Compact's per-call output is bounded (`max_abstractive_words: 400` ≈ 600 tokens). Non-streaming is correct.
- **Auto-fallback from cloud to Ollama on cloud outage.** Silent provider switching causes summary-format drift. Failure modes are explicit per §8.
- **Cost guardrails this phase.** A `max_daily_cost_usd` config is desirable but lands in a follow-up; first ship telemetry (P3) so users see real numbers before a guardrail is calibrated.
- **Embedding migration to cloud.** The existing `qwen3-embedding:0.6b` Ollama path stays. This design covers chat/completion only.
- **Computer-use, tool calling, MCP, structured outputs.** None apply to compact/ask. The trait surface omits them entirely.

## 3. Architecture

```
                          ┌──────────────────────────────────┐
                          │  mur-core/src/conversations/     │
                          │      backend/  (NEW)             │
                          │                                  │
                          │  trait ChatBackend  ─────────┐   │
                          │     │                         │  │
                          │     ├─ OllamaBackend         │  │
                          │     │   (wraps existing      │  │
                          │     │    OllamaClient)       │  │
                          │     │                         │  │
                          │     ├─ AnthropicBackend      │  │
                          │     │   (raw HTTP via        │  │
                          │     │    reqwest, no SDK)    │  │
                          │     │                         │  │
                          │     └─ MockBackend            │  │
                          │         (test-only, replaces │  │
                          │          MUR_OLLAMA_MOCK)    │  │
                          │                              │  │
                          │  fn build(BackendConfig) ────┘  │
                          └────────────┬─────────────────────┘
                                       │ used by
            ┌──────────────────────────┴──────────────────────────┐
            │                                                     │
   conversations/summarize/                          conversations/ask/
     extractive.rs                                    rewriter.rs
     abstractive.rs                                   abstractive.rs
     rollup.rs                                        generate.rs (streaming)
```

**File structure:**

| File | Role | Status |
|---|---|---|
| `mur-core/src/conversations/backend/mod.rs` | `ChatBackend` trait, `ChatRequest`, `ChatResponse`, `Usage`, `ChatChunk`, `BackendError` | **New** (~150 LOC) |
| `mur-core/src/conversations/backend/ollama.rs` | `OllamaBackend` — wraps `OllamaClient` | **New** (~80 LOC) |
| `mur-core/src/conversations/backend/anthropic.rs` | `AnthropicBackend` — non-streaming + SSE streaming via `reqwest` | **New** (~250 LOC) |
| `mur-core/src/conversations/backend/mock.rs` | `MockBackend` — replaces `mock_generate()` from `ollama.rs` | **New** (~80 LOC) |
| `mur-core/src/conversations/backend/factory.rs` | `build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>>` + retry policy | **New** (~60 LOC) |
| `mur-common/src/config.rs` | `BackendConfig` struct; `CompactConfig`/`AskConfig` gain optional per-stage overrides | Modify |
| `mur-core/src/conversations/summarize/{extractive,abstractive,rollup}.rs` | Replace `OllamaClient` calls with `ChatBackend` | Modify |
| `mur-core/src/conversations/ask/{rewriter,abstractive,generate}.rs` | Replace `OllamaClient` calls with `ChatBackend` | Modify |
| `mur-core/src/cmd/conversations_cmd.rs` | Wire `factory::build()` into `cmd_conversations_compact` and `cmd_ask` | Modify |
| `mur-core/src/cmd/conversations_cmd.rs::cmd_conversations_doctor` | Add cloud-provider reachability + API-key checks | Modify |
| `mur-core/tests/cli_conversations.rs` | Mock-backend tests; add cloud-provider integration test gated `#[ignore]` | Modify |

**No changes to:** `OllamaClient` itself (kept as backend impl detail), embedding pipeline, vector store, capture/retrieve/inject, agent runtime.

**New Cargo dependencies:** `eventsource-stream` is **declined** — SSE parsing is ~30 lines and we already hand-roll NDJSON in `ollama.rs::generate_stream`. Reuse the same buffer-and-split pattern. No new crates.

**Tech stack:** Rust 2024 · `reqwest` (already in deps) · `tokio` · `tracing` · `anyhow` for application errors · `thiserror` for `BackendError` at the trait boundary.

## 4. `ChatBackend` trait (`backend/mod.rs`)

### 4.1 Trait

```rust
use std::pin::Pin;
use std::sync::Arc;
use anyhow::Result;
use futures::stream::Stream;

#[async_trait::async_trait]
pub trait ChatBackend: Send + Sync {
    /// Single non-streaming completion. Used by extractive, abstractive,
    /// rewriter, and ask::abstractive::compress_hit.
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse>;

    /// Token-streaming completion. Used by ask::generate::stream_answer.
    /// Implementations that don't support streaming MAY emit the whole
    /// response as a single chunk.
    async fn generate_stream(
        &self,
        req: ChatRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>>;

    fn provider_name(&self) -> &'static str;

    /// True if the backend honors `cache_system` / `cache_user_prefix`
    /// hints in `ChatRequest`. False = hints are silently ignored.
    fn supports_caching(&self) -> bool { false }
}
```

### 4.2 Request / response types

```rust
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub user: &'a str,
    pub max_tokens: u32,
    /// Per-call temperature override. Trait-level — backends decide whether
    /// to send it. AnthropicBackend ignores on Opus 4.7 (where it 400s).
    pub temperature: Option<f32>,
    pub stop: Vec<String>,
    /// Anthropic prompt-caching hints. Ignored when supports_caching() = false.
    pub cache_system: bool,
    pub cache_user_prefix: Option<usize>, // bytes from front of `user` to cache
}

pub struct ChatResponse {
    pub text: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64, // Anthropic-only; 0 for others
    pub cache_read_input_tokens: u64,     // Anthropic-only; 0 for others
    pub provider: &'static str,
    pub model: String,
}

pub struct ChatChunk {
    pub delta: String,
    pub usage: Option<Usage>, // Some on the final chunk only
}
```

### 4.3 Errors

`BackendError` is a `thiserror` enum at the trait boundary so callers can dispatch on failure mode (vs. anyhow's opaque chain):

```rust
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("provider {provider} returned 401: invalid or missing API key")]
    Unauthorized { provider: &'static str },

    #[error("provider {provider} returned 429: rate limited (retry-after: {retry_after_secs:?}s)")]
    RateLimited { provider: &'static str, retry_after_secs: Option<u64> },

    #[error("provider {provider} returned 5xx: server error (status {status})")]
    ServerError { provider: &'static str, status: u16 },

    #[error("provider {provider} model {model} not found")]
    ModelNotFound { provider: &'static str, model: String },

    #[error("provider {provider} timed out after {seconds}s")]
    Timeout { provider: &'static str, seconds: u64 },

    #[error("network error talking to {provider}: {source}")]
    Network { provider: &'static str, #[source] source: reqwest::Error },

    #[error("malformed response from {provider}: {0}")]
    BadResponse(String),
}
```

`ChatBackend::generate` returns `anyhow::Result<ChatResponse>` (not `Result<_, BackendError>`) so call sites stay simple — but every backend impl wraps its raw error in `BackendError` first, then `.into()` to anyhow. This preserves the typed dispatch via `err.downcast_ref::<BackendError>()` for the retry loop in §8.

## 5. Backend implementations

### 5.1 `OllamaBackend` — wraps existing client

Pure adapter. Construct with the same `OllamaClient::new(endpoint, timeout)` and forward calls. `Usage` is populated from `GenerateResponse.prompt_eval_count` + `eval_count` (already returned by Ollama, currently unused per the audit). `cache_*_input_tokens` always 0. `supports_caching()` = false.

### 5.2 `AnthropicBackend` — raw HTTP

Rust has no official Anthropic SDK ([cached `claude-api` skill confirms](#sources)). Implement raw HTTP via `reqwest`.

**Endpoint:** `https://api.anthropic.com/v1/messages`

**Required headers:**
```
x-api-key: $ANTHROPIC_API_KEY
anthropic-version: 2023-06-01
content-type: application/json
```

No beta headers needed for the features we use (caching is GA).

**Request body** (non-streaming):
```json
{
  "model": "claude-haiku-4-5",
  "max_tokens": 4096,
  "system": [
    {"type": "text", "text": "...", "cache_control": {"type": "ephemeral"}}
  ],
  "messages": [
    {"role": "user", "content": [
      {"type": "text", "text": "<cacheable prefix>", "cache_control": {"type": "ephemeral"}},
      {"type": "text", "text": "<volatile suffix>"}
    ]}
  ]
}
```

`cache_control` blocks are emitted **only when `req.cache_system` is true** (system) and **only when `req.cache_user_prefix` is `Some(n)`** (user, splitting the user string at byte n). When neither is set the `system` field is sent as a plain string and the `user` content is a single-block array. This minimizes JSON-shape churn for callers that don't need caching.

**Streaming request:** add `"stream": true`. SSE response shape from the `claude-api` skill (`curl/examples.md`):

```
event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
```

Parser dispatches on `data` JSON `type` field:
- `content_block_delta` with `delta.type == "text_delta"` → emit `ChatChunk { delta: text, usage: None }`
- `message_delta` → capture `usage` for the final chunk
- `message_stop` → emit `ChatChunk { delta: "", usage: Some(final_usage) }` and end stream

**Caching invariants** (per `shared/prompt-caching.md`, loaded via the `claude-api` skill):

1. **Render order is `tools → system → messages`.** A breakpoint on the last system block caches both tools and system together. mur uses no tools in compact/ask, so a single system breakpoint caches the full prefix.
2. **Any byte change anywhere in the prefix invalidates everything after.** The `compact` and `ask` system prompts MUST be free of timestamps, UUIDs, and per-call interpolation. Move any per-day or per-call data into the user message.
3. **Max 4 `cache_control` breakpoints per request.** mur uses at most 2 (system + user prefix).
4. **Minimum cacheable prefix:** 2048 tokens for Haiku 4.5, 4096 tokens for Opus 4.6/4.7. Below this, `cache_control` is silently ignored — verify via `usage.cache_creation_input_tokens`.

**Sampling parameters on Opus 4.7:** the skill states `temperature`, `top_p`, `top_k` return 400 on Opus 4.7. `AnthropicBackend` checks `req.model.starts_with("claude-opus-4-7")` and **drops** `temperature` from the request body if set. `tracing::warn!` once on first occurrence (avoid log spam).

**Thinking config:** mur uses no extended thinking. `AnthropicBackend` sends `"thinking": {"type": "disabled"}` for Opus 4.6+ to skip the implicit adaptive default — saves tokens, irrelevant to our use case.

### 5.3 `MockBackend` — replaces `MUR_OLLAMA_MOCK`

Reuses the existing `mock_generate()` pattern-matching logic from `ollama.rs:261-323` verbatim (extractive JSON, abstractive prose, narrative time windows, CONDENSE identity, QA citation). Returns synthetic `Usage` numbers.

**Activation:** new env var `MUR_LLM_MOCK=1`. The old `MUR_OLLAMA_MOCK=1` continues to work for backward compat — `MockBackend` is selected by `factory::build()` when **either** is set. Deprecation warning is `tracing::warn!` only; both env vars are accepted indefinitely.

### 5.4 Factory

```rust
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok()
        || std::env::var("MUR_OLLAMA_MOCK").is_ok()
    {
        return Ok(Arc::new(MockBackend::new()));
    }
    match cfg.provider.as_str() {
        "ollama" => Ok(Arc::new(OllamaBackend::new(
            &cfg.endpoint.as_deref().unwrap_or("http://localhost:11434"),
            Duration::from_secs(cfg.timeout_secs.unwrap_or(120)),
        ))),
        "anthropic" => {
            let key = resolve_api_key(cfg)?;
            Ok(Arc::new(AnthropicBackend::new(key, &cfg)?))
        }
        other => bail!("unsupported provider: {other}"),
    }
}

fn resolve_api_key(cfg: &BackendConfig) -> Result<String> {
    // 1. cfg.api_key_env (e.g. "ANTHROPIC_API_KEY")
    // 2. fallback to std env "ANTHROPIC_API_KEY"
    // 3. (P3+) keychain via secrecy::SecretString — see §10
}
```

`Arc<dyn ChatBackend>` (not `Box`) so call sites can clone cheaply for `tokio::spawn`.

## 6. Config schema (`mur-common/src/config.rs`)

Add `BackendConfig` and per-stage overrides. `#[serde(default)]` everywhere — every existing config file keeps working unchanged.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendConfig {
    /// "ollama" | "anthropic". Defaults to "ollama" for backward compat.
    pub provider: String,
    /// Model name as the provider sees it ("claude-haiku-4-5", "qwen3:14b", …).
    pub model: String,
    /// Provider endpoint. None = provider default
    /// (ollama: http://localhost:11434, anthropic: https://api.anthropic.com).
    pub endpoint: Option<String>,
    /// Env var holding the API key. None = no auth (ollama).
    pub api_key_env: Option<String>,
    /// Per-call timeout. None = 120s.
    pub timeout_secs: Option<u64>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        }
    }
}
```

`CompactConfig` and `AskConfig` gain optional per-stage `BackendConfig` overrides:

```rust
pub struct CompactConfig {
    // ...existing fields preserved...
    pub extractive_model: String,         // kept; legacy fallback
    pub abstractive_model: String,        // kept; legacy fallback
    pub ollama_endpoint: String,          // kept; legacy fallback

    /// NEW — per-stage overrides. None = synthesize from legacy fields above.
    #[serde(default)]
    pub extractive_backend: Option<BackendConfig>,
    #[serde(default)]
    pub abstractive_backend: Option<BackendConfig>,
}

pub struct AskConfig {
    // ...existing fields preserved...
    pub model: String,
    pub ollama_endpoint: String,

    #[serde(default)]
    pub backend: Option<BackendConfig>,        // for the answer model
    #[serde(default)]
    pub rewriter_backend: Option<BackendConfig>,
}
```

**Resolution order** when constructing the backend for a stage:
1. If the per-stage `backend` field is `Some`, use it.
2. Else, synthesize a `BackendConfig { provider: "ollama", model: <legacy field>, endpoint: Some(<legacy ollama_endpoint>), .. }`.

This keeps every existing config file working byte-identically — no migration required, no deprecation warnings.

**Recommended user config** (documented in `~/.mur/config.yaml` comments and the docs site):

```yaml
conversations:
  compact:
    extractive_backend:
      provider: anthropic
      model: claude-haiku-4-5
      api_key_env: ANTHROPIC_API_KEY
    abstractive_backend:
      provider: anthropic
      model: claude-haiku-4-5
      api_key_env: ANTHROPIC_API_KEY
  ask:
    backend:
      provider: anthropic
      model: claude-sonnet-4-6
      api_key_env: ANTHROPIC_API_KEY
    rewriter_backend:
      provider: ollama
      model: llama3.2:3b   # tiny local model — rewriter is latency-sensitive
```

## 7. Per-stage routing rationale

| Stage | Volume | Quality bar | Default cloud model | Rationale |
|---|---|---|---|---|
| `compact.extractive` | High (every chunk every day) | Low — span selection | `claude-haiku-4-5` | $1/$5 per 1M; quality difference vs Sonnet on span-selection is negligible |
| `compact.abstractive` | Medium (1× per day + rollups) | Medium — narrative summary | `claude-haiku-4-5` | Same; user can upgrade to Sonnet for prose quality |
| `ask` answer gen | Low (user-initiated, streaming) | High — final user-facing answer | `claude-sonnet-4-6` | $3/$15; matters for the answer the user reads |
| `ask` rewriter | Low (per ask, ≤80 tokens out) | Low — query reformulation | `llama3.2:3b` (Ollama) | Local + fast; rewriter latency directly hits TTFB |

**Why not default to Opus 4.7 for `ask`:** the loaded `claude-api` skill says *"Always use claude-opus-4-7 unless the user explicitly names a different model. Never downgrade for cost — that's the user's decision."* That guidance applies to **engineering an Anthropic-API app from scratch** (where the developer is the decider). For mur, the *user* is the decider, and they configure the model in `config.yaml`. The shipped defaults (Haiku/Sonnet) are reasoned defaults for autonomous nightly compact + interactive ask, not "downgrades." Documentation explicitly flags Opus 4.7 as the upgrade path.

## 8. Retry, timeout, and failure policy

### 8.1 Retry envelope

Lift the retry logic from `mur-core/src/extract_llm.rs:216-252` into `factory::build` (so all backends inherit it):

- 3 attempts max
- Exponential backoff: 1s, 2s, 4s + jitter
- Retry on: `BackendError::ServerError { status: 500..=599 }`, `BackendError::Timeout`, `BackendError::RateLimited` (respect `retry_after_secs` if present, capped at 30s)
- Do **not** retry on: `Unauthorized`, `ModelNotFound`, `BadResponse`, `Network` other than connect-timeout

Retry happens inside the `Arc<dyn ChatBackend>` adapter so callers don't need to reimplement.

### 8.2 Failure surface per command

| Command | Backend down | Cloud auth fails | Local model missing |
|---|---|---|---|
| `mur conversations compact` | **Skip day**, mark `outcome: deferred`, retry on next sweep | **Hard fail** with API-key error | **Hard fail** with `ollama pull X` hint |
| `mur conversations ask` | **Hard fail** — user is waiting | **Hard fail** with API-key error | **Hard fail** with `ollama pull X` hint |
| `mur conversations doctor` | Show ✗ for unreachable provider; keep other checks running | Show ✗ with hint | Show ✗ with hint |

**No automatic fallback from cloud to Ollama** (or vice versa). Silent provider switching causes summary-format drift that pollutes the LanceDB index. Users who want a fallback configure two `mur conversations compact` cron jobs with different configs and let the second one fill gaps.

### 8.3 Doctor enhancements

`cmd_conversations_doctor` (`mur-core/src/cmd/conversations_cmd.rs:434`) gains:

- For each unique provider in active config: probe reachability (Ollama: GET `/api/tags`; Anthropic: GET `/v1/models` with the API key)
- For each unique cloud provider: verify the named env var is set and non-empty
- For each Anthropic model in config: GET `/v1/models/{id}` to verify it exists

Probe pattern follows the existing Ollama check (2-second timeout, non-fatal if backend is intentionally disabled).

## 9. Streaming SSE parser

`AnthropicBackend::generate_stream` mirrors the structure of `ollama.rs::generate_stream` (lines 161-243) — same `futures::stream::unfold` shape with a line-buffered byte stream, but the inner parser dispatches on SSE `event:` / `data:` framing instead of raw NDJSON.

Pseudocode:

```rust
loop {
    // 1. Drain any complete `event: X\ndata: Y\n\n` block from buf
    if let Some(end) = buf.find("\n\n") {
        let block: String = buf.drain(..=end + 1).collect();
        match parse_sse_block(&block) {
            SseEvent::ContentBlockDelta { text } => return yield ChatChunk { delta: text, usage: None },
            SseEvent::MessageDelta { usage } => { final_usage = Some(usage); continue; }
            SseEvent::MessageStop => return yield ChatChunk { delta: String::new(), usage: final_usage },
            SseEvent::Other => continue,
        }
    }
    // 2. Read more bytes
    match inner.next().await { ... }
}
```

This is ~30 lines of bespoke parsing — no `eventsource-stream` dep needed. Test coverage mirrors the existing `ollama.rs:387-465` chunked-JSON-split tests.

## 10. Secrets handling

P1 ships **env-var only** (`api_key_env` field naming the env var that holds the key). Sufficient for local CLI use; matches `LlmConfig.api_key_env`'s existing shape.

P3+ gains keychain integration via the existing `mur-agent-runtime` keychain code path (`agent secret set` already uses `keyring` crate). The `BackendConfig` struct gains a `secret_ref: Option<String>` field that resolves through the same model-registry → SecretRef lookup that `mur-agent-runtime` uses (per `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`).

Until P3, document `ANTHROPIC_API_KEY` in the install guide and add a `mur conversations doctor` check that prints a clear hint if the env var is unset.

## 11. Telemetry (P3, ships before P4)

Every `ChatBackend::generate` and `generate_stream` call emits a structured tracing span at `info` level:

```rust
#[tracing::instrument(skip_all, fields(
    provider = backend.provider_name(),
    model = req.model,
    stage = stage,
))]
async fn call_with_telemetry(...) {
    let start = Instant::now();
    let resp = backend.generate(req).await?;
    tracing::info!(
        input_tokens = resp.usage.input_tokens,
        output_tokens = resp.usage.output_tokens,
        cache_read_tokens = resp.usage.cache_read_input_tokens,
        cache_write_tokens = resp.usage.cache_creation_input_tokens,
        latency_ms = start.elapsed().as_millis() as u64,
        "llm call completed"
    );
    Ok(resp)
}
```

A `tracing_subscriber` JSON layer writes these to `~/.mur/telemetry/llm-calls-<date>.jsonl` (rotated daily). New command `mur conversations cost-report [--since <duration>]` aggregates these spans and prints provider × model × stage × token totals + estimated USD cost (from a hardcoded price table sourced from `claude-api`'s `shared/models.md`).

## 12. Phased delivery

Each phase is independently shippable and reviewable.

| Phase | Scope | LOC est | User-visible? |
|---|---|---|---|
| **P0** | `ChatBackend` trait + `OllamaBackend` + `MockBackend` + `factory::build`. Refactor `ask::rewriter` only. No behavior change. | ~350 | No |
| **P1** | `AnthropicBackend` (non-streaming). `BackendConfig` schema. Wire `compact.extractive` as canary. Doctor enhancements. | ~400 | Yes — opt-in |
| **P2** | `AnthropicBackend::generate_stream` (SSE). Wire `ask` answer gen. | ~150 | Yes |
| **P3** | Prompt caching wiring (cache_control on system prompts). Telemetry + `cost-report` command. | ~200 | Yes |
| **P4** | Migrate `learn` / `extract_llm` onto `ChatBackend`. Delete `mur-core/src/llm.rs`. | ~100 net (delete) | No |

P0 lands as a refactor PR; P1 is the first user-visible PR and gates on a manual test pass. P3 is the unlock that makes cloud cheaper than local for users without the 14B model pulled.

## 13. Cost guidance (documentation)

For a heavy mur user (~50K tokens/day of transcript content, ~5 chunks/day, compact runs nightly):

| Setup | Per-day | Per-month |
|---|---|---|
| All Ollama (current) | $0 + ~12GB RAM peak | $0 |
| Compact: Haiku, Ask: Sonnet, no caching | ~$0.10 | ~$3.00 |
| Compact: Haiku, Ask: Sonnet, **with caching** (P3) | ~$0.08 | ~$2.40 |
| Compact: Sonnet, Ask: Sonnet (overkill) | ~$0.30 | ~$9.00 |
| Compact: Opus, Ask: Opus (very overkill) | ~$1.50 | ~$45 |

Docs and product-page copy should lead with "Haiku for compact, Sonnet for ask, ~$3/month" — that's the recommended config and the 90th-percentile cost.

## 14. Migration / backward compatibility

Any existing `~/.mur/config.yaml` continues to work unchanged. The audit confirms current configs hold legacy `extractive_model`, `abstractive_model`, `ollama_endpoint` strings; resolution order in §6 synthesizes a `BackendConfig` from those when no per-stage override is set.

`MUR_OLLAMA_MOCK=1` continues to activate `MockBackend` (alongside the new `MUR_LLM_MOCK=1`). All existing test fixtures pass without changes.

`mur conversations compact` and `mur conversations ask` CLI surfaces are unchanged. Existing `--model` flag continues to work and overrides the per-stage `backend.model` field at call time.

## 15. Open questions

1. Should `BackendConfig` support **per-call retry-policy override** (some users may want zero retries on `ask` for fast-fail UX)? Lean towards no — defer until requested.
2. Should the `ask` streaming path show a **provider tag** in the streamed output (e.g. `[claude-sonnet-4-6]` prefix on first chunk)? Useful for debugging mixed-provider configs but noisy for normal use. Lean: gate behind `--show-provider` flag.
3. Should `cost-report` also report against an **org-level budget** read from a new config field? Defer to follow-up — first ship telemetry, see usage, then design the guardrail.
4. Eventually, should `mur agent` also use `ChatBackend`? P0a runtime currently has its own model-resolution path. Out of scope for this design but worth flagging — the model-registry design (`2026-04-29-model-registry-and-secret-refs-design.md`) and this design should converge in a future cleanup.

## 16. References

- `docs/superpowers/specs/2026-04-19-mur-conversations-design.md` — original conversations subsystem design
- `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md` — agent runtime's `SecretRef` model that P3 keychain support will mirror
- `mur-core/src/conversations/ollama.rs` — current Ollama client (becomes `OllamaBackend` impl detail)
- `mur-core/src/llm.rs` — current Anthropic/OpenAI/Gemini path (P4 migrates onto `ChatBackend` and deletes this file)
- Anthropic prompt caching, model catalog, SSE format, sampling-param removal on Opus 4.7 — sourced from the `claude-api` skill content (cached 2026-04-15)
