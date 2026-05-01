//! ChatBackend trait and supporting types. See spec
//! `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` §4.

#![allow(dead_code)] // wired progressively across P0 tasks.

use anyhow::Result;
use futures::stream::Stream;
use serde::Serialize;
use std::pin::Pin;

/// Per-call request to a chat-completion backend. Borrows where it can —
/// callers typically hold owned strings and pass &str.
#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub user: &'a str,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub stop: Vec<String>,
    /// Anthropic prompt-caching hint for the system prompt. Ignored by
    /// backends where `supports_caching()` is false.
    pub cache_system: bool,
    /// Anthropic prompt-caching hint: split `user` at this byte offset and
    /// place a cache_control breakpoint after the prefix. Ignored when
    /// `supports_caching()` is false. P0 stub only — wiring lands in P3.
    pub cache_user_prefix: Option<usize>,
}

/// Non-streaming response. `text` is the model output; `usage` reports
/// per-call token accounting (cache fields are 0 on non-caching backends).
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: String,
    pub usage: Usage,
}

/// Per-call token accounting. Both Anthropic-specific cache fields are
/// always present and default to 0 on non-Anthropic backends — keeps
/// downstream serialization shape uniform.
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub provider: &'static str,
    pub model: String,
}

/// Streaming chunk. `delta` is the incremental token payload (may be empty
/// on the final chunk). `usage` is `Some` ONLY on the final chunk.
#[derive(Debug, Clone)]
pub struct ChatChunk {
    pub delta: String,
    pub usage: Option<Usage>,
}

/// Type alias for the boxed stream of chunks returned by `generate_stream`.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_builds_with_required_fields() {
        let req = ChatRequest {
            model: "test-model",
            system: Some("you are a tester"),
            user: "hello",
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        assert_eq!(req.user, "hello");
        assert_eq!(req.model, "test-model");
    }

    #[test]
    fn usage_serializes_with_zero_cache_fields_on_non_anthropic() {
        let u = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            provider: "ollama",
            model: "qwen3:14b".into(),
        };
        let json = serde_json::to_string(&u).unwrap();
        assert!(json.contains("\"cache_read_input_tokens\":0"));
        assert!(json.contains("\"provider\":\"ollama\""));
    }
}
