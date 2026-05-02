# Cloud LLM Backend P2 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add streaming support to the `ChatBackend` trait — real `OllamaBackend::generate_stream` (replacing the P0 `bail!` stub) plus a from-scratch SSE parser for `AnthropicBackend::generate_stream` — and migrate `ask::generate` (the answer-generation path that streams tokens to stdout) onto the trait via `factory::build`. Streaming call sites (just `ask::generate` for now) become provider-agnostic; non-streaming call sites already migrated by P0/P1 are unchanged.

**Architecture:** `OllamaBackend::generate_stream` wraps the existing `OllamaClient::generate_stream` (NDJSON parser) and adapts the `Result<String>` chunks into `ChatChunk` items. `AnthropicBackend::generate_stream` issues `POST /v1/messages` with `"stream": true` and parses the SSE response body (`event:`+`data:` framed blocks) using the same buffer-and-split shape as `ollama.rs::generate_stream`, dispatching on `data` JSON `type` field. `RetryingBackend::generate_stream` retries the **connection establishment** but propagates mid-stream errors without retry (mid-stream retry would re-send the prompt and waste tokens). `ask::generate::stream_answer` constructs the backend via `factory::build` instead of `OllamaClient::new` directly, so user config controls which provider serves the answer.

**Tech Stack:** Rust 2024 · `reqwest` (already a dep, supports `bytes_stream()`) · `tokio` · `tracing` · `anyhow` for application errors · `thiserror` for `BackendError` (no new variants — existing `BadResponse` covers SSE parse errors) · `wiremock = "0.6"` (already a dev-dep) for mocking SSE response bodies. **No new crates.** SSE parser is ~40 lines hand-rolled — same buffer-and-split shape as `mur-core/src/conversations/ollama.rs::generate_stream`.

**Spec:** `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` — §4.2 (`ChatChunk`/`ChatStream`), §5.2 (AnthropicBackend), §9 (SSE parser detail), §12 (phase boundaries). P3 (prompt caching wiring + cost telemetry + migrate remaining 3 non-streaming call sites + cost-report command), P4 (delete `mur-core/src/llm.rs`) are out of scope.

**Out of scope for P2** — explicitly do not implement:
- Migrate `compact.abstractive`, `summarize::rollup`, `ask::abstractive::compress_hit` — P3 (alongside prompt caching wiring, since those non-streaming call sites benefit most from caching)
- Prompt caching wiring on `AnthropicBackend` (`cache_system` / `cache_user_prefix` hints stay defaulted-to-false; `supports_caching()` stays `false`) — P3
- Cost telemetry / `mur conversations cost-report` command — P3
- Migrate `learn`/`extract_llm` — P4
- Mid-stream retry semantics (`RetryingBackend::generate_stream` retries the connect attempt only) — possible future enhancement
- Switching default ask model to cloud — `~/.mur/config.yaml` defaults stay at Ollama unless user opts in
- Doctor enhancements specific to streaming — P1's probes already cover the model-existence check

**Plan deviations flagged from spec:** none. P2 implements exactly what spec §12 describes for this phase.

---

## Task 0: Verify foundation + read context (no commit)

**Files:** none modified.

**Step 1: Confirm P0 + P1 are on main**

Run:
```bash
git log --oneline | grep -E "79e4b72|f692594" | head -2
```
Expected:
```
f692594 feat: cloud-LLM backend P1 (AnthropicBackend + per-stage routing + retry envelope) (#91)
79e4b72 refactor(conversations): introduce ChatBackend trait (P0 of cloud-LLM rollout) (#80)
```
If either SHA is missing, **STOP** — P2 assumes both phases are landed.

**Step 2: Read the existing streaming code**

- `mur-core/src/conversations/ollama.rs:161-243` (`OllamaClient::generate_stream` — NDJSON parser using `futures::stream::unfold` over `bytes_stream()`)
- `mur-core/src/conversations/ask/generate.rs` end-to-end (the call site that streams tokens to stdout — currently builds `OllamaClient` directly)
- `mur-core/src/conversations/backend/{ollama,anthropic,retry,factory}.rs` (the trait surface from P0/P1)

**Step 3: Read the master spec's SSE parser notes**

`docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` §5.2 (AnthropicBackend streaming) and §9 (SSE parser sketch). The parser dispatches on `data` JSON `type` field:

| `type` | Action |
|---|---|
| `content_block_delta` with `delta.type == "text_delta"` | yield `ChatChunk { delta: text, usage: None }` |
| `message_delta` | capture `usage` for the final chunk |
| `message_stop` | yield `ChatChunk { delta: "", usage: Some(final_usage) }` and end |
| anything else (`message_start`, `content_block_start`, `content_block_stop`, `ping`) | ignore |

**Step 4: No commit** — context-loading only.

---

## Task 1: Real `OllamaBackend::generate_stream`

**Files:**
- Modify: `mur-core/src/conversations/backend/ollama.rs` (replace `generate_stream` body, add tests)

**Step 1: Update the failing test that asserts the P0 stub bails**

In `mur-core/src/conversations/backend/ollama.rs`, find the test `generate_stream_returns_unimplemented_error_in_p0` and **delete it** (the stub it tested is being replaced).

Add a new test in its place:

```rust
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn generate_stream_propagates_connection_failure() {
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
        // generate_stream may return Err immediately OR return a stream that errors on first poll.
        // Either is acceptable — the "unreachable endpoint" path doesn't have to fail at the
        // same layer for both backends, just somewhere in the stream lifecycle.
        match b.generate_stream(req).await {
            Err(_) => { /* failed at connect — fine */ }
            Ok(mut s) => {
                use futures::StreamExt;
                let first = s.next().await.expect("expected at least one stream item");
                assert!(first.is_err(), "stream should yield an Err for unreachable endpoint");
            }
        }
    }
```

**Step 2: Run tests to confirm the OLD `_in_p0` test is gone and the new one fails**

Run: `cargo test -p mur-core --lib conversations::backend::ollama 2>&1 | tail -10`

Expected: 3 tests pass (provider_name, propagates_connection_failure_for_generate, the new generate_stream_propagates_connection_failure compiles but may fail or pass depending on whether the bail! is still there).

**Step 3: Replace the `generate_stream` body**

Replace the existing `generate_stream` impl (the `bail!("OllamaBackend::generate_stream not wired in P0")` stub):

```rust
    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        use crate::conversations::ollama::{GenerateOptions, GenerateRequest};
        use futures::StreamExt;
        let g_req = GenerateRequest {
            model: req.model,
            prompt: req.user,
            system: req.system,
            stream: true,
            options: GenerateOptions {
                temperature: req.temperature,
                top_p: None,
                num_predict: Some(req.max_tokens),
                stop: req.stop.clone(),
            },
        };
        let inner_stream = self.client.generate_stream(g_req).await?;
        // Adapt the existing OllamaClient::generate_stream `Stream<Item = Result<String>>`
        // to ChatStream `Stream<Item = Result<ChatChunk>>`. Ollama doesn't surface usage
        // in its NDJSON stream (the final `done: true` line carries it but the existing
        // client discards it), so usage stays None for streamed Ollama responses in P2.
        // P3 may revisit if cost telemetry needs per-chunk usage from Ollama.
        let chunks = inner_stream.map(|item| {
            item.map(|delta| ChatChunk {
                delta,
                usage: None,
            })
        });
        Ok(Box::pin(chunks))
    }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib conversations::backend::ollama 2>&1 | tail -10`

Expected: PASS.

**Step 5: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/ollama.rs
git commit -m "$(cat <<'EOF'
feat(backend): real OllamaBackend::generate_stream (replaces P0 bail stub)

Wraps the existing OllamaClient::generate_stream NDJSON parser and
adapts Result<String> chunks to ChatChunk items. Ollama's NDJSON stream
carries text in the `response` field of each line; the existing client
emits those as String chunks via Stream::unfold. We map them 1:1 into
ChatChunk { delta, usage: None }.

Usage stays None for streamed Ollama responses in P2 — Ollama's final
`done: true` line carries token counts but OllamaClient currently
discards it. P3 may revisit if cost telemetry needs per-chunk usage.

Replaces the P0 stub:
- generate_stream_returns_unimplemented_error_in_p0 → deleted
- generate_stream_propagates_connection_failure → new test verifying
  unreachable endpoint surfaces as either immediate Err or first-poll Err

Refs spec §4.2 + §5.1. Plan task 1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: SSE parser for `AnthropicBackend::generate_stream`

**Files:**
- Modify: `mur-core/src/conversations/backend/anthropic.rs` (replace `generate_stream` body, add tests, add SSE parser private fn)

**Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` in `mur-core/src/conversations/backend/anthropic.rs`:

```rust
    #[tokio::test]
    async fn streaming_happy_path_emits_text_deltas_then_final_usage() {
        use futures::StreamExt;
        let server = MockServer::start().await;
        // Multi-event SSE body covering message_start, content_block_start,
        // 3 content_block_deltas, content_block_stop, message_delta with
        // usage, message_stop. Each block is `event: <name>\ndata: <json>\n\n`.
        let sse_body = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_x\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" \"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&server)
            .await;

        let b = AnthropicBackend::new(&server.uri(), "test-key", Duration::from_secs(5));
        let mut stream = b
            .generate_stream(req("claude-haiku-4-5", "hi"))
            .await
            .unwrap();

        let mut text = String::new();
        let mut final_usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            text.push_str(&chunk.delta);
            if let Some(u) = chunk.usage {
                assert!(final_usage.is_none(), "usage should arrive only on final chunk");
                final_usage = Some(u);
            }
        }
        assert_eq!(text, "Hello world");
        let u = final_usage.expect("expected final usage chunk");
        assert_eq!(u.input_tokens, 5);
        assert_eq!(u.output_tokens, 3);
        assert_eq!(u.provider, "anthropic");
        assert_eq!(u.model, "claude-haiku-4-5");
    }

    #[tokio::test]
    async fn streaming_unauthorized_401_maps_to_typed_error_at_connect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
        let r = b.generate_stream(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err
            .downcast_ref::<BackendError>()
            .expect("typed BackendError");
        assert!(matches!(
            typed,
            BackendError::Unauthorized {
                provider: "anthropic"
            }
        ));
    }

    #[tokio::test]
    async fn streaming_rate_limited_429_maps_to_typed_error_at_connect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate_stream(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err
            .downcast_ref::<BackendError>()
            .expect("typed BackendError");
        assert!(matches!(typed, BackendError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn streaming_request_body_includes_stream_true() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                    ),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let _ = b
            .generate_stream(req("claude-haiku-4-5", "hi"))
            .await
            .unwrap();
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(body.get("stream").and_then(|v| v.as_bool()), Some(true));
    }
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-core --lib conversations::backend::anthropic 2>&1 | tail -15`

Expected: FAIL — `streaming_*` tests can't pass while `generate_stream` still bails.

**Step 3: Implement the SSE parser + `generate_stream`**

Replace the existing `generate_stream` body in `anthropic.rs`:

```rust
    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        use futures::stream::StreamExt;
        let url = format!("{}/v1/messages", self.endpoint);

        // Same param prep as non-streaming generate (Opus 4.7 sampling drop +
        // thinking-disabled), but with stream:true.
        let temperature = if req.model.starts_with("claude-opus-4-7") {
            None
        } else {
            req.temperature
        };
        let body = serde_json::json!({
            "model": req.model,
            "max_tokens": if req.max_tokens == 0 { DEFAULT_MAX_TOKENS } else { req.max_tokens },
            "messages": [{"role": "user", "content": req.user}],
            "stream": true,
            "thinking": {"type": "disabled"},
            // Conditionally include optional fields so wiremock body inspection is clean.
            "system": req.system,
            "temperature": temperature,
            "stop_sequences": if req.stop.is_empty() { serde_json::Value::Null } else { serde_json::json!(req.stop) },
        });
        // Strip nulls so the request looks like the non-streaming path's serde-skipped form.
        let body = strip_null_values(body);

        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|source| BackendError::Network {
                provider: "anthropic",
                source,
            })?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await.unwrap_or_default();
            return Err(map_error(status, &raw_body, req.model));
        }

        let model = req.model.to_string(); // owned copy for the closure below
        let byte_stream = resp.bytes_stream();
        let chunk_stream = futures::stream::unfold(
            (byte_stream, String::new(), None::<Usage>, false, model),
            move |(mut inner, mut buf, mut final_usage, done, model)| async move {
                if done {
                    return None;
                }
                loop {
                    // Look for a complete SSE event: `\n\n` separator.
                    if let Some(end) = buf.find("\n\n") {
                        let block: String = buf.drain(..=end + 1).collect();
                        match parse_sse_block(&block, &model) {
                            SseEvent::TextDelta(text) => {
                                return Some((
                                    Ok(ChatChunk { delta: text, usage: None }),
                                    (inner, buf, final_usage, false, model),
                                ));
                            }
                            SseEvent::FinalUsage(u) => {
                                final_usage = Some(u);
                                continue;
                            }
                            SseEvent::Stop => {
                                let usage = final_usage.take();
                                return Some((
                                    Ok(ChatChunk { delta: String::new(), usage }),
                                    (inner, buf, None, true, model),
                                ));
                            }
                            SseEvent::Ignore => continue,
                            SseEvent::Error(e) => {
                                return Some((Err(e), (inner, buf, None, true, model)));
                            }
                        }
                    }
                    // Need more bytes.
                    match inner.next().await {
                        Some(Ok(bytes)) => match std::str::from_utf8(&bytes) {
                            Ok(s) => buf.push_str(s),
                            Err(e) => {
                                return Some((
                                    Err(BackendError::BadResponse {
                                        provider: "anthropic",
                                        message: format!("non-utf8 in SSE stream: {e}"),
                                    }
                                    .into()),
                                    (inner, buf, None, true, model),
                                ));
                            }
                        },
                        Some(Err(e)) => {
                            return Some((
                                Err(BackendError::Network {
                                    provider: "anthropic",
                                    source: e,
                                }
                                .into()),
                                (inner, buf, None, true, model),
                            ));
                        }
                        None => {
                            // EOF without `message_stop` — emit final usage if we have it,
                            // else end cleanly.
                            if let Some(u) = final_usage.take() {
                                return Some((
                                    Ok(ChatChunk { delta: String::new(), usage: Some(u) }),
                                    (inner, buf, None, true, model),
                                ));
                            }
                            return None;
                        }
                    }
                }
            },
        );
        Ok(Box::pin(chunk_stream))
    }
```

Add the SSE parser as a private fn in the same file (above `map_error` is fine):

```rust
/// Parsed SSE event variants we care about. Everything else maps to Ignore.
enum SseEvent {
    TextDelta(String),
    FinalUsage(Usage),
    Stop,
    Ignore,
    Error(anyhow::Error),
}

/// Parse one SSE block (`event: <name>\ndata: <json>\n\n`).
/// Multi-line `data:` is concatenated per spec; we expect Anthropic to send
/// a single `data:` line per event.
fn parse_sse_block(block: &str, model: &str) -> SseEvent {
    // Find the `data:` line(s); ignore `event:`/`id:`/`retry:` and comments.
    let mut data = String::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            // Per SSE spec, strip exactly one optional leading space.
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(payload);
        }
    }
    if data.is_empty() {
        return SseEvent::Ignore;
    }
    let v: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            return SseEvent::Error(
                BackendError::BadResponse {
                    provider: "anthropic",
                    message: format!("SSE data not JSON: {e} ({data:?})"),
                }
                .into(),
            );
        }
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("content_block_delta") => {
            let text = v
                .get("delta")
                .and_then(|d| {
                    if d.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                        d.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                SseEvent::Ignore
            } else {
                SseEvent::TextDelta(text)
            }
        }
        Some("message_delta") => {
            // message_delta carries the final usage in `usage`. There may be
            // multiple message_deltas in theory; we take the latest.
            let usage_v = v.get("usage");
            if let Some(u) = usage_v {
                SseEvent::FinalUsage(Usage {
                    input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                    output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                    cache_creation_input_tokens: u
                        .get("cache_creation_input_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0),
                    cache_read_input_tokens: u
                        .get("cache_read_input_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0),
                    provider: "anthropic",
                    model: model.into(),
                })
            } else {
                SseEvent::Ignore
            }
        }
        Some("message_stop") => SseEvent::Stop,
        // message_start, content_block_start, content_block_stop, ping, error — ignore.
        // (error events carry their own JSON payload but we don't currently surface them
        // — they're rare and the connection will close anyway.)
        _ => SseEvent::Ignore,
    }
}

/// Recursively strip null values from a JSON object so the request body
/// matches the non-streaming serde-derived path's `skip_serializing_if`.
fn strip_null_values(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let filtered: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_null_values(v)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(strip_null_values).collect())
        }
        other => other,
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib conversations::backend::anthropic 2>&1 | tail -20`

Expected: PASS — 4 new streaming tests pass + the 9 existing non-streaming tests still pass + 1 ignored live test.

**Step 5: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/anthropic.rs
git commit -m "$(cat <<'EOF'
feat(backend): AnthropicBackend SSE streaming via hand-rolled parser

Replaces the P1 generate_stream stub with a real SSE parser. Issues
POST /v1/messages with stream:true, parses the response body as
event:/data: framed blocks using the same buffer-and-split pattern as
ollama.rs::generate_stream (no eventsource-stream dep — ~80 lines of
parsing code).

Dispatches on data JSON `type` field per spec §9:
- content_block_delta with delta.type=text_delta → ChatChunk text
- message_delta carries final usage (captured, emitted on Stop)
- message_stop → final ChatChunk with usage, end stream
- message_start/content_block_start/content_block_stop/ping/error → ignored

Connect-time errors (401/404/429/5xx) map to typed BackendError before
the stream starts, same as non-streaming generate(). Mid-stream errors
(network, malformed JSON, non-UTF8 bytes) are propagated as Err items
in the stream — no retry (RetryingBackend handles connect retry only).

4 wiremock tests: happy-path multi-event stream, 401-at-connect maps to
Unauthorized, 429-at-connect maps to RateLimited, request body verifies
stream:true is set.

Plan deviation: spec hinted at eventsource-stream crate as optional;
hand-rolled parser keeps zero new deps. Same shape as the existing
NDJSON parser in ollama.rs.

Refs spec §5.2 + §9. Plan task 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `RetryingBackend::generate_stream` semantics — retry connect, propagate mid-stream

**Files:**
- Modify: `mur-core/src/conversations/backend/retry.rs`

**Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `retry.rs`:

```rust
    #[tokio::test]
    async fn generate_stream_retries_connect_then_succeeds() {
        // Inner backend: fails generate_stream twice with ServerError(503),
        // then succeeds with a single-chunk stream.
        struct StreamFailNTimes {
            fail_n: u32,
            attempts: Arc<AtomicU32>,
        }
        #[async_trait]
        impl ChatBackend for StreamFailNTimes {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("not used")
            }
            async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
                let n = self.attempts.fetch_add(1, Ordering::SeqCst);
                if n < self.fail_n {
                    return Err(BackendError::ServerError {
                        provider: "test",
                        status: 503,
                    }
                    .into());
                }
                let chunk = ChatChunk {
                    delta: "hi".into(),
                    usage: Some(Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        provider: "test",
                        model: req.model.into(),
                    }),
                };
                Ok(Box::pin(futures::stream::iter(vec![Ok(chunk)])))
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }
        let inner = Arc::new(StreamFailNTimes {
            fail_n: 2,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        use futures::StreamExt;
        let mut stream = backend.generate_stream(req()).await.unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.delta, "hi");
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // 1 + 2 retries
    }

    #[tokio::test]
    async fn generate_stream_does_not_retry_on_unauthorized_at_connect() {
        struct AlwaysUnauthorized {
            attempts: Arc<AtomicU32>,
        }
        #[async_trait]
        impl ChatBackend for AlwaysUnauthorized {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("not used")
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Err(BackendError::Unauthorized { provider: "test" }.into())
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }
        let inner = Arc::new(AlwaysUnauthorized {
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate_stream(req()).await;
        assert!(r.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1); // No retries
    }
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-core --lib conversations::backend::retry 2>&1 | tail -10`

Expected: FAIL — `generate_stream` currently passes through without retry, so the retries_connect test sees attempts=1 and fails the assertion.

**Step 3: Update `RetryingBackend::generate_stream`**

Replace the existing `generate_stream` impl in `retry.rs`:

```rust
    async fn generate_stream(
        &self,
        req: ChatRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        // Retry the connect attempt only — mid-stream failures propagate.
        // Mid-stream retry would re-send the prompt and silently waste tokens
        // on a duplicate request. P3+ may revisit if telemetry shows this is
        // a real problem.
        let mut attempt: u32 = 0;
        loop {
            let req_clone = ChatRequest {
                model: req.model,
                system: req.system,
                user: req.user,
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                stop: req.stop.clone(),
                cache_system: req.cache_system,
                cache_user_prefix: req.cache_user_prefix,
            };
            match self.inner.generate_stream(req_clone).await {
                Ok(stream) => return Ok(stream),
                Err(e) => match Self::should_retry(&e, attempt, &self.policy) {
                    Some(delay) => {
                        tracing::warn!(
                            provider = self.inner.provider_name(),
                            attempt = attempt + 1,
                            max_attempts = self.policy.max_attempts,
                            delay_secs = delay.as_secs(),
                            "backend stream connect transient error: {e:#}, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    None => return Err(e),
                },
            }
        }
    }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib conversations::backend::retry 2>&1 | tail -10`

Expected: PASS — 8 retry tests now (6 existing + 2 new).

**Step 5: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 6: Commit**

```bash
git add mur-core/src/conversations/backend/retry.rs
git commit -m "$(cat <<'EOF'
feat(backend): RetryingBackend::generate_stream retries connect only

Mid-stream errors (network drops, malformed SSE, etc.) propagate without
retry — re-sending the prompt mid-stream would silently double-charge
tokens on the inner backend.

Connect-time errors (Timeout/ServerError 5xx/RateLimited) get the same
3-attempt linear backoff treatment as the non-streaming generate path.
Same should_retry helper, same RetryPolicy, same dispatch on typed
BackendError.

2 new tests verify: retries 503 twice then succeeds (3 attempts total),
no retry on Unauthorized at connect (1 attempt, immediate Err).

Refs spec §8.1. Plan task 3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Wire `ask::generate` to `factory::build`

**Files:**
- Modify: `mur-core/src/conversations/ask/generate.rs` (signature change to take `&dyn ChatBackend`)
- Modify: `mur-core/src/conversations/ask/mod.rs` (likely the `ask_stream` function that constructs OllamaClient and calls into generate)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (cmd_ask call site if it constructs OllamaClient directly for the answer path)

**Step 1: Map the existing call chain**

```bash
grep -n "OllamaClient\|generate_stream\|ask_stream" /Users/david/Projects/mur/mur-core/src/conversations/ask/generate.rs /Users/david/Projects/mur/mur-core/src/conversations/ask/mod.rs | head -20
```

Find:
- Where `ask::generate::stream_answer` (or equivalent) accepts an `&OllamaClient` and calls `client.generate_stream(...)`
- Where the caller (likely in `ask::ask_stream` in `ask/mod.rs`) constructs the `OllamaClient`

**Step 2: Refactor `stream_answer` (or equivalent)**

Change the function signature from `(client: &OllamaClient, ...)` to `(backend: &dyn ChatBackend, ...)`. Replace the `client.generate_stream(GenerateRequest{...})` call with `backend.generate_stream(ChatRequest{...})`. Adapt the chunk consumer to handle `ChatChunk { delta, usage }` instead of raw `String`.

The pattern mirrors P0 task 6 (`ask::rewriter` migration) and P1 task 7 (`compact::extractive` migration). See those commits for reference.

**Step 3: Update the caller (`ask::ask_stream` or wherever OllamaClient is constructed for the answer path)**

Replace `OllamaClient::new(&endpoint, ...)` construction with:

```rust
    let answer_cfg = ask_cfg.synthesize_backend();
    let answer_backend = crate::conversations::backend::factory::build(&answer_cfg)?;
    // ... pass answer_backend.as_ref() to stream_answer
```

The `synthesize_backend()` helper from P1 returns the per-stage `BackendConfig` if set, else synthesizes ollama from legacy fields. Behavior is byte-identical for users without `backend:` override in their ask config.

**Step 4: Update tests in `ask/generate.rs`**

Existing tests likely use `OllamaClient` directly. Update them to use `MockBackend::new()` (no env-var needed since MockBackend impls ChatBackend directly) or `OllamaBackend` with an unreachable endpoint.

Pattern from P0 task 6:
```rust
use crate::conversations::backend::ollama::OllamaBackend;
let backend = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(200));
stream_answer(&backend, ...).await
```

**Step 5: Run ask tests**

```bash
cargo test -p mur-core --lib conversations::ask -- --test-threads=1 2>&1 | tail -15
```

Expected: PASS for all ask tests (rewriter from P0 + generate now via trait + others unchanged).

**Step 6: Run integration tests**

```bash
cargo test -p mur-core --test cli_conversations -- --test-threads=1 2>&1 | tail -10
```

Expected: PASS — 24+ integration tests pass. The mock-backed `mur ask` smoke test path now goes through the full trait stack.

**Step 7: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy --workspace -- -D warnings
```

(Workspace clippy may still report the pre-existing `companion_enums.rs` issue per P1 — that's not your concern.)

**Step 8: Commit**

```bash
git add mur-core/src/conversations/ask/generate.rs mur-core/src/conversations/ask/mod.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "$(cat <<'EOF'
refactor(ask): wire generate (answer streaming) to ChatBackend trait

ask::generate::stream_answer now takes &dyn ChatBackend instead of
&OllamaClient. The ask::ask_stream caller constructs the backend via
factory::build using ask_cfg.synthesize_backend() — honors per-stage
ask.backend override and falls back to legacy model + ollama_endpoint
for users without override.

This is the third call-site migration (after P0's ask::rewriter and
P1's compact::extractive) and the first streaming one. Behavior is
byte-identical for users with no ask.backend override (still hits
local Ollama via OllamaBackend). Users who set ask.backend.provider
to anthropic now get cloud-side answer streaming with token-by-token
output via the SSE parser from Task 2.

Refs spec §3 (call-site refactor list). Plan task 4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: End-to-end verification (no commit)

**Files:** none modified.

**Step 1: Full workspace build**

```bash
cargo build --workspace
```

Expected: clean.

**Step 2: Full workspace test**

```bash
cargo test --workspace -- --test-threads=1
```

(Don't pipe through `tail` — the previous P0/P1 sessions hit issues where the output buffered until close. If you must pipe, use `tee /tmp/p2-test.log | tail -200`.)

Expected: all tests pass, exit 0.

**Step 3: Workspace clippy + fmt**

```bash
cargo fmt --check
cargo clippy -p mur-core --all-targets -- -D warnings
```

Expected: both clean. (Workspace `clippy --workspace --all-targets` may still hit the pre-existing `companion_enums.rs` issue — not your concern.)

**Step 4: Smoke test ask with mock**

```bash
MUR_LLM_MOCK=1 cargo run --bin mur --quiet -- ask "what did I ship today?" 2>&1 | head -10
```

Expected: command exits 0, prints either canned no-hits response or some mock streaming output. The full chain now flows through `factory::build → MockBackend → ChatChunk stream`.

**Step 5: Smoke test ask with `MUR_OLLAMA_MOCK=1` (legacy env var)**

```bash
MUR_OLLAMA_MOCK=1 cargo run --bin mur --quiet -- ask "what did I ship today?" 2>&1 | head -10
```

Expected: identical output to step 4.

**Step 6: (Optional) Smoke test ask with synthetic anthropic config**

Create a temp config with `ask.backend.provider: anthropic` and a bogus key, expect the cmd to fail with a 401 error from the real Anthropic API:

```bash
TMPDIR=$(mktemp -d) && mkdir -p "$TMPDIR/.mur"
cat > "$TMPDIR/.mur/config.yaml" <<'YAML'
embedding:
  provider: ollama
  model: qwen3-embedding:0.6b
  dimensions: 1024
  ollama_endpoint: http://localhost:11434
conversations:
  ask:
    backend:
      provider: anthropic
      model: claude-haiku-4-5
      api_key_env: ANTHROPIC_API_KEY
YAML
HOME="$TMPDIR" ANTHROPIC_API_KEY=stub-not-real /Users/david/Projects/mur/target/debug/mur ask "test" 2>&1 | head -10
```

Expected: error mentioning `401 Unauthorized` from Anthropic — confirms the trait stack reaches real Anthropic API on opt-in.

**Step 7: (Optional, costs $0.0001) Live API streaming test**

The P1 `live_anthropic_haiku_responds` test only verifies non-streaming. P2 has no `#[ignore]`d live streaming test by default — add one if you want belt-and-suspenders coverage. Otherwise, the smoke test in step 6 is sufficient evidence the wire format works (request reaches Anthropic, gets a response).

If you want to add it, append to `anthropic.rs` test module:

```rust
    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY env var; costs ~$0.0001 per run"]
    async fn live_anthropic_haiku_streams() {
        use futures::StreamExt;
        let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
            panic!("ANTHROPIC_API_KEY must be set to run this --ignored test");
        };
        let b = AnthropicBackend::new("https://api.anthropic.com", &key, Duration::from_secs(30));
        let mut stream = b
            .generate_stream(ChatRequest {
                model: "claude-haiku-4-5",
                system: Some("You answer in exactly one short sentence."),
                user: "What is 2+2?",
                max_tokens: 32,
                temperature: Some(0.0),
                stop: vec![],
                cache_system: false,
                cache_user_prefix: None,
            })
            .await
            .expect("live API call should succeed");
        let mut text = String::new();
        let mut got_usage = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            text.push_str(&chunk.delta);
            if chunk.usage.is_some() {
                got_usage = true;
            }
        }
        assert!(!text.is_empty(), "expected streamed text");
        assert!(got_usage, "expected final usage chunk");
    }
```

**Step 8: Report**

Summary for human reviewer:
- 4 commits on `feat/cloud-llm-backend-p2-plan` after the docs commit
- New: real `OllamaBackend::generate_stream`, `AnthropicBackend::generate_stream` (SSE parser), `RetryingBackend::generate_stream` retry-on-connect-only
- Migrated: `ask::generate` (third call-site migration; first streaming one)
- Behavior: identical for users with legacy config. Users who set `ask.backend.provider = anthropic` get cloud-side streaming answers.
- Test count delta: +6 new tests (4 anthropic streaming + 2 retry streaming) plus updated ollama generate_stream test.

---

## Out of scope — explicitly deferred to P3+

Do **not** implement any of these in P2:

- Migrating `compact.abstractive`, `summarize::rollup`, `ask::abstractive::compress_hit` — P3 (alongside prompt caching wiring)
- Prompt caching wiring on `AnthropicBackend` (`cache_system` / `cache_user_prefix` hint plumbing into request body) — P3
- `supports_caching()` returning `true` for `AnthropicBackend` — P3
- Cost telemetry, `mur conversations cost-report` — P3
- Migrating `learn`/`extract_llm`, deleting `mur-core/src/llm.rs` — P4
- Mid-stream retry on `RetryingBackend::generate_stream` — possible future enhancement; current behavior (retry connect, propagate mid-stream) is correct for P2
- Switching default ask model to cloud — defaults stay at Ollama
- Doctor enhancements specific to streaming — P1's probes already validate model existence

If an instruction in this plan tempts you to touch these, **stop and ask** — it means the plan or spec needs amendment.
