use crate::error::LlmError;

/// Trait for LLM providers (Anthropic, OpenAI, Ollama).
/// Shared between mur-core and mur-commander.
///
/// Edition 2024 supports async fn in traits natively.
pub trait LlmClient: Send + Sync {
    /// Text completion
    fn complete(&self, prompt: &str, system: Option<&str>) -> impl Future<Output = Result<String, LlmError>> + Send;

    /// Generate embedding vector
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, LlmError>> + Send;
}

use std::future::Future;
