use anyhow::Result;

/// Trait for LLM providers (Anthropic, OpenAI, Ollama).
/// Shared between mur-core and mur-commander.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Text completion
    async fn complete(&self, prompt: &str, system: Option<&str>) -> Result<String>;

    /// Generate embedding vector
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
