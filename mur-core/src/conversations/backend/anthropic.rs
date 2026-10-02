//! Anthropic Claude API backend. Raw HTTP via reqwest — no Rust SDK
//! exists for Anthropic. Non-streaming only in P1; streaming lands in P2.
//!
//! See spec §5.2.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

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

/// The `temperature` to send for `model`, dropping it on models that reject
/// sampling parameters with a 400.
///
/// Sampling params (`temperature` / `top_p` / `top_k`) were removed from Opus
/// 4.7 onward and from the Fable/Sonnet-5 line. The previous check was
/// `starts_with("claude-opus-4-7")` — a single hardcoded model, which went
/// stale the moment a newer model became the default and would have started
/// 400ing every request that carries a temperature. Keeping the list in one
/// named place makes the next model addition a one-line edit in an obvious
/// spot rather than a silent outage.
fn sampling_temperature(model: &str, requested: Option<f32>) -> Option<f32> {
    const REJECTS_SAMPLING: &[&str] = &[
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    if REJECTS_SAMPLING.iter().any(|m| model.starts_with(m)) {
        if requested.is_some() {
            tracing::debug!(
                model,
                "dropping temperature — sampling params 400 on this model"
            );
        }
        return None;
    }
    requested
}

// ── Trait impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl ChatBackend for AnthropicBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/v1/messages", self.endpoint);

        let temperature = sampling_temperature(req.model, req.temperature);

        let body = build_request_body(&req, temperature, false /* stream */);

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

        let temperature = sampling_temperature(req.model, req.temperature);
        let body = build_request_body(&req, temperature, true /* stream */);

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
        true
    }
}

/// Build the JSON request body for /v1/messages.
///
/// When `cache_system` is true and a system prompt is present, emits `system`
/// as a single-block array with `cache_control: {type: ephemeral}` (per spec
/// §5.2 caching invariants). When `cache_user_prefix` is `Some(n)`, splits
/// the user content at byte n and emits a two-block content array with the
/// breakpoint on the prefix block. When neither hint is set, emits the
/// legacy shape: `system` as a plain string, `content` as a plain string.
///
/// The `stream` flag adds `"stream": true` for SSE responses.
///
/// LIMITATION: `cache_user_prefix` is a byte offset. If it falls in the
/// middle of a multi-byte UTF-8 codepoint, the slice operations below will
/// panic. The `n > 0 && n < req.user.len()` guard ensures the offset is
/// in-range but does not enforce a UTF-8 char boundary. No call site sets
/// `cache_user_prefix` today (P3 Tasks 6-7 only set `cache_system`), so
/// this is a known followup, not an immediate hazard.
fn build_request_body(
    req: &ChatRequest<'_>,
    temperature: Option<f32>,
    stream: bool,
) -> serde_json::Value {
    use serde_json::json;

    let max_tokens = if req.max_tokens == 0 {
        DEFAULT_MAX_TOKENS
    } else {
        req.max_tokens
    };

    // System: array form (with cache_control) only when cache_system && system present.
    let system_value = match (req.cache_system, req.system) {
        (true, Some(s)) => json!([
            {"type": "text", "text": s, "cache_control": {"type": "ephemeral"}}
        ]),
        (_, Some(s)) => json!(s),
        (_, None) => serde_json::Value::Null,
    };

    // User content: two-block array (cached prefix + volatile suffix) only when
    // cache_user_prefix is Some and the offset is in range. Otherwise plain string.
    let content_value = match req.cache_user_prefix {
        Some(n) if n > 0 && n < req.user.len() && req.user.is_char_boundary(n) => {
            let prefix = &req.user[..n];
            let suffix = &req.user[n..];
            json!([
                {"type": "text", "text": prefix, "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": suffix},
            ])
        }
        _ => json!(req.user),
    };

    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": content_value}],
        "thinking": {"type": "disabled"},
    });

    let map = body.as_object_mut().unwrap();
    if !system_value.is_null() {
        map.insert("system".into(), system_value);
    }
    if let Some(t) = temperature {
        map.insert("temperature".into(), json!(t));
    }
    if !req.stop.is_empty() {
        map.insert("stop_sequences".into(), json!(req.stop));
    }
    if stream {
        map.insert("stream".into(), json!(true));
    }
    body
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
mod tests;
