//! Google Generative Language API backend (Gemini). Non-streaming only.
//! Differs from OpenAI in: API key in URL query string (not header),
//! system_instruction + contents wire shape, usageMetadata for tokens.
//! See P4 plan task 3.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{BackendError, ChatBackend, ChatRequest, ChatResponse, ChatStream, Usage};

pub struct GeminiBackend {
    endpoint: String,
    api_key: String,
    http: reqwest::Client,
}

impl GeminiBackend {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<ApiContent<'a>>,
    contents: Vec<ApiContent<'a>>,
}

#[derive(Debug, Serialize)]
struct ApiContent<'a> {
    parts: Vec<ApiPart<'a>>,
}

#[derive(Debug, Serialize)]
struct ApiPart<'a> {
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(default)]
    candidates: Vec<ApiCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: ApiUsage,
}

#[derive(Debug, Deserialize)]
struct ApiCandidate {
    content: ApiCandidateContent,
}

#[derive(Debug, Deserialize)]
struct ApiCandidateContent {
    #[serde(default)]
    parts: Vec<ApiResponsePart>,
}

#[derive(Debug, Deserialize)]
struct ApiResponsePart {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct ApiUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u64,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u64,
    /// Subset of `promptTokenCount`, not an additional count — Google bills it
    /// at ~1/10 the input rate, so it has to be split out rather than summed.
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: u64,
}

#[async_trait]
impl ChatBackend for GeminiBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.endpoint, req.model, self.api_key
        );

        let body = ApiRequest {
            system_instruction: req.system.map(|s| ApiContent {
                parts: vec![ApiPart { text: s }],
            }),
            contents: vec![ApiContent {
                parts: vec![ApiPart { text: req.user }],
            }],
        };

        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|source| BackendError::Network {
                provider: "gemini",
                source,
            })?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await.unwrap_or_default();
            return Err(map_error(status, &raw_body, req.model));
        }

        let parsed: ApiResponse = resp.json().await.map_err(|e| BackendError::BadResponse {
            provider: "gemini",
            message: format!("json parse: {e}"),
        })?;

        let text = parsed
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_default();

        let cached = parsed.usage_metadata.cached_content_token_count;
        Ok(ChatResponse {
            text,
            usage: Usage {
                // Downstream cost math (and the Anthropic backend it shares a
                // shape with) treats input and cache tokens as disjoint, so the
                // cached slice comes out of the prompt count instead of being
                // charged twice at the full input rate.
                input_tokens: parsed
                    .usage_metadata
                    .prompt_token_count
                    .saturating_sub(cached),
                output_tokens: parsed.usage_metadata.candidates_token_count,
                // Gemini bills cache writes per storage-hour, not per token.
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: cached,
                provider: "gemini",
                model: req.model.into(),
            },
        })
    }

    async fn generate_stream(&self, _req: ChatRequest<'_>) -> Result<ChatStream> {
        anyhow::bail!("GeminiBackend::generate_stream not implemented in P4")
    }

    fn provider_name(&self) -> &'static str {
        "gemini"
    }
}

fn map_error(status: reqwest::StatusCode, raw_body: &str, model: &str) -> anyhow::Error {
    use reqwest::StatusCode;
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            // Gemini returns 403 for bad/missing API key (not 401 like Anthropic/OpenAI)
            BackendError::Unauthorized { provider: "gemini" }.into()
        }
        StatusCode::TOO_MANY_REQUESTS => BackendError::RateLimited {
            provider: "gemini",
            retry_after_secs: None,
        }
        .into(),
        StatusCode::NOT_FOUND => BackendError::ModelNotFound {
            provider: "gemini",
            model: model.into(),
        }
        .into(),
        s if s.is_server_error() => BackendError::ServerError {
            provider: "gemini",
            status: s.as_u16(),
        }
        .into(),
        _ => BackendError::BadResponse {
            provider: "gemini",
            message: format!("HTTP {status}: {raw_body}"),
        }
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
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
    fn provider_name_is_gemini() {
        let b = GeminiBackend::new("http://unused", "k", Duration::from_millis(100));
        assert_eq!(b.provider_name(), "gemini");
    }

    #[tokio::test]
    async fn generate_happy_path_with_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-pro-3:generateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":1}}"#),
            )
            .mount(&server)
            .await;

        let b = GeminiBackend::new(&server.uri(), "synthetic-key", Duration::from_secs(5));
        let r = b.generate(req("gemini-pro-3", "hi")).await.unwrap();
        assert_eq!(r.text, "hi");
        assert_eq!(r.usage.input_tokens, 4);
        assert_eq!(r.usage.output_tokens, 1);
        assert_eq!(r.usage.provider, "gemini");
    }

    #[tokio::test]
    async fn cached_prompt_tokens_are_split_out_not_double_counted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-pro-3:generateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":1,"cachedContentTokenCount":80}}"#),
            )
            .mount(&server)
            .await;

        let b = GeminiBackend::new(&server.uri(), "synthetic-key", Duration::from_secs(5));
        let r = b.generate(req("gemini-pro-3", "hi")).await.unwrap();
        // 100 prompt tokens of which 80 were cached: 20 fresh, 80 at the cache
        // rate. Summing them instead would bill 180 tokens for a 100-token call.
        assert_eq!(r.usage.input_tokens, 20);
        assert_eq!(r.usage.cache_read_input_tokens, 80);
        assert_eq!(r.usage.cache_creation_input_tokens, 0);
    }

    #[tokio::test]
    async fn generate_request_puts_api_key_in_query_string() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-pro-3:generateContent"))
            .and(query_param("key", "synthetic-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]}}],"usageMetadata":{}}"#),
            )
            .mount(&server)
            .await;

        let b = GeminiBackend::new(&server.uri(), "synthetic-key", Duration::from_secs(5));
        let r = b.generate(req("gemini-pro-3", "hi")).await;
        assert!(
            r.is_ok(),
            "request should succeed when key matches the query_param matcher"
        );
    }

    #[tokio::test]
    async fn generate_403_maps_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-pro-3:generateContent"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let b = GeminiBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
        let r = b.generate(req("gemini-pro-3", "hi")).await;
        let err = r.err().unwrap();
        assert!(matches!(
            err.downcast_ref::<BackendError>().unwrap(),
            BackendError::Unauthorized { provider: "gemini" }
        ));
    }

    #[tokio::test]
    async fn generate_429_maps_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-pro-3:generateContent"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let b = GeminiBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("gemini-pro-3", "hi")).await;
        assert!(matches!(
            r.err().unwrap().downcast_ref::<BackendError>().unwrap(),
            BackendError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn generate_stream_bails_in_p4() {
        let b = GeminiBackend::new("http://unused", "k", Duration::from_millis(100));
        let r = b.generate_stream(req("gemini-pro-3", "hi")).await;
        assert!(r.is_err());
    }
}
