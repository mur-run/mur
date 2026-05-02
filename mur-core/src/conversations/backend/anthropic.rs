//! Anthropic Claude API backend. Raw HTTP via reqwest — no Rust SDK
//! exists for Anthropic. Non-streaming only in P1; streaming lands in P2.
//!
//! See spec §5.2.

#![allow(dead_code)] // wired by factory + compact.extractive in Tasks 6 & 7.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{BackendError, ChatBackend, ChatChunk, ChatRequest, ChatResponse, ChatStream, Usage};

const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicBackend {
    endpoint: String,
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicBackend {
    /// Construct from explicit api_key + endpoint. Pulls api_key from
    /// the env var named in BackendConfig.api_key_env at the factory
    /// boundary; this constructor takes the resolved key.
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
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<ApiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<String>,
    /// Always send `{type: "disabled"}` on Opus 4.6+ so we don't pay
    /// for implicit adaptive thinking. Older models accept it as a no-op.
    thinking: ApiThinking,
}

#[derive(Debug, Serialize)]
struct ApiMessage<'a> {
    role: &'a str, // "user"
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ApiThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    content: Vec<ApiContentBlock>,
    usage: ApiUsage,
    #[allow(dead_code)] // future telemetry
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    error: ApiErrorBody,
}

#[derive(Debug, Default, Deserialize)]
struct ApiErrorBody {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

// ── Trait impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl ChatBackend for AnthropicBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/v1/messages", self.endpoint);

        // Sampling param removal on Opus 4.7 per claude-api skill —
        // sending temperature on Opus 4.7 returns 400.
        let temperature = if req.model.starts_with("claude-opus-4-7") {
            if req.temperature.is_some() {
                tracing::debug!(
                    model = req.model,
                    "dropping temperature for Opus 4.7 (sampling params 400 on this model)"
                );
            }
            None
        } else {
            req.temperature
        };

        let body = ApiRequest {
            model: req.model,
            max_tokens: if req.max_tokens == 0 {
                DEFAULT_MAX_TOKENS
            } else {
                req.max_tokens
            },
            system: req.system,
            messages: vec![ApiMessage {
                role: "user",
                content: req.user,
            }],
            temperature,
            stop_sequences: req.stop.clone(),
            thinking: ApiThinking { kind: "disabled" },
        };

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

        let parsed: ApiResponse = resp.json().await.map_err(|e| BackendError::BadResponse {
            provider: "anthropic",
            message: format!("json parse: {e}"),
        })?;

        // Concatenate all text blocks; ignore non-text variants.
        let text = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<String>();

        Ok(ChatResponse {
            text,
            usage: Usage {
                input_tokens: parsed.usage.input_tokens,
                output_tokens: parsed.usage.output_tokens,
                cache_creation_input_tokens: parsed.usage.cache_creation_input_tokens,
                cache_read_input_tokens: parsed.usage.cache_read_input_tokens,
                provider: "anthropic",
                model: req.model.into(),
            },
        })
    }

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        use futures::stream::StreamExt;
        let url = format!("{}/v1/messages", self.endpoint);

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
            "system": req.system,
            "temperature": temperature,
            "stop_sequences": if req.stop.is_empty() { serde_json::Value::Null } else { serde_json::json!(req.stop) },
        });
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

        let model = req.model.to_string();
        let byte_stream = resp.bytes_stream();
        let chunk_stream = futures::stream::unfold(
            (byte_stream, String::new(), None::<Usage>, false, model),
            move |(mut inner, mut buf, mut final_usage, done, model)| async move {
                if done {
                    return None;
                }
                loop {
                    if let Some(end) = buf.find("\n\n") {
                        let block: String = buf.drain(..=end + 1).collect();
                        match parse_sse_block(&block, &model) {
                            SseEvent::TextDelta(text) => {
                                return Some((
                                    Ok(ChatChunk {
                                        delta: text,
                                        usage: None,
                                    }),
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
                                    Ok(ChatChunk {
                                        delta: String::new(),
                                        usage,
                                    }),
                                    (inner, buf, None, true, model),
                                ));
                            }
                            SseEvent::Ignore => continue,
                            SseEvent::Error(e) => {
                                return Some((Err(e), (inner, buf, None, true, model)));
                            }
                        }
                    }
                    match inner.next().await {
                        Some(Ok(bytes)) => match std::str::from_utf8(&bytes) {
                            Ok(s) => buf.push_str(&s.replace("\r\n", "\n")),
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
                            // EOF without `message_stop`. Salvage: parse any partial trailing
                            // block to see if it carries a FinalUsage we'd otherwise lose.
                            if !buf.trim().is_empty() {
                                if let SseEvent::FinalUsage(u) = parse_sse_block(&buf, &model) {
                                    final_usage = Some(u);
                                }
                                buf.clear();
                            }
                            // Emit final usage if we have it (from earlier message_delta or
                            // the salvage above), else end cleanly.
                            if let Some(u) = final_usage.take() {
                                return Some((
                                    Ok(ChatChunk {
                                        delta: String::new(),
                                        usage: Some(u),
                                    }),
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

    fn provider_name(&self) -> &'static str {
        "anthropic"
    }

    fn supports_caching(&self) -> bool {
        // P3 wiring; cache_system / cache_user_prefix hints are silently
        // ignored in P1.
        false
    }
}

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
    let mut data = String::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
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

/// Map an HTTP error response to the appropriate BackendError variant.
fn map_error(status: reqwest::StatusCode, body: &str, model: &str) -> anyhow::Error {
    let parsed: Option<ApiError> = serde_json::from_str(body).ok();
    let typed = match status.as_u16() {
        401 => BackendError::Unauthorized {
            provider: "anthropic",
        },
        404 => BackendError::ModelNotFound {
            provider: "anthropic",
            model: model.into(),
        },
        429 => BackendError::RateLimited {
            provider: "anthropic",
            retry_after_secs: None,
        },
        s @ 500..=599 => BackendError::ServerError {
            provider: "anthropic",
            status: s,
        },
        _ => BackendError::BadResponse {
            provider: "anthropic",
            message: parsed
                .map(|p| format!("{}: {}", p.error.kind, p.error.message))
                .unwrap_or_else(|| format!("status {status}: {body}")),
        },
    };
    typed.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req<'a>(model: &'a str, user: &'a str) -> ChatRequest<'a> {
        ChatRequest {
            model,
            system: None,
            user,
            max_tokens: 16,
            temperature: Some(0.5),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[tokio::test]
    async fn provider_name_is_anthropic() {
        let b = AnthropicBackend::new("http://127.0.0.1:1", "k", Duration::from_millis(100));
        assert_eq!(b.provider_name(), "anthropic");
        assert!(!b.supports_caching());
    }

    #[tokio::test]
    async fn happy_path_returns_text_and_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_x",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "hello world"}],
                "model": "claude-haiku-4-5",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 5, "output_tokens": 7}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "test-key", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await.unwrap();
        assert_eq!(r.text, "hello world");
        assert_eq!(r.usage.input_tokens, 5);
        assert_eq!(r.usage.output_tokens, 7);
        assert_eq!(r.usage.provider, "anthropic");
        assert_eq!(r.usage.model, "claude-haiku-4-5");
    }

    #[tokio::test]
    async fn unauthorized_401_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"type": "authentication_error", "message": "invalid x-api-key"}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await;
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
    async fn not_found_404_maps_to_model_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"type": "not_found_error", "message": "model not found"}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-bogus", "hi")).await;
        let err = r.err().unwrap();
        let typed = err
            .downcast_ref::<BackendError>()
            .expect("typed BackendError");
        match typed {
            BackendError::ModelNotFound { provider, model } => {
                assert_eq!(*provider, "anthropic");
                assert_eq!(model, "claude-bogus");
            }
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn server_error_500_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err
            .downcast_ref::<BackendError>()
            .expect("typed BackendError");
        assert!(matches!(
            typed,
            BackendError::ServerError { status: 500, .. }
        ));
    }

    #[tokio::test]
    async fn rate_limited_429_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err
            .downcast_ref::<BackendError>()
            .expect("typed BackendError");
        assert!(matches!(typed, BackendError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn opus_4_7_drops_temperature() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let _ = b.generate(req("claude-opus-4-7", "hi")).await.unwrap();

        // Verify the request body explicitly — this is more robust than
        // body_json matchers (which can be brittle to serde_json key ordering).
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(
            body.get("model").and_then(|v| v.as_str()),
            Some("claude-opus-4-7")
        );
        assert!(
            body.get("temperature").is_none(),
            "temperature should be dropped for Opus 4.7, but found: {:?}",
            body.get("temperature")
        );
        assert_eq!(
            body.get("thinking")
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("disabled")
        );
    }

    #[tokio::test]
    async fn non_opus_4_7_keeps_temperature() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let _ = b.generate(req("claude-haiku-4-5", "hi")).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(
            body.get("temperature").and_then(|v| v.as_f64()),
            Some(0.5),
            "temperature should be preserved for Haiku"
        );
    }

    #[tokio::test]
    async fn streaming_happy_path_emits_text_deltas_then_final_usage() {
        use futures::StreamExt;
        let server = MockServer::start().await;
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
                assert!(
                    final_usage.is_none(),
                    "usage should arrive only on final chunk"
                );
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
                    .set_body_string("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
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

    #[tokio::test]
    async fn streaming_handles_crlf_block_separators() {
        use futures::StreamExt;
        let server = MockServer::start().await;
        // Same body as happy-path but with \r\n line endings (some proxies emit CRLF).
        let sse_body = "\
event: content_block_delta\r\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\r\n\
\r\n\
event: message_delta\r\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\r\n\
\r\n\
event: message_stop\r\n\
data: {\"type\":\"message_stop\"}\r\n\
\r\n";
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let mut stream = b
            .generate_stream(req("claude-haiku-4-5", "hi"))
            .await
            .unwrap();
        let mut text = String::new();
        let mut got_usage = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            text.push_str(&chunk.delta);
            if chunk.usage.is_some() {
                got_usage = true;
            }
        }
        assert_eq!(text, "hi");
        assert!(
            got_usage,
            "expected final usage chunk despite CRLF separators"
        );
    }

    #[tokio::test]
    async fn streaming_salvages_final_usage_on_truncated_eof() {
        use futures::StreamExt;
        let server = MockServer::start().await;
        // message_delta arrives but the closing \n\n and message_stop are missing
        // (server cut connection mid-stream). The salvage path should still emit usage.
        let sse_body = "\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}\n";
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let mut stream = b
            .generate_stream(req("claude-haiku-4-5", "hi"))
            .await
            .unwrap();
        let mut final_usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            if let Some(u) = chunk.usage {
                final_usage = Some(u);
            }
        }
        let u = final_usage.expect("expected usage salvaged from truncated stream");
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.output_tokens, 2);
    }

    /// One real-API integration test gated on ANTHROPIC_API_KEY.
    /// Run via `cargo test -- --ignored` only.
    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY env var; costs ~$0.0001 per run"]
    async fn live_anthropic_haiku_responds() {
        let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
            panic!("ANTHROPIC_API_KEY must be set to run this --ignored test");
        };
        let b = AnthropicBackend::new("https://api.anthropic.com", &key, Duration::from_secs(30));
        let r = b
            .generate(ChatRequest {
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
        assert!(!r.text.is_empty());
        assert!(r.usage.input_tokens > 0);
        assert!(r.usage.output_tokens > 0);
        assert_eq!(r.usage.provider, "anthropic");
    }
}
