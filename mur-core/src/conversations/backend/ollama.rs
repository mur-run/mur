//! Adapter wrapping the existing OllamaClient as a ChatBackend.
//! See spec §5.1.

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
                // max_tokens == 0 is the cross-backend "use the backend default"
                // sentinel (Anthropic/OpenAI substitute DEFAULT_MAX_TOKENS;
                // Ollama has no hard default in this layer, so we omit
                // num_predict and let Ollama's server-side default apply —
                // matches the pre-P4 ollama_complete behavior. Sending
                // num_predict: 0 over the wire would make Ollama produce
                // exactly zero tokens (silent empty-response regression).
                num_predict: (req.max_tokens != 0).then_some(req.max_tokens),
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

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        use crate::conversations::ollama::{GenerateOptions, GenerateRequest};
        use futures::StreamExt;
        // Capture the model name as an owned String so the per-chunk closure
        // (which lives as long as the stream) can stamp it into Usage without
        // borrowing from the consumed `req`.
        let model_owned = req.model.to_string();
        let g_req = GenerateRequest {
            model: req.model,
            prompt: req.user,
            system: req.system,
            stream: true,
            options: GenerateOptions {
                temperature: req.temperature,
                top_p: None,
                // Same sentinel as `generate`: 0 == "let Ollama's server-side
                // default apply". See comment in `generate` above.
                num_predict: (req.max_tokens != 0).then_some(req.max_tokens),
                stop: req.stop.clone(),
            },
        };
        let inner_stream = self.client.generate_stream(g_req).await?;
        // Adapt OllamaClient::generate_stream `Stream<Item = Result<OllamaStreamChunk>>`
        // to ChatStream `Stream<Item = Result<ChatChunk>>`. The final NDJSON line
        // (done:true) carries prompt_eval_count + eval_count; OllamaClient surfaces
        // those as `Some(OllamaUsage)` on the final chunk only, which we map into
        // the trait-level `Usage` here (cache fields stay 0 — Ollama has no caching).
        let chunks = inner_stream.map(move |item| {
            item.map(|chunk| ChatChunk {
                delta: chunk.delta,
                usage: chunk.usage.map(|u| Usage {
                    input_tokens: u.prompt_eval_count,
                    output_tokens: u.eval_count,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    provider: "ollama",
                    model: model_owned.clone(),
                }),
            })
        });
        Ok(Box::pin(chunks))
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
    #[allow(clippy::await_holding_lock)]
    async fn generate_omits_num_predict_when_max_tokens_is_zero() {
        // Regression test: the cross-backend `max_tokens == 0` sentinel must
        // OMIT `options.num_predict` from the wire body. Sending it as 0
        // makes Ollama interpret it literally as "produce 0 tokens" and
        // return an empty response — the silent-empty-response regression
        // that broke `mur learn extract --llm`, `mur out` workflow extraction
        // and the LLM-starters path for users with provider:ollama.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(
                        r#"{"response":"ok","done":true,"model":"qwen3:14b","prompt_eval_count":1,"eval_count":1}"#,
                    ),
            )
            .mount(&server)
            .await;
        let b = OllamaBackend::new(&server.uri(), Duration::from_secs(5));
        let req = ChatRequest {
            model: "qwen3:14b",
            user: "hi",
            system: None,
            max_tokens: 0, // sentinel — should send options.num_predict: None
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let _ = b.generate(req).await.unwrap();
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1, "expected exactly one POST /api/generate");
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        // GenerateRequest serializes options as a nested object (see
        // mur-core/src/conversations/ollama.rs::GenerateOptions).
        let options = body
            .get("options")
            .expect("body should carry an `options` object");
        assert!(
            options.get("num_predict").is_none(),
            "num_predict must be absent when max_tokens=0 — sending it as 0 makes Ollama return empty (regression introduced in P4). got options: {options:?}"
        );
    }

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
                assert!(
                    first.is_err(),
                    "stream should yield an Err for unreachable endpoint"
                );
            }
        }
    }
}
