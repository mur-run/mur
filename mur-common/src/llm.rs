use crate::error::LlmError;

/// Trait for LLM providers (Anthropic, OpenAI, Ollama).
/// Shared between mur-core and mur-commander.
///
/// Edition 2024 supports async fn in traits natively.
pub trait LlmClient: Send + Sync {
    /// Text completion
    fn complete(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> impl Future<Output = Result<String, LlmError>> + Send;

    /// Generate embedding vector
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, LlmError>> + Send;
}

use std::future::Future;

/// Default Anthropic API base URL.
pub const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Resolve the Anthropic API base URL from `ANTHROPIC_BASE_URL` env, with a
/// trailing slash stripped. Falls back to `ANTHROPIC_DEFAULT_BASE_URL`.
///
/// Honored at every upstream call site so that users can route Anthropic
/// traffic through Bedrock, Vertex, a corporate egress proxy, an external
/// auth bridge, or test fixtures without touching code.
pub fn anthropic_base_url() -> String {
    let raw = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| ANTHROPIC_DEFAULT_BASE_URL.to_string());
    raw.trim_end_matches('/').to_string()
}
