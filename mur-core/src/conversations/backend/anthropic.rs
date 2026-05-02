//! Anthropic Claude API backend. Raw HTTP via reqwest — no Rust SDK
//! exists for Anthropic. Non-streaming only in P1; streaming lands in P2.
//!
//! See spec §5.2.

#![allow(dead_code)] // wired by factory + compact.extractive in Tasks 6 & 7.

use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use super::{BackendError, ChatBackend, ChatChunk, ChatRequest, ChatResponse, Usage};

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

    async fn generate_stream(
        &self,
        _req: ChatRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        anyhow::bail!("AnthropicBackend::generate_stream lands in P2")
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
    async fn streaming_bails_in_p1() {
        let b = AnthropicBackend::new("http://127.0.0.1:1", "k", Duration::from_millis(100));
        let r = b.generate_stream(req("claude-haiku-4-5", "hi")).await;
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("P2"));
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
