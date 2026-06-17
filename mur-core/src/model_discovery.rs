//! Generic OpenAI-compatible `/v1/models` discovery.
//!
//! Parses model lists from OpenAI, Ollama, and bare array JSON shapes.
//! Provides best-effort HTTP discovery with timeout and optional API key support.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Response envelope shapes we support.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum Resp {
    /// OpenAI shape: `{data: [{id: "..."}]}`
    OpenAi { data: Vec<Entry> },
    /// Ollama shape: `{models: [{id: "..."}]}`
    Ollama { models: Vec<Entry> },
    /// Bare array: `[{id: "..."} | {name: "..."}]`
    Bare(Vec<Entry>),
}

/// Model entry with either `id` or `name` field.
#[derive(Debug, Deserialize, Serialize)]
struct Entry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl Entry {
    /// Extract the model identifier, preferring `id` over `name`.
    fn ident(self) -> Option<String> {
        self.id.or(self.name)
    }
}

/// Parse a JSON response from `/v1/models` endpoint.
///
/// Handles three shapes:
/// - OpenAI: `{data: [{id: "model-id"}]}`
/// - Ollama: `{models: [{id: "model-id"} | {name: "model-name"}]}`
/// - Bare array: `[{id: "..."} | {name: "..."}]`
pub fn parse_models_response(json: &str) -> Vec<String> {
    let resp: Resp = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let entries = match resp {
        Resp::OpenAi { data } => data,
        Resp::Ollama { models } => models,
        Resp::Bare(v) => v,
    };

    entries.into_iter().filter_map(Entry::ident).collect()
}

/// Construct the `/v1/models` URL, handling bases that may already end with `/v1`.
fn models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/models")
    } else {
        format!("{trimmed}/v1/models")
    }
}

/// Discover available models via HTTP GET to `{base}/v1/models`.
///
/// Best-effort with timeout. If API key is provided and non-empty, sends `Authorization: Bearer <key>`.
/// Returns model IDs extracted from the response envelope.
#[allow(dead_code)]
pub fn discover_models(
    base_url: &str,
    api_key: Option<&str>,
    timeout_secs: u64,
) -> Result<Vec<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;

    let mut rb = client.get(models_url(base_url));
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        rb = rb.header("Authorization", format!("Bearer {k}"));
    }

    let body = rb.send()?.text()?;
    Ok(parse_models_response(&body))
}

/// Generate a stable registry alias for a model: `{provider}_{modelslug}`,
/// lowercased, with non-alphanumerics collapsed to underscore.
///
/// Example: `("anthropic", "claude-opus-4.8")` → `"anthropic_claude_opus_4_8"`
///
/// Wired by S3 Task 2/3.
#[allow(dead_code)]
pub fn default_alias(provider: &str, model_id: &str) -> String {
    let mut s = String::with_capacity(provider.len() + 1 + model_id.len());
    s.push_str(&provider.to_ascii_lowercase());
    s.push('_');

    for c in model_id.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else {
            s.push('_');
        }
    }

    // Collapse consecutive underscores.
    let mut result = String::new();
    let mut last_was_underscore = false;
    for c in s.chars() {
        if c == '_' {
            if !last_was_underscore {
                result.push(c);
            }
            last_was_underscore = true;
        } else {
            result.push(c);
            last_was_underscore = false;
        }
    }

    // Trim leading/trailing underscores.
    result.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_envelope() {
        let json = r#"{"data":[{"id":"gpt-4","object":"model"},{"id":"gpt-3.5-turbo"}]}"#;
        let models = parse_models_response(json);
        assert_eq!(models, vec!["gpt-4", "gpt-3.5-turbo"]);
    }

    #[test]
    fn parses_ollama_and_bare_shapes() {
        // Ollama shape
        let ollama_json = r#"{"models":[{"name":"llama2"},{"id":"mistral"}]}"#;
        let ollama_models = parse_models_response(ollama_json);
        assert_eq!(ollama_models, vec!["llama2", "mistral"]);

        // Bare array shape
        let bare_json = r#"[{"id":"model-a"},{"name":"model-b"}]"#;
        let bare_models = parse_models_response(bare_json);
        assert_eq!(bare_models, vec!["model-a", "model-b"]);
    }

    #[test]
    fn alias_slugs_punctuation() {
        assert_eq!(default_alias("anthropic", "claude-opus-4.8"), "anthropic_claude_opus_4_8");
        assert_eq!(default_alias("openai", "gpt-4-turbo"), "openai_gpt_4_turbo");
        assert_eq!(default_alias("meta", "llama-2-70b-chat"), "meta_llama_2_70b_chat");
        assert_eq!(default_alias("provider", "model@latest#v1"), "provider_model_latest_v1");
        // S3 Task 1 mandated test vectors
        assert_eq!(default_alias("anthropic", "claude-opus-4-8"), "anthropic_claude_opus_4_8");
        assert_eq!(default_alias("openrouter", "meta-llama/llama-4"), "openrouter_meta_llama_llama_4");
        assert_eq!(default_alias("ollama", "qwen3:8b"), "ollama_qwen3_8b");
        // Lowercasing fix verification
        assert_eq!(default_alias("Anthropic", "Claude-Opus-4-8"), "anthropic_claude_opus_4_8");
    }
}
