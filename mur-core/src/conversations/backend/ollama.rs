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
        let chunks = inner_stream.map(|item| item.map(|delta| ChatChunk { delta, usage: None }));
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
