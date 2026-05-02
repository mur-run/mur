//! Test-only ChatBackend that returns pattern-matched canned responses.
//! Reuses the prompt-dispatch logic from `ollama.rs::mock_generate` —
//! activated by `MUR_LLM_MOCK=1` (preferred) or `MUR_OLLAMA_MOCK=1` (legacy).
//!
//! See spec §5.3.

use anyhow::Result;
use async_trait::async_trait;
use futures::stream;

use super::{ChatBackend, ChatChunk, ChatRequest, ChatResponse, ChatStream, Usage};

pub struct MockBackend;

impl MockBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChatBackend for MockBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        // Reuse the existing pattern dispatcher. ollama::mock_generate takes
        // a GenerateRequest, so build one from our ChatRequest.
        use crate::conversations::ollama::{GenerateOptions, GenerateRequest};
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
        // Mirror the legacy OllamaClient::generate fail-injection: when
        // MUR_ABSTRACTIVE_MOCK_FAIL=timeout AND the request is the Stage 1b
        // abstractive prompt, sleep long enough that the caller's
        // tokio::time::timeout fires. Keeps the
        // `mur_ask_stage_1b_soft_fails_gracefully` end-to-end test working
        // after Stage 1b moved off OllamaClient onto ChatBackend.
        let is_abstractive = req
            .system
            .map(|s| s.contains("You compress text for retrieval context"))
            .unwrap_or(false);
        if is_abstractive && std::env::var("MUR_ABSTRACTIVE_MOCK_FAIL").as_deref() == Ok("timeout")
        {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
        let g_resp = crate::conversations::ollama::mock_generate(&g_req);
        Ok(ChatResponse {
            text: g_resp.response,
            usage: Usage {
                input_tokens: g_resp.prompt_eval_count,
                output_tokens: g_resp.eval_count,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                provider: "mock",
                model: req.model.to_string(),
            },
        })
    }

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        // Mock streaming = single-chunk stream containing the full mock response.
        let resp = self.generate(req).await?;
        let final_chunk = ChatChunk {
            delta: resp.text.clone(),
            usage: Some(resp.usage),
        };
        Ok(Box::pin(stream::iter(vec![Ok(final_chunk)])))
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn req<'a>(prompt: &'a str) -> ChatRequest<'a> {
        ChatRequest {
            model: "mock-model",
            system: None,
            user: prompt,
            max_tokens: 100,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[tokio::test]
    async fn generate_returns_text_and_usage() {
        let b = MockBackend::new();
        let r = b.generate(req("hello")).await.unwrap();
        // Mock returns *some* text — exact content depends on prompt patterns
        // in ollama::mock_generate. The contract here is just non-empty.
        assert!(!r.text.is_empty());
        assert_eq!(r.usage.provider, "mock");
        assert_eq!(r.usage.model, "mock-model");
    }

    #[tokio::test]
    async fn generate_stream_emits_single_chunk_with_usage() {
        let b = MockBackend::new();
        let mut stream = b.generate_stream(req("hello")).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert!(!first.delta.is_empty());
        assert!(first.usage.is_some());
        assert!(
            stream.next().await.is_none(),
            "should be a single-chunk stream"
        );
    }

    #[test]
    fn provider_name_is_mock() {
        assert_eq!(MockBackend::new().provider_name(), "mock");
    }
}
