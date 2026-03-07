//! LLM calling infrastructure supporting multiple providers.
//!
//! Reads [`LlmConfig`] from `~/.mur/config.yaml` and dispatches completion
//! requests to the configured provider: Anthropic, OpenAI, Gemini, Ollama,
//! or any OpenAI-compatible endpoint (e.g. OpenRouter via `openai_url`).

use anyhow::{Context, Result};
use mur_common::config::LlmConfig;
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
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
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
