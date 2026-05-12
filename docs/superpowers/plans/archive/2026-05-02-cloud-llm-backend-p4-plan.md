# Cloud LLM Backend P4 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Migrate the four remaining `OllamaClient`-bypass call sites (`extract_llm.rs`, `cmd/learn.rs`, `capture/starter.rs`, `cmd/misc.rs`) onto `ChatBackend`, port OpenAI + Gemini provider support into the trait-based factory (so OpenRouter/Together/Fireworks users via `openai_url` keep working), and delete `mur-core/src/llm.rs` (524 LOC). After P4 the conversations subsystem is the single LLM-routing layer for the whole crate.

**Architecture:** Two new `ChatBackend` impls (`OpenAIBackend` + `GeminiBackend`) using the same wiremock-tested non-streaming shape as `AnthropicBackend`. `factory::build_raw` gains four new provider arms (`openai`, `openrouter`, `gemini`, plus a small `default_key_env(provider)` helper so `BackendConfig.api_key_env: None` resolves to the conventional env var). A new `LlmConfig::to_backend_config()` conversion preserves byte-identical deserialization for existing `~/.mur/config.yaml` `llm:` sections — so users with `provider: openai` or `provider: gemini` keep working without touching their config. Callers convert at the call site (`config.llm.to_backend_config()`) and call the trait. `extract_llm.rs`'s 30-line manual retry envelope is replaced by `RetryingBackend`. `is_reasoning_model` (a pure helper unrelated to provider dispatch) relocates to `mur-common::llm`. Then `mur-core/src/llm.rs` and `pub mod llm;` go away — net ~150 LOC deleted.

**Tech Stack:** Rust 2024 · `reqwest` · `tokio` · `tracing` · `wiremock` (dev-dep, already used by P1/P2 backends) · `anyhow` for app errors · `thiserror` for `BackendError` (no new variants — existing `Unauthorized/RateLimited/ServerError/ModelNotFound/Timeout/Network/BadResponse` covers OpenAI + Gemini error shapes). **No new crates.**

**Spec:** `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` — §12 P4 row says "Migrate `learn` / `extract_llm` onto `ChatBackend`. Delete `mur-core/src/llm.rs`. ~100 LOC net (delete)." Spec under-stated by ~50 LOC because P4 ALSO ports OpenAI + Gemini (preserving the OpenRouter/Together/Fireworks ecosystem via `endpoint`). Net delete is closer to ~150 LOC. Plan-level deviation flagged: P4 expands provider coverage in `factory::build` from 2 → 4 (plus `openrouter` alias).

**Out of scope for P4** — explicitly do not touch:
- Streaming for OpenAI / Gemini — neither call site that's being migrated streams. Add `Box::pin(once_chunk(generate))` style stub for `generate_stream` if needed (or `bail!` like P0 OllamaBackend did before P2). P5+ if a streaming caller materializes.
- Migrating `mur-agent-runtime` LLM paths (`mur-agent-runtime/src/llm/ollama.rs` etc.) — those are a separate subsystem with a different lifecycle.
- Cost-report telemetry prices for OpenAI / Gemini models — `cost-report`'s price table only covers Anthropic. New OpenAI/Gemini calls will appear in cost-report with `est_$ = "—"` (correct: we don't know the price). Adding price tables is a separate follow-up if the user asks.
- Bedrock / Vertex / Foundry — declined non-goal per spec §2.
- Auto-migrating `~/.mur/config.yaml` from `llm:` to `backend:` — `LlmConfig::to_backend_config()` runs in-process per call; on-disk shape is unchanged. Schema cleanup is its own task.

**Plan deviations flagged from spec:** P4 expands `factory::build`'s provider coverage from `{ollama, anthropic}` to `{ollama, anthropic, openai, openrouter, gemini}`. Justification: spec §12 promised "delete `llm.rs`" + "~100 LOC net", which is impossible without porting the providers `llm.rs` covered. This is the obvious read.

---

## Task 0: Verify foundation + read context (no commit)

**Files:** none modified.

**Step 1: Confirm P0 + P1 + P2 + P3 are on `main`**

```bash
git log --oneline | grep -E "8f0f712|f692594|79e4b72|f7acf51" | head -4
```

Expected — four lines, ordered most-recent-first:
```
f7acf51 feat: cloud-LLM backend P3 (caching wire + telemetry + cost-report + final migrations) (#103)
8f0f712 feat: cloud-LLM backend P2 (streaming on the trait + ask::generate canary) (#98)
f692594 feat: cloud-LLM backend P1 (AnthropicBackend + per-stage routing + retry envelope) (#91)
79e4b72 refactor(conversations): introduce ChatBackend trait (P0 of cloud-LLM rollout) (#80)
```

If any SHA missing, **STOP**.

**Step 2: Read the spec sections for P4**

- `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` §12 (phase boundaries — confirms P4 = delete llm.rs)

**Step 3: Read `llm.rs` end-to-end**

`mur-core/src/llm.rs` (524 lines). Note specifically:
- Provider dispatch at lines 18-38 — anthropic / openai / gemini / ollama / openrouter / generic-openai-compatible-via-openai_url
- `is_reasoning_model` at lines 47-85 — pure helper, unrelated to provider dispatch (Task 1 relocates this)
- `default_key_env` at lines 108-116 — provider→env-var helper (Task 4 brings into factory)
- `warn_if_oauth_key_misconfigured` at lines 125-143 — Claude OAuth bridge warning (Task 2 carries into AnthropicBackend OR keeps as a sticky one-shot warning when migrating)
- Anthropic / OpenAI / Gemini / Ollama request+response struct families — Tasks 2 + 3 port the OpenAI + Gemini ones as `*Backend` impls

**Step 4: Read the four caller sites**

- `mur-core/src/extract_llm.rs:16, 210-258` — has its OWN 3-attempt retry envelope on `llm_complete` (Task 6 deletes it; `RetryingBackend` from P0 covers this)
- `mur-core/src/cmd/learn.rs:7, 122` — single `llm::llm_complete(&config.llm, &system, &prompt)` call
- `mur-core/src/capture/starter.rs:779` — single `crate::llm::llm_complete(config, system, &prompt)` call (config is already `&LlmConfig`)
- `mur-core/src/cmd/misc.rs:104` — uses `is_reasoning_model` only (Task 1 relocates the helper)

**Step 5: Read the existing factory**

`mur-core/src/conversations/backend/factory.rs` — `build_raw(cfg)` dispatches on `cfg.provider.as_str()`. Currently knows `ollama` and `anthropic`. Task 4 extends the match arms.

**Step 6: No commit** — context-loading only.

---

## Task 1: Relocate `is_reasoning_model` to `mur-common::llm`

**Why:** `is_reasoning_model` is a pure string-classifier helper unrelated to provider dispatch. Once `llm.rs` is deleted (Task 9), this helper needs a durable home. `mur-common::llm` already houses `anthropic_base_url` so the natural fit.

**Files:**
- Modify: `mur-common/src/llm.rs` (add the function + tests)
- Modify: `mur-core/src/cmd/misc.rs:104` (update import)

**Step 1: Move the function**

Append to `mur-common/src/llm.rs`:

```rust
/// Check if a model name matches recommended reasoning models for session analysis.
///
/// Recommended: Anthropic Opus, OpenAI GPT-5/O3/O4, Gemini Pro 3+,
/// or any model with "reasoning" or "think" in the name.
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_lowercase();

    if m.contains("opus") {
        return true;
    }
    if m.contains("gpt-5") || m.contains("o3") || m.contains("o4") {
        return true;
    }
    if m.contains("gemini") && m.contains("pro") {
        if let Some(pos) = m.find("pro") {
            let after = &m[pos + 3..];
            let version_str: String = after
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(v) = version_str.parse::<u32>()
                && v >= 3
            {
                return true;
            }
        }
    }
    if m.contains("reasoning") || m.contains("think") {
        return true;
    }
    false
}
```

Copy the existing `test_is_reasoning_model` test from `mur-core/src/llm.rs:476-503` into `mur-common/src/llm.rs` `mod tests` (creating it if absent) — VERBATIM. Same assertions.

**Step 2: Update the misc.rs caller**

In `mur-core/src/cmd/misc.rs:104`, change:
```rust
use crate::llm::is_reasoning_model;
```
to:
```rust
use mur_common::llm::is_reasoning_model;
```

**Step 3: Run tests**

```bash
cargo test -p mur-common --lib -- is_reasoning_model 2>&1 | tail -5
cargo test -p mur-core --bin mur 2>&1 | tail -5
```

Expected: PASS (one new test in mur-common; mur-core bin still compiles).

**Step 4: Lint + commit**

```bash
cargo fmt -p mur-common -p mur-core && cargo fmt --check
cargo clippy -p mur-common -p mur-core --lib --tests -- -D warnings 2>&1 | tail -5
git add mur-common/src/llm.rs mur-core/src/cmd/misc.rs
git commit -m "$(cat <<'EOF'
refactor(common): relocate is_reasoning_model to mur-common::llm

Pure string-classifier helper that's unrelated to provider dispatch.
Moves to mur-common::llm alongside anthropic_base_url so it survives
the eventual deletion of mur-core/src/llm.rs (P4 task 9).

The cmd/misc.rs:cmd_doctor caller updates its import. Logic unchanged;
8 existing assertions preserved verbatim in the new test location.

Refs spec §12 P4. Plan task 1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `OpenAIBackend` (non-streaming, OpenAI-compatible)

**Files:**
- Create: `mur-core/src/conversations/backend/openai.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (`pub mod openai;`)

**Step 1: Write the failing tests**

Create `mur-core/src/conversations/backend/openai.rs` with module skeleton + 5 wiremock tests. Tests mirror the AnthropicBackend test structure (`mur-core/src/conversations/backend/anthropic.rs::mod tests`):

```rust
//! OpenAI Chat Completions API backend. Also covers OpenAI-compatible
//! providers (OpenRouter, Together, Fireworks, etc.) via `endpoint` override.
//! Non-streaming only — see spec §5.x, plan task 2.

#![allow(dead_code)] // wired into factory in P4 task 4.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{BackendError, ChatBackend, ChatChunk, ChatRequest, ChatResponse, ChatStream, Usage};

const DEFAULT_MAX_TOKENS: u32 = 4096;
const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1";

pub struct OpenAIBackend {
    endpoint: String,
    api_key: String,
    http: reqwest::Client,
}

impl OpenAIBackend {
    pub fn new(endpoint: &str, api_key: &str, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build");
        Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            api_key: api_key.into(),
            http,
        }
    }
}

// ── Wire types ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    messages: Vec<ApiMessage<'a>>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    choices: Vec<ApiChoice>,
    #[serde(default)]
    usage: ApiUsage,
}

#[derive(Debug, Deserialize)]
struct ApiChoice {
    message: ApiChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ApiChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

// ── Trait impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl ChatBackend for OpenAIBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.endpoint);

        let max_tokens = if req.max_tokens == 0 {
            DEFAULT_MAX_TOKENS
        } else {
            req.max_tokens
        };

        // OpenAI Chat Completions takes role/content messages; system goes
        // as a `system` role message at the head, user goes after.
        let mut messages: Vec<ApiMessage> = Vec::with_capacity(2);
        if let Some(s) = req.system {
            messages.push(ApiMessage { role: "system", content: s });
        }
        messages.push(ApiMessage { role: "user", content: req.user });

        let body = ApiRequest {
            model: req.model,
            messages,
            max_tokens,
            temperature: req.temperature,
            stop: req.stop.clone(),
        };

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|source| BackendError::Network {
                provider: "openai",
                source,
            })?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await.unwrap_or_default();
            return Err(map_error(status, &raw_body, req.model));
        }

        let parsed: ApiResponse = resp.json().await.map_err(|e| BackendError::BadResponse {
            provider: "openai",
            message: format!("json parse: {e}"),
        })?;

        let text = parsed
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        Ok(ChatResponse {
            text,
            usage: Usage {
                input_tokens: parsed.usage.prompt_tokens,
                output_tokens: parsed.usage.completion_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                provider: "openai",
                model: req.model.into(),
            },
        })
    }

    async fn generate_stream(&self, _req: ChatRequest<'_>) -> Result<ChatStream> {
        // P4 ships OpenAI as non-streaming only. P5+ may add SSE streaming
        // (OpenAI's SSE format is similar to but not identical to Anthropic).
        anyhow::bail!("OpenAIBackend::generate_stream not implemented in P4")
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

fn map_error(status: reqwest::StatusCode, raw_body: &str, model: &str) -> anyhow::Error {
    use reqwest::StatusCode;
    match status {
        StatusCode::UNAUTHORIZED => BackendError::Unauthorized { provider: "openai" }.into(),
        StatusCode::TOO_MANY_REQUESTS => BackendError::RateLimited {
            provider: "openai",
            retry_after_secs: None,
        }
        .into(),
        StatusCode::NOT_FOUND => BackendError::ModelNotFound {
            provider: "openai",
            model: model.into(),
        }
        .into(),
        s if s.is_server_error() => BackendError::ServerError {
            provider: "openai",
            status: s.as_u16(),
        }
        .into(),
        _ => BackendError::BadResponse {
            provider: "openai",
            message: format!("HTTP {status}: {raw_body}"),
        }
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req<'a>(model: &'a str, user: &'a str) -> ChatRequest<'a> {
        ChatRequest {
            model,
            system: None,
            user,
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[test]
    fn provider_name_is_openai() {
        let b = OpenAIBackend::new("http://unused", "k", Duration::from_millis(100));
        assert_eq!(b.provider_name(), "openai");
    }

    #[tokio::test]
    async fn generate_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":4,"completion_tokens":1}}"#),
            )
            .mount(&server)
            .await;

        let b = OpenAIBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("gpt-4o-mini", "hi")).await.unwrap();
        assert_eq!(r.text, "hi");
        assert_eq!(r.usage.input_tokens, 4);
        assert_eq!(r.usage.output_tokens, 1);
        assert_eq!(r.usage.provider, "openai");
    }

    #[tokio::test]
    async fn generate_request_includes_system_role() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#),
            )
            .mount(&server)
            .await;
        let b = OpenAIBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let mut r = req("gpt-4o-mini", "hi");
        r.system = Some("you are a tester");
        let _ = b.generate(r).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].get("role").and_then(|v| v.as_str()), Some("system"));
        assert_eq!(messages[0].get("content").and_then(|v| v.as_str()), Some("you are a tester"));
        assert_eq!(messages[1].get("role").and_then(|v| v.as_str()), Some("user"));
    }

    #[tokio::test]
    async fn generate_401_maps_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let b = OpenAIBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
        let r = b.generate(req("gpt-4o-mini", "hi")).await;
        let err = r.err().unwrap();
        let typed = err
            .downcast_ref::<BackendError>()
            .expect("typed BackendError");
        assert!(matches!(
            typed,
            BackendError::Unauthorized { provider: "openai" }
        ));
    }

    #[tokio::test]
    async fn generate_429_maps_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let b = OpenAIBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("gpt-4o-mini", "hi")).await;
        let err = r.err().unwrap();
        assert!(matches!(
            err.downcast_ref::<BackendError>().unwrap(),
            BackendError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn generate_stream_bails_in_p4() {
        let b = OpenAIBackend::new("http://unused", "k", Duration::from_millis(100));
        let r = b.generate_stream(req("gpt-4o-mini", "hi")).await;
        assert!(r.is_err());
    }
}
```

**Step 2: Wire `pub mod openai;`** in `mur-core/src/conversations/backend/mod.rs`:

```rust
pub mod anthropic;
pub mod factory;
pub mod mock;
pub mod ollama;
pub mod openai;          // ← new
pub mod retry;
pub mod telemetry;
```

**Step 3: Run tests**

```bash
cargo test -p mur-core --lib conversations::backend::openai 2>&1 | tail -10
```

Expected: 6 PASS.

**Step 4: Lint + commit**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings 2>&1 | tail -5
git add mur-core/src/conversations/backend/openai.rs mur-core/src/conversations/backend/mod.rs
git commit -m "$(cat <<'EOF'
feat(backend): OpenAIBackend — non-streaming, OpenAI-compatible

Adds OpenAIBackend implementing ChatBackend. Posts to <endpoint>/chat/
completions with Bearer auth and the standard OpenAI Chat Completions
request shape (role/content messages with system at the head).

The endpoint field doubles as the OpenAI-compatible-provider switch:
- https://api.openai.com/v1 (default) → OpenAI
- https://openrouter.ai/api/v1 → OpenRouter
- https://api.together.xyz/v1 → Together
- http://localhost:8080/v1 → local llama.cpp / vLLM / etc.

P4 ships non-streaming only; generate_stream bails. Streaming can land
in P5 if a streaming caller materializes (none today — extract_llm,
learn, starter all need only non-streaming).

Usage maps OpenAI's prompt_tokens/completion_tokens onto ChatBackend's
input_tokens/output_tokens. Cache fields stay 0 (OpenAI Chat Completions
has no equivalent of Anthropic's cache_control).

Error mapping: 401 → Unauthorized, 429 → RateLimited, 404 → ModelNotFound,
5xx → ServerError, anything else → BadResponse.

6 wiremock tests cover: provider_name, happy-path with usage, system
message in body, 401 + 429 mapping, generate_stream bails.

Refs spec §12. Plan task 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `GeminiBackend`

**Files:**
- Create: `mur-core/src/conversations/backend/gemini.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (`pub mod gemini;`)

**Difference from OpenAI:** Gemini's API expects the API key as a `?key=...` query string parameter (NOT a header). The URL shape is `<endpoint>/v1beta/models/<model>:generateContent?key=<api_key>`. Wire types differ: `system_instruction` + `contents[]` with `parts[]` arrays of `{text}`. Response: `candidates[0].content.parts[0].text`.

**Step 1: Create the module + 5 tests**

Same shape as Task 2. Tests mirror OpenAI ones. Key test: `generate_request_puts_api_key_in_query_string`. Default endpoint: `"https://generativelanguage.googleapis.com"`.

Provider name: `"gemini"`.

`generate_stream` bails in P4 (Gemini SSE is doable but no caller today).

`Usage`: Gemini's response includes `usageMetadata { promptTokenCount, candidatesTokenCount }`. Map promptTokenCount → input_tokens, candidatesTokenCount → output_tokens. (Verify exact field names by reading the existing `gemini_complete` in `mur-core/src/llm.rs:329-369` — current code IGNORES usage; new backend should not.)

**Step 2: Wire `pub mod gemini;`**

**Step 3: Run + lint + commit**

```bash
cargo test -p mur-core --lib conversations::backend::gemini 2>&1 | tail -10
cargo fmt -p mur-core && cargo clippy -p mur-core --lib --tests -- -D warnings
git add mur-core/src/conversations/backend/gemini.rs mur-core/src/conversations/backend/mod.rs
git commit -m "$(cat <<'EOF'
feat(backend): GeminiBackend — Google Generative Language API

Adds GeminiBackend implementing ChatBackend. POSTs to <endpoint>/v1beta/
models/<model>:generateContent?key=<api_key>. Differs from OpenAI in:
- API key in URL query string, not Authorization header
- Wire types: system_instruction + contents[].parts[].text (vs.
  role/content messages)
- Usage: usageMetadata.{promptTokenCount, candidatesTokenCount}

P4 ships non-streaming only; generate_stream bails. Streaming can land
in P5 if a caller materializes (Gemini SSE is supported but no current
mur caller uses it).

5 wiremock tests cover: provider_name, happy-path with usage, API key
in query string, 401 + 429 mapping, generate_stream bails.

Refs spec §12. Plan task 3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Extend `factory::build_raw` to dispatch on openai / openrouter / gemini + add `default_key_env` fallback

**Files:**
- Modify: `mur-core/src/conversations/backend/factory.rs`

**Why two changes in one task:** they're entangled — the new providers need API keys, and the cleanest way to support `BackendConfig.api_key_env: None` (which `LlmConfig::to_backend_config()` will produce in Task 5 when the user hasn't set `api_key_env` explicitly) is `default_key_env(provider)` fallback. Doing them separately would require an awkward intermediate state.

**Step 1: Write the failing tests**

Append to `factory.rs` `mod tests`:

```rust
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn openai_provider_returns_openai_backend_when_key_present() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_OPENAI_KEY", "sk-synthetic") };
        let cfg = BackendConfig {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_OPENAI_KEY".into()),
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "openai");
        unsafe { std::env::remove_var("MUR_TEST_OPENAI_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gemini_provider_returns_gemini_backend_when_key_present() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_GEMINI_KEY", "synthetic") };
        let cfg = BackendConfig {
            provider: "gemini".into(),
            model: "gemini-pro-3".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_GEMINI_KEY".into()),
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "gemini");
        unsafe { std::env::remove_var("MUR_TEST_GEMINI_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn openrouter_provider_aliases_to_openai_with_default_endpoint() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_OR_KEY", "sk-or-v1-synthetic") };
        let cfg = BackendConfig {
            provider: "openrouter".into(),
            model: "anthropic/claude-haiku-4-5".into(),
            endpoint: None, // factory should auto-set https://openrouter.ai/api/v1
            api_key_env: Some("MUR_TEST_OR_KEY".into()),
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        // openrouter alias surfaces as "openai" (it IS an OpenAI-compat backend)
        assert_eq!(b.provider_name(), "openai");
        unsafe { std::env::remove_var("MUR_TEST_OR_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_uses_default_env_when_api_key_env_field_missing() {
        // P4 behavior change: factory now falls back to default_key_env when
        // api_key_env is None — so LlmConfig users without explicit api_key_env
        // (the historical default for anthropic) keep working.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "synthetic-default") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None, // factory uses default_key_env("anthropic") = "ANTHROPIC_API_KEY"
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "anthropic");
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
    }
```

The existing `anthropic_provider_errors_when_api_key_env_field_missing` test (lines ~150-170 of factory.rs) needs updating: under the new behavior, `api_key_env: None` is no longer an error if the env var named by `default_key_env("anthropic")` (= `ANTHROPIC_API_KEY`) is missing — but still errors if both the field AND the default env are missing. Rename to `anthropic_provider_errors_when_default_env_var_unset_and_api_key_env_field_missing` and adjust:

```rust
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_errors_when_default_env_var_unset_and_api_key_env_field_missing() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err(), "should error when default env var ANTHROPIC_API_KEY is unset and api_key_env is None");
    }
```

**Step 2: Run to confirm fail**

```bash
cargo test -p mur-core --lib conversations::backend::factory 2>&1 | tail -15
```

Expected: 4 new tests fail to compile (factory doesn't know openai/openrouter/gemini); 1 existing test fails (the api_key_env: None case is now no longer immediate error).

**Step 3: Update factory.rs**

Add a `default_key_env` helper at the top of the module:

```rust
/// Default env var name for an API-key-bearing provider. Mirrors the
/// historical mur-common::config::LlmConfig fallback so users with
/// `provider: anthropic` and no explicit `api_key_env:` keep working
/// after P4 migrates them onto BackendConfig.
fn default_key_env(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "LLM_API_KEY",
    }
}

fn resolve_api_key(cfg: &BackendConfig) -> Result<String> {
    let env_var = cfg
        .api_key_env
        .as_deref()
        .unwrap_or_else(|| default_key_env(&cfg.provider));
    std::env::var(env_var).map_err(|_| {
        anyhow::anyhow!(
            "{} backend env var {env_var} is not set or not readable",
            cfg.provider
        )
    })
}
```

Update `build_raw` match arms — extend with:

```rust
        "openai" => {
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg.endpoint.as_deref().unwrap_or("https://api.openai.com/v1");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::openai::OpenAIBackend::new(endpoint, &api_key, timeout))
        }
        "openrouter" => {
            // Alias → OpenAI-compatible at openrouter.ai. Default endpoint applied
            // when not overridden.
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg.endpoint.as_deref().unwrap_or("https://openrouter.ai/api/v1");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::openai::OpenAIBackend::new(endpoint, &api_key, timeout))
        }
        "gemini" => {
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg.endpoint.as_deref().unwrap_or("https://generativelanguage.googleapis.com");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::gemini::GeminiBackend::new(endpoint, &api_key, timeout))
        }
```

Update the existing `anthropic` arm to also use `resolve_api_key(cfg)` (replaces the inline `cfg.api_key_env.as_deref().ok_or_else(...)` block — net simplification).

**Step 4: Run tests**

```bash
cargo test -p mur-core --lib conversations::backend::factory 2>&1 | tail -15
```

Expected: PASS — 4 new tests + the renamed existing one.

**Step 5: Lint + commit**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
git add mur-core/src/conversations/backend/factory.rs
git commit -m "$(cat <<'EOF'
feat(factory): dispatch openai / openrouter / gemini + default_key_env

Extends factory::build_raw provider dispatch from {ollama, anthropic} to
{ollama, anthropic, openai, openrouter, gemini}. openrouter is an alias
that reuses OpenAIBackend with a different default endpoint.

Adds resolve_api_key(cfg) helper that falls back to default_key_env(provider)
when BackendConfig.api_key_env is None. This is a (desirable) behavior
change for the existing anthropic arm: previously bailed if api_key_env
was None; now uses ANTHROPIC_API_KEY by default. Matches the historical
LlmConfig behavior so P4 task 5's LlmConfig::to_backend_config conversion
preserves user behavior.

5 new factory tests cover the three new providers + the openrouter alias
+ the default-env fallback. The existing api_key_env-missing test renamed
+ updated to assert the BOTH-missing case (default env var also unset).

Refs spec §12. Plan task 4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `LlmConfig::to_backend_config()` conversion + tests

**Files:**
- Modify: `mur-common/src/config.rs` (impl block on `LlmConfig`)

**Step 1: Write failing tests**

Append to `mur-common/src/config.rs` `mod tests`:

```rust
    #[test]
    fn llm_config_to_backend_config_anthropic_passthrough() {
        let cfg = LlmConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            openai_url: None,
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "anthropic");
        assert_eq!(b.model, "claude-haiku-4-5");
        assert_eq!(b.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(b.endpoint, None);
        assert_eq!(b.timeout_secs, None);
    }

    #[test]
    fn llm_config_to_backend_config_openai_url_maps_to_endpoint() {
        let cfg = LlmConfig {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            api_key_env: None,
            openai_url: Some("https://api.together.xyz/v1".into()),
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "openai");
        assert_eq!(b.endpoint.as_deref(), Some("https://api.together.xyz/v1"));
        assert_eq!(b.api_key_env, None); // factory will fall back to OPENAI_API_KEY
    }

    #[test]
    fn llm_config_to_backend_config_ollama_openai_url_maps_to_endpoint() {
        let cfg = LlmConfig {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            api_key_env: None,
            openai_url: Some("http://192.168.1.10:11434".into()),
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.endpoint.as_deref(), Some("http://192.168.1.10:11434"));
    }

    #[test]
    fn llm_config_to_backend_config_unknown_with_openai_url_aliases_to_openai() {
        // Historical LlmConfig allowed provider="custom" + openai_url to act as
        // an OpenAI-compatible passthrough. Preserve that by re-tagging as
        // "openai" so factory dispatches to OpenAIBackend.
        let cfg = LlmConfig {
            provider: "custom-name".into(),
            model: "some-model".into(),
            api_key_env: Some("CUSTOM_KEY".into()),
            openai_url: Some("https://my-proxy.local/v1".into()),
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "openai", "unknown provider + openai_url should alias to openai");
        assert_eq!(b.endpoint.as_deref(), Some("https://my-proxy.local/v1"));
    }
```

**Step 2: Run to confirm fail**

```bash
cargo test -p mur-common --lib -- llm_config_to_backend_config 2>&1 | tail -10
```

Expected: FAIL — `to_backend_config` doesn't exist.

**Step 3: Implement the conversion**

In `mur-common/src/config.rs`, add an impl block on `LlmConfig` (find it via `grep -n "pub struct LlmConfig" mur-common/src/config.rs`):

```rust
impl LlmConfig {
    /// Convert legacy LlmConfig (used by extract_llm, learn, capture/starter)
    /// into a BackendConfig that the new ChatBackend factory consumes.
    /// Mapping:
    /// - `provider` 1:1, except: unknown providers WITH openai_url become "openai"
    ///   (preserves the historical LlmConfig::llm_complete fall-through for
    ///   OpenAI-compatible passthrough proxies).
    /// - `model` 1:1.
    /// - `api_key_env` 1:1 (factory's resolve_api_key falls back to
    ///   default_key_env(provider) when None — preserves LlmConfig behavior).
    /// - `openai_url` → `endpoint` (semantic rename; same string semantics).
    /// - `timeout_secs` always None (factory defaults to 120s — matches
    ///   the historical 60s reqwest default behavior closely enough).
    pub fn to_backend_config(&self) -> BackendConfig {
        let provider = match self.provider.as_str() {
            "anthropic" | "openai" | "openrouter" | "gemini" | "ollama" => self.provider.clone(),
            _ if self.openai_url.is_some() => "openai".into(),
            other => other.into(), // factory will reject with "unsupported provider"
        };
        BackendConfig {
            provider,
            model: self.model.clone(),
            endpoint: self.openai_url.clone(),
            api_key_env: self.api_key_env.clone(),
            timeout_secs: None,
        }
    }
}
```

**Step 4: Run tests + lint + commit**

```bash
cargo test -p mur-common --lib -- llm_config_to_backend_config 2>&1 | tail -10
cargo fmt -p mur-common && cargo fmt --check -p mur-common
cargo clippy -p mur-common --lib --tests -- -D warnings
git add mur-common/src/config.rs
git commit -m "$(cat <<'EOF'
feat(common): LlmConfig::to_backend_config conversion for P4 callers

Adds the bridge that lets P4 task 6/7/8 call sites convert their existing
LlmConfig (used by extract_llm, learn, capture/starter) into a
BackendConfig at the call site without changing on-disk config shape.

Mapping:
- provider 1:1, EXCEPT unknown providers with openai_url alias to "openai"
  (preserves the historical LlmConfig::llm_complete fall-through path for
  OpenAI-compatible passthrough proxies).
- model 1:1.
- api_key_env 1:1 (factory's resolve_api_key falls back to default_key_env
  when None — preserves LlmConfig behavior).
- openai_url → endpoint (semantic rename, same string semantics).
- timeout_secs None (factory defaults to 120s).

4 tests cover: anthropic passthrough, openai_url maps to endpoint,
ollama openai_url maps to endpoint, unknown-with-openai_url aliases.

Existing ~/.mur/config.yaml `llm:` sections continue to deserialize
unchanged. The conversion runs in-process per call.

Refs spec §12. Plan task 5.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Migrate `extract_llm.rs` — replace manual retry envelope with `RetryingBackend`

**Files:**
- Modify: `mur-core/src/extract_llm.rs` (~30 lines deleted, ~10 added)

**Why this task is bigger than 7+8:** `extract_llm.rs` has its own 3-attempt retry loop classifying on `529/overload/timeout/503` substrings. P0's `RetryingBackend` already covers this via typed `BackendError` dispatch on `ServerError(5xx)/Timeout/RateLimited`. So the migration not only swaps `llm_complete` for `backend.generate(...)` — it also DELETES the manual loop. Net delete ~20 lines.

**Step 1: Write failing tests**

`extract_llm.rs` currently has tests that mock `llm_complete` somehow. Read the existing test module first (`grep -n "mod tests" mur-core/src/extract_llm.rs`). The new tests should verify the migration preserves behavior:

```rust
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn extract_llm_retries_503_then_succeeds_via_chat_backend() {
        // Mock backend that fails twice then succeeds. Verify extract_llm
        // sees the success after retries — proving RetryingBackend has
        // replaced the manual envelope.
        // (Implementation pattern: refer to retry.rs::generate_stream_retries_connect_then_succeeds
        // for the FailNTimes idiom.)
        // ...
    }
```

(Keep the test compact — the pattern is well-established by P0/P2 tests.)

**Step 2: Update `call_llm_with_retry` (or whatever the extract_llm.rs function is named)**

Around line 200 of `extract_llm.rs`, find the retry loop. Replace the entire `for attempt in 0..3 { ... }` block with:

```rust
    let backend_cfg = llm_config.to_backend_config();
    let backend = match crate::conversations::backend::factory::build_for_stage(&backend_cfg, "extract_llm") {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("LLM backend init failed: {e:#}, falling back to logic extraction");
            return Ok(logic_result);
        }
    };
    let req = crate::conversations::backend::ChatRequest {
        model: &backend_cfg.model,
        system: Some(system_prompt),
        user: &user_prompt,
        max_tokens: 0, // backend default
        temperature: None,
        stop: vec![],
        cache_system: false,
        cache_user_prefix: None,
    };
    match backend.generate(req).await {
        Ok(resp) => match parse_llm_response(&resp.text) {
            Some(parsed) => {
                let _ = save_cache(&cache_path, &parsed);
                Ok(build_workflow_from_llm(session_id, events, &parsed))
            }
            None => {
                tracing::warn!("LLM returned invalid JSON, falling back to logic extraction");
                tracing::debug!(
                    "LLM response (first 2000 chars): {}",
                    &resp.text[..resp.text.len().min(2000)]
                );
                let _ = std::fs::write("/tmp/mur-llm-response.txt", &resp.text);
                Ok(logic_result)
            }
        },
        Err(e) => {
            tracing::warn!("LLM call failed (after backend retries): {e:#}, falling back to logic extraction");
            Ok(logic_result)
        }
    }
```

(`build_for_stage` wraps in `RetryingBackend` AND `TelemetryBackend` — so the manual retry loop is gone AND extract_llm calls now appear in `mur conversations cost-report`. Two birds.)

Update `use crate::llm::llm_complete;` → remove (no longer needed).

**Step 3: Run tests**

```bash
cargo test -p mur-core --lib extract_llm 2>&1 | tail -15
cargo test -p mur-core --bin mur 2>&1 | tail -5
```

Expected: PASS.

**Step 4: Lint + commit**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
git add mur-core/src/extract_llm.rs
git commit -m "$(cat <<'EOF'
refactor(extract): migrate extract_llm to ChatBackend + delete manual retry

Replaces extract_llm.rs::call_llm_with_retry's hand-rolled 3-attempt loop
(classifying on 529/overload/timeout/503 substrings) with
factory::build_for_stage(&cfg.to_backend_config(), "extract_llm"), which
wraps the backend in RetryingBackend → typed BackendError dispatch on
{Timeout, ServerError(5xx), RateLimited}.

Net delete: ~25 lines (the manual loop + transient-error classifier).

Side benefit: extract_llm calls now flow through TelemetryBackend, so
they appear in `mur conversations cost-report` under stage "extract_llm".
Previously invisible to telemetry.

Soft-fail behavior preserved: any backend error (after retries exhausted)
falls back to logic_result, same as the pre-P4 code.

Refs spec §12. Plan task 6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Migrate `cmd/learn.rs` — single call site swap

**Files:**
- Modify: `mur-core/src/cmd/learn.rs` (~5 lines)

**Step 1: Update the single call site**

Around line 122, replace:
```rust
        match llm::llm_complete(&config.llm, &system, &prompt).await {
```
with:
```rust
        let backend_cfg = config.llm.to_backend_config();
        let backend = crate::conversations::backend::factory::build_for_stage(&backend_cfg, "learn")?;
        let req = crate::conversations::backend::ChatRequest {
            model: &backend_cfg.model,
            system: Some(&system),
            user: &prompt,
            max_tokens: 0,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        match backend.generate(req).await {
            Ok(resp) => {
                let response = resp.text;
```

(The inner block continues working with the `response` String identical to before.)

Drop the `use crate::llm;` import at line 7.

**Step 2: Run tests + lint + commit**

```bash
cargo test -p mur-core --lib learn 2>&1 | tail -10
cargo build -p mur-core --bin mur 2>&1 | tail -3
cargo fmt -p mur-core && cargo clippy -p mur-core --lib --tests -- -D warnings
git add mur-core/src/cmd/learn.rs
git commit -m "$(cat <<'EOF'
refactor(learn): migrate cmd_learn to ChatBackend trait

Single call site swap. config.llm.to_backend_config() converts the
legacy LlmConfig into BackendConfig in-process; factory::build_for_stage
wraps in retry+telemetry. Stage tag "learn".

Behavior identical for users with provider:anthropic/openai/gemini/ollama
in their llm config (which is everyone — there's no other shape today).

Refs spec §12. Plan task 7.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Migrate `capture/starter.rs` — single call site swap

**Files:**
- Modify: `mur-core/src/capture/starter.rs` (~5 lines)

**Step 1: Update line 779**

Replace:
```rust
    match crate::llm::llm_complete(config, system, &prompt).await {
```
with the same pattern as Task 7, stage tag `"starter"`. The `config` parameter type may need updating from `&LlmConfig` to call `config.to_backend_config()` inline.

**Step 2: Run tests + lint + commit**

```bash
cargo test -p mur-core --lib capture::starter 2>&1 | tail -10
cargo build -p mur-core --bin mur 2>&1 | tail -3
cargo fmt -p mur-core && cargo clippy -p mur-core --lib --tests -- -D warnings
git add mur-core/src/capture/starter.rs
git commit -m "$(cat <<'EOF'
refactor(starter): migrate emergence detection to ChatBackend trait

Final single call site swap. Stage tag "starter".

After this commit, all four legacy llm.rs callers (extract_llm, cmd_learn,
capture/starter, cmd_doctor for is_reasoning_model) have moved off
crate::llm. Task 9 deletes mur-core/src/llm.rs.

Refs spec §12. Plan task 8.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Delete `mur-core/src/llm.rs`

**Files:**
- Delete: `mur-core/src/llm.rs`
- Modify: `mur-core/src/lib.rs` (remove `pub mod llm;`)

**Step 1: Verify no remaining callers**

```bash
grep -rn "use crate::llm\|crate::llm::\|llm::llm_complete" /Users/david/Projects/mur-p4-plan/mur-core/src/ 2>&1 | grep -v "src/llm.rs:" | grep -v "src/conversations/" | head -10
```

Expected: empty output. Anything that returns is a missed migration; STOP and fix the missed call site before continuing.

(The exclusion of `src/conversations/` covers the `crate::conversations::backend::*` paths that are unrelated to the legacy `crate::llm`.)

**Step 2: Delete the file**

```bash
rm /Users/david/Projects/mur-p4-plan/mur-core/src/llm.rs
```

**Step 3: Update lib.rs**

Find and remove the `pub mod llm;` line (or `mod llm;` — read first).

**Step 4: Verify the build**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace -- --test-threads=1 2>&1 | tee /tmp/p4-task9-test.log | tail -20
```

Expected: clean build, all tests pass. Any compile error means a stub use or test still references `crate::llm::*` — find via the previous grep + grep for `crate::llm::` directly.

**Step 5: Commit**

```bash
git add -A mur-core/src/llm.rs mur-core/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(core): delete mur-core/src/llm.rs (524 lines)

Closing commit of P4. All four legacy callers (extract_llm, cmd_learn,
capture/starter, cmd_doctor for is_reasoning_model) have migrated to
ChatBackend in tasks 1, 6, 7, 8. The OpenAI + Gemini + OpenRouter
provider impls landed in tasks 2-4. is_reasoning_model relocated to
mur-common::llm in task 1.

Net delete after P4 (vs. P3 baseline):
  + ~250 lines  OpenAIBackend + GeminiBackend + tests + factory arms
  + ~50 lines   LlmConfig::to_backend_config + tests
  + ~30 lines   migrated call sites (extract_llm + learn + starter)
  - ~30 lines   extract_llm manual retry envelope (RetryingBackend covers)
  - 524 lines   mur-core/src/llm.rs deleted
  -------
  ~ -224 LOC net (deeper delete than spec's ~100 estimate)

After P4 the conversations subsystem owns the only LLM-routing layer
in mur-core; mur-agent-runtime is independent and out of scope.

Closes spec §12 P4 row. Plan task 9.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: End-to-end verification (no commit)

**Files:** none modified.

**Step 1: Full workspace build + test**

```bash
cd /Users/david/Projects/mur-p4-plan && cargo build --workspace 2>&1 | tail -3
cd /Users/david/Projects/mur-p4-plan && cargo test --workspace -- --test-threads=1 2>&1 | tee /tmp/p4-final-test.log | tail -30
```

Expected: clean build, all tests pass.

**Step 2: Workspace clippy + fmt**

```bash
cd /Users/david/Projects/mur-p4-plan && cargo fmt --check
cd /Users/david/Projects/mur-p4-plan && cargo clippy -p mur-core --all-targets -- -D warnings
cd /Users/david/Projects/mur-p4-plan && cargo clippy -p mur-common --all-targets -- -D warnings
```

Expected: clean. (Workspace-wide clippy may surface the pre-existing companion_enums.rs issue from earlier phases — not your concern.)

**Step 3: Smoke test cost-report includes new stage tags**

After running mur-mode tests, check that `cost-report` table can render the new stage tags. (Empty rows are fine — point is the aggregation doesn't choke on `extract_llm`/`learn`/`starter`.)

```bash
cd /Users/david/Projects/mur-p4-plan && TMPDIR=$(mktemp -d) && HOME="$TMPDIR" /Users/david/Projects/mur-p4-plan/target/debug/mur conversations cost-report --since 7d 2>&1 | head -10
```

Expected: empty table, no panic.

**Step 4: Smoke test `mur learn` against mock backend**

```bash
TMPDIR=$(mktemp -d) && HOME="$TMPDIR" MUR_LLM_MOCK=1 /Users/david/Projects/mur-p4-plan/target/debug/mur learn --help 2>&1 | head -5
```

Expected: clap help text for `mur learn`. (Full e2e of `mur learn` requires a fixture session transcript; out of scope for smoke.)

**Step 5: Smoke test the OpenAI/Gemini wire shapes (optional, costs $0)**

Wiremock-based; no API costs. The unit tests in tasks 2 + 3 already exercise these paths. Nothing extra needed.

**Step 6: Report**

Summary for human reviewer:
- 9 commits on `feat/cloud-llm-backend-p4-plan` after the docs commit
- New: `OpenAIBackend`, `GeminiBackend`, factory dispatch arms for openai/openrouter/gemini, `LlmConfig::to_backend_config()` bridge, `default_key_env(provider)` fallback in factory
- Migrated: `extract_llm` (with retry-envelope deletion), `cmd_learn`, `capture::starter`, `cmd::misc::cmd_doctor` (is_reasoning_model relocated)
- Deleted: `mur-core/src/llm.rs` (524 lines), `pub mod llm;` from lib.rs
- Behavior: identical for users with existing `~/.mur/config.yaml` `llm:` sections (provider=anthropic/openai/gemini/ollama, optional openai_url for OpenAI-compat). Subtle improvement: `extract_llm` calls now visible in `cost-report` via the `extract_llm` stage tag.
- Test count delta: ~16 new tests across openai backend, gemini backend, factory provider arms, LlmConfig::to_backend_config conversion. Existing tests preserved.

---

## Out of scope — explicitly deferred

Do **not** implement any of these in P4:

- Streaming for OpenAI / Gemini — neither current caller streams; `generate_stream` bails
- Migrating `mur-agent-runtime/src/llm/*` — separate subsystem with its own model registry
- Adding OpenAI/Gemini prices to `cost-report`'s price table — defer until usage shows up
- Bedrock / Vertex / Foundry — declined non-goal
- Auto-migrating `~/.mur/config.yaml` from `llm:` to `backend:` — `to_backend_config` runs in-process per call
- Re-routing `mur-agent-runtime` agent calls through `factory::build_for_stage` — separate cleanup
- Moving `default_key_env` into `mur-common` — it's an implementation detail of `factory::build_raw`'s key resolution; lives in `factory.rs` for proximity

If an instruction in this plan tempts you to touch these, **stop and ask** — it means the plan or spec needs amendment.
