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

/// Check if a model name matches recommended reasoning models for session analysis.
///
/// Recommended: Anthropic Opus, OpenAI GPT-5/O3/O4, Gemini Pro 3+,
/// or any model with "reasoning" or "think" in the name.
#[allow(clippy::collapsible_if)]
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_lowercase();

    if m.contains("opus") {
        return true;
    }
    if m.contains("gpt-5") || m.contains("o3") || m.contains("o4") {
        return true;
    }
    if m.contains("gemini") && m.contains("pro") {
        // The version may follow ("gemini-pro-3.5") or precede ("gemini-3.5-pro")
        // the tier, so take the major version from the first number in the name.
        if let Some(start) = m.find(|c: char| c.is_ascii_digit()) {
            let tail = &m[start..];
            let end = tail
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(tail.len());
            if let Ok(v) = tail[..end].parse::<u32>()
                && v >= 3
            {
                return true;
            }
        }
    }
    if m.contains("reasoning") || m.contains("think") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_reasoning_model() {
        // Anthropic opus models
        assert!(is_reasoning_model("claude-opus-5"));
        assert!(is_reasoning_model("claude-opus-4-20250514"));

        // OpenAI reasoning models
        assert!(is_reasoning_model("gpt-5"));
        assert!(is_reasoning_model("chatgpt-5.4"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(is_reasoning_model("o4-preview"));

        // Gemini pro >= 3 (version before or after the tier)
        assert!(is_reasoning_model("gemini-pro-3.5"));
        assert!(is_reasoning_model("gemini-pro-3"));
        assert!(is_reasoning_model("gemini-3.5-pro"));
        assert!(!is_reasoning_model("gemini-2.5-pro"));
        assert!(!is_reasoning_model("gemini-pro-2"));
        assert!(!is_reasoning_model("gemini-pro-1.5"));

        // Generic reasoning/thinking
        assert!(is_reasoning_model("deepseek-reasoning-v2"));
        assert!(is_reasoning_model("qwen-thinking-32b"));

        // Non-recommended
        assert!(!is_reasoning_model("claude-sonnet-4-20250514"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("gemini-flash-2"));
        assert!(!is_reasoning_model("llama3"));
    }
}
