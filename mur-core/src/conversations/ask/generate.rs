//! Streaming Ollama generation for ask. Thin adapter over conversations::ollama.

use anyhow::Result;
use futures::stream::Stream;
use std::pin::Pin;
use std::time::Duration;

use super::super::ollama::{GenerateOptions, GenerateRequest, OllamaClient};

pub async fn stream_answer(
    endpoint: &str,
    model: &str,
    system: &str,
    user: &str,
    response_tokens: u32,
    timeout: Duration,
) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
    let client = OllamaClient::new(endpoint, timeout);
    client
        .generate_stream(GenerateRequest {
            model,
            prompt: user,
            system: Some(system),
            stream: true,
            options: GenerateOptions {
                temperature: Some(0.1),
                top_p: Some(0.9),
                num_predict: Some(response_tokens),
                stop: vec!["\n\nQ:".into(), "\n\nQuestion:".into()],
            },
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_stream_yields_tokens() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let mut s = stream_answer(
            "http://unused",
            "qwen3:14b",
            "system",
            "ask about [cit:",
            256,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let mut combined = String::new();
        while let Some(chunk) = s.next().await {
            combined.push_str(&chunk.unwrap());
        }
        assert!(combined.contains("[cit: 2026-04-19 claude-code/mock:L1]"));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
