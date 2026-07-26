//! OpenAI Chat Completions API backend. Also covers OpenAI-compatible
//! providers (OpenRouter, Together, Fireworks, etc.) via `endpoint` override.
//! Non-streaming only — see spec §5.x, plan task 2.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{BackendError, ChatBackend, ChatRequest, ChatResponse, ChatStream, Usage};

const DEFAULT_MAX_TOKENS: u32 = 4096;

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
    #[serde(default)]
    prompt_tokens_details: ApiPromptDetails,
}

/// `cached_tokens` is a subset of `prompt_tokens`, not an extra count — it is
/// billed at ~1/10 the input rate, so it has to be split out rather than summed.
#[derive(Debug, Default, Deserialize)]
struct ApiPromptDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[async_trait]
impl ChatBackend for OpenAIBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.endpoint);

        let max_tokens = if req.max_tokens == 0 {
            DEFAULT_MAX_TOKENS
        } else {
            req.max_tokens
        };

        let mut messages: Vec<ApiMessage> = Vec::with_capacity(2);
        if let Some(s) = req.system {
            messages.push(ApiMessage {
                role: "system",
                content: s,
            });
        }
        messages.push(ApiMessage {
            role: "user",
            content: req.user,
        });

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

        let cached = parsed.usage.prompt_tokens_details.cached_tokens;
        Ok(ChatResponse {
            text,
            usage: Usage {
                input_tokens: parsed.usage.prompt_tokens.saturating_sub(cached),
                output_tokens: parsed.usage.completion_tokens,
                // Prompt caching is automatic; there is no per-token write fee.
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: cached,
                provider: "openai",
                model: req.model.into(),
            },
        })
    }

    async fn generate_stream(&self, _req: ChatRequest<'_>) -> Result<ChatStream> {
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
        assert_eq!(
            messages[0].get("role").and_then(|v| v.as_str()),
            Some("system")
        );
        assert_eq!(
            messages[0].get("content").and_then(|v| v.as_str()),
            Some("you are a tester")
        );
        assert_eq!(
            messages[1].get("role").and_then(|v| v.as_str()),
            Some("user")
        );
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
