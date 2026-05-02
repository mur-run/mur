//! LLM calling infrastructure supporting multiple providers.
//!
//! Reads [`LlmConfig`] from `~/.mur/config.yaml` and dispatches completion
//! requests to the configured provider: Anthropic, OpenAI, Gemini, Ollama,
//! or any OpenAI-compatible endpoint (e.g. OpenRouter via `openai_url`).
//!
//! NOTE (P4 Task 8 → Task 9): all callers have migrated to the
//! `conversations::backend::ChatBackend` trait. This entire module is
//! orphaned and is deleted in Task 9. The `dead_code` allow below silences
//! the transitional warning between commits.
#![allow(dead_code)]

use anyhow::{Context, Result};
use mur_common::config::LlmConfig;
use mur_common::llm::anthropic_base_url;
use serde::{Deserialize, Serialize};

// ─── Public API ─────────────────────────────────────────────────────

/// Send a completion request to the configured LLM provider.
pub async fn llm_complete(config: &LlmConfig, system: &str, prompt: &str) -> Result<String> {
    let api_key = resolve_api_key(config)?;

    match config.provider.as_str() {
        "anthropic" => anthropic_complete(config, &api_key, system, prompt).await,
        "openai" => openai_complete(config, None, &api_key, system, prompt).await,
        "gemini" => gemini_complete(config, &api_key, system, prompt).await,
        "ollama" => ollama_complete(config, system, prompt).await,
        "openrouter" => {
            let base_url = config
                .openai_url
                .as_deref()
                .unwrap_or("https://openrouter.ai/api/v1");
            openai_complete(config, Some(base_url), &api_key, system, prompt).await
        }
        other => {
            // If openai_url is set, treat as OpenAI-compatible
            if let Some(url) = &config.openai_url {
                openai_complete(config, Some(url), &api_key, system, prompt).await
            } else {
                anyhow::bail!("Unsupported LLM provider: {other}")
            }
        }
    }
}

// ─── Model Quality Check ────────────────────────────────────────────

/// Check if a model name matches recommended reasoning models for session analysis.
///
/// Recommended: Anthropic Opus, OpenAI GPT-5/O3/O4, Gemini Pro 3+,
/// or any model with "reasoning" or "think" in the name.
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_lowercase();

    // Anthropic: any model containing "opus"
    if m.contains("opus") {
        return true;
    }

    // OpenAI: gpt-5, o3, o4
    if m.contains("gpt-5") || m.contains("o3") || m.contains("o4") {
        return true;
    }

    // Gemini: "pro" with version >= 3
    if m.contains("gemini") && m.contains("pro") {
        // Look for a version number after "pro"
        if let Some(pos) = m.find("pro") {
            let after = &m[pos + 3..];
            // Extract first digit sequence
            let version_str: String = after
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(v) = version_str.parse::<u32>()
                && v >= 3
            {
                return true;
            }
        }
    }

    // Generic: "reasoning" or "think"
    if m.contains("reasoning") || m.contains("think") {
        return true;
    }

    false
}

// ─── Key Resolution ─────────────────────────────────────────────────

fn resolve_api_key(config: &LlmConfig) -> Result<String> {
    // Ollama doesn't need an API key
    if config.provider == "ollama" {
        return Ok(String::new());
    }

    let env_var = config
        .api_key_env
        .as_deref()
        .unwrap_or(default_key_env(&config.provider));

    std::env::var(env_var).with_context(|| {
        format!(
            "Missing API key: set ${env_var} for provider '{}'",
            config.provider
        )
    })
}

fn default_key_env(provider: &str) -> &str {
    match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "LLM_API_KEY",
    }
}

// ─── Anthropic ──────────────────────────────────────────────────────

/// Warn once per process if the configured key looks like a Claude
/// subscription OAuth token (`sk-ant-oat*`) but `ANTHROPIC_BASE_URL`
/// still points at api.anthropic.com directly. Such requests will 401 —
/// the user almost certainly meant to route through a local OAuth
/// bridge (e.g. cc-proxy listening on 127.0.0.1:8088).
fn warn_if_oauth_key_misconfigured(api_key: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    if !api_key.contains("sk-ant-oat") {
        return;
    }
    let base = anthropic_base_url();
    if base.starts_with("https://api.anthropic.com") {
        tracing::warn!(
            base_url = %base,
            "ANTHROPIC_API_KEY looks like an OAuth subscription token (sk-ant-oat*), \
             but ANTHROPIC_BASE_URL points to api.anthropic.com — Anthropic will reject \
             the request. Set ANTHROPIC_BASE_URL to a local OAuth bridge."
        );
    }
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<AnthropicMessage<'a>>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    text: String,
}

async fn anthropic_complete(
    config: &LlmConfig,
    api_key: &str,
    system: &str,
    prompt: &str,
) -> Result<String> {
    warn_if_oauth_key_misconfigured(api_key);

    let client = reqwest::Client::new();
    let body = AnthropicRequest {
        model: &config.model,
        max_tokens: 4096,
        system,
        messages: vec![AnthropicMessage {
            role: "user",
            content: prompt,
        }],
    };

    let resp = client
        .post(format!("{}/v1/messages", anthropic_base_url()))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .header("x-api-key", api_key)
        .json(&body)
        .send()
        .await
        .context("Failed to connect to Anthropic API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Anthropic API error ({status}): {body}");
    }

    let data: AnthropicResponse = resp.json().await.context("Invalid Anthropic response")?;
    data.content
        .first()
        .map(|c| c.text.clone())
        .ok_or_else(|| anyhow::anyhow!("Empty Anthropic response"))
}

// ─── OpenAI (and compatible: OpenRouter, etc.) ──────────────────────

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    max_tokens: u32,
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
}

#[derive(Deserialize)]
struct OpenAiChoiceMessage {
    content: Option<String>,
}

async fn openai_complete(
    config: &LlmConfig,
    base_url: Option<&str>,
    api_key: &str,
    system: &str,
    prompt: &str,
) -> Result<String> {
    let url = format!(
        "{}/chat/completions",
        base_url.unwrap_or("https://api.openai.com/v1")
    );
    let client = reqwest::Client::new();

    let body = OpenAiRequest {
        model: &config.model,
        messages: vec![
            OpenAiMessage {
                role: "system",
                content: system,
            },
            OpenAiMessage {
                role: "user",
                content: prompt,
            },
        ],
        max_tokens: 4096,
    };

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to connect to OpenAI-compatible API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI API error ({status}): {body}");
    }

    let data: OpenAiResponse = resp.json().await.context("Invalid OpenAI response")?;
    data.choices
        .first()
        .and_then(|c| c.message.content.clone())
        .ok_or_else(|| anyhow::anyhow!("Empty OpenAI response"))
}

// ─── Gemini ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GeminiRequest<'a> {
    system_instruction: GeminiContent<'a>,
    contents: Vec<GeminiContent<'a>>,
}

#[derive(Serialize)]
struct GeminiContent<'a> {
    parts: Vec<GeminiPart<'a>>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiCandidateContent,
}

#[derive(Deserialize)]
struct GeminiCandidateContent {
    parts: Vec<GeminiResponsePart>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    text: String,
}

#[derive(Serialize)]
struct GeminiPart<'a> {
    text: &'a str,
}

async fn gemini_complete(
    config: &LlmConfig,
    api_key: &str,
    system: &str,
    prompt: &str,
) -> Result<String> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        config.model, api_key
    );
    let client = reqwest::Client::new();

    let body = GeminiRequest {
        system_instruction: GeminiContent {
            parts: vec![GeminiPart { text: system }],
        },
        contents: vec![GeminiContent {
            parts: vec![GeminiPart { text: prompt }],
        }],
    };

    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to connect to Gemini API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Gemini API error ({status}): {body}");
    }

    let data: GeminiResponse = resp.json().await.context("Invalid Gemini response")?;
    data.candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .map(|p| p.text.clone())
        .ok_or_else(|| anyhow::anyhow!("Empty Gemini response"))
}

// ─── Ollama ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OllamaRequest<'a> {
    model: &'a str,
    system: &'a str,
    prompt: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaResponse {
    response: String,
}

async fn ollama_complete(config: &LlmConfig, system: &str, prompt: &str) -> Result<String> {
    let endpoint = config
        .openai_url
        .as_deref()
        .unwrap_or("http://localhost:11434");
    let url = format!("{endpoint}/api/generate");
    let client = reqwest::Client::new();

    let body = OllamaRequest {
        model: &config.model,
        system,
        prompt,
        stream: false,
    };

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("Failed to connect to Ollama")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama API error ({status}): {body}");
    }

    let data: OllamaResponse = resp.json().await.context("Invalid Ollama response")?;
    Ok(data.response)
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_api_key_ollama_no_key_needed() {
        let config = LlmConfig {
            provider: "ollama".to_string(),
            model: "llama3".to_string(),
            api_key_env: None,
            openai_url: None,
        };
        assert!(resolve_api_key(&config).is_ok());
        assert_eq!(resolve_api_key(&config).unwrap(), "");
    }

    #[test]
    fn test_resolve_api_key_missing() {
        let config = LlmConfig {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            api_key_env: Some("MUR_TEST_NONEXISTENT_KEY_12345".to_string()),
            openai_url: None,
        };
        assert!(resolve_api_key(&config).is_err());
    }

    #[test]
    fn test_resolve_api_key_from_env() {
        let config = LlmConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key_env: Some("MUR_TEST_API_KEY".to_string()),
            openai_url: None,
        };
        // Temporarily set
        unsafe {
            std::env::set_var("MUR_TEST_API_KEY", "sk-test-123");
        }
        let result = resolve_api_key(&config);
        unsafe {
            std::env::remove_var("MUR_TEST_API_KEY");
        }
        assert_eq!(result.unwrap(), "sk-test-123");
    }

    #[test]
    fn test_default_key_env() {
        assert_eq!(default_key_env("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(default_key_env("openai"), "OPENAI_API_KEY");
        assert_eq!(default_key_env("gemini"), "GEMINI_API_KEY");
        assert_eq!(default_key_env("openrouter"), "OPENROUTER_API_KEY");
        assert_eq!(default_key_env("custom"), "LLM_API_KEY");
    }

    #[test]
    fn test_is_reasoning_model() {
        // Anthropic opus models
        assert!(is_reasoning_model("claude-opus-4-6"));
        assert!(is_reasoning_model("claude-opus-4-20250514"));

        // OpenAI reasoning models
        assert!(is_reasoning_model("gpt-5"));
        assert!(is_reasoning_model("chatgpt-5.4"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(is_reasoning_model("o4-preview"));

        // Gemini pro >= 3
        assert!(is_reasoning_model("gemini-pro-3.5"));
        assert!(is_reasoning_model("gemini-pro-3"));
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

    #[test]
    fn test_unsupported_provider_without_url() {
        let config = LlmConfig {
            provider: "unknown".to_string(),
            model: "model".to_string(),
            api_key_env: Some("MUR_TEST_API_KEY".to_string()),
            openai_url: None,
        };
        // llm_complete would fail because no openai_url is set and provider is unknown
        // We can't test the async function directly here, but we verify key resolution works
        unsafe {
            std::env::set_var("MUR_TEST_API_KEY", "test");
        }
        let key = resolve_api_key(&config);
        unsafe {
            std::env::remove_var("MUR_TEST_API_KEY");
        }
        assert!(key.is_ok());
    }
}
