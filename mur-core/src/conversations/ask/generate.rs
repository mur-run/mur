//! Streaming answer generation for ask. Thin adapter over the ChatBackend
//! trait — backend selection (Ollama vs Anthropic vs Mock) happens at the
//! call site via `factory::build`.

use anyhow::Result;
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;

use crate::conversations::backend::{ChatBackend, ChatRequest};

/// Stream the answer through `backend`. Yields raw token deltas as `String`s
/// to keep the downstream grounding/citation filter unchanged. The trailing
/// usage chunk (if any) is consumed and discarded — token accounting is
/// already estimated downstream from the forwarded text.
pub async fn stream_answer(
    backend: &dyn ChatBackend,
    model: &str,
    system: &str,
    user: &str,
    response_tokens: u32,
) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
    let req = ChatRequest {
        model,
        user,
        system: Some(system),
        max_tokens: response_tokens,
        temperature: Some(0.1),
        stop: vec!["\n\nQ:".into(), "\n\nQuestion:".into()],
        cache_system: false,
        cache_user_prefix: None,
    };
    let chunk_stream = backend.generate_stream(req).await?;
    // Adapt ChatChunk → String. Drop empty deltas (final usage chunk is
    // typically delta="" + usage=Some(...)) so downstream sees the same
    // token-only stream the OllamaClient path used to emit.
    let token_stream = chunk_stream.filter_map(|chunk| async move {
        match chunk {
            Ok(c) if c.delta.is_empty() => None,
            Ok(c) => Some(Ok(c.delta)),
            Err(e) => Some(Err(e)),
        }
    });
    Ok(Box::pin(token_stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::backend::mock::MockBackend;

    #[tokio::test]
    async fn mock_backend_stream_yields_tokens() {
        // MockBackend reuses ollama::mock_generate's pattern dispatcher;
        // a prompt containing "[cit:" trips the canned citation reply.
        let backend = MockBackend::new();
        let mut s = stream_answer(&backend, "qwen3:14b", "system", "ask about [cit:", 256)
            .await
            .unwrap();
        let mut combined = String::new();
        while let Some(chunk) = s.next().await {
            combined.push_str(&chunk.unwrap());
        }
        assert!(combined.contains("[cit: 2026-04-19 claude-code/mock:L1]"));
    }
}
