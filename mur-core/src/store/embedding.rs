//! Embedding generation via Ollama or OpenAI API.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Embedding provider configuration.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub provider: EmbeddingProvider,
    pub model: String,
    #[allow(dead_code)] // Used by callers to pass dimensions to VectorStore
    pub dimensions: usize,
}

#[derive(Debug, Clone)]
pub enum EmbeddingProvider {
    Ollama { base_url: String },
    OpenAI { api_key: String, base_url: String },
}

impl EmbeddingConfig {
    /// Create from the global mur config.
    pub fn from_config(cfg: &mur_common::config::Config) -> Self {
        let provider = match cfg.embedding.provider.as_str() {
            "openai" | "gemini" | "anthropic" | "voyage" | "omlx" | "mlx" => {
                // Resolve API key from api_key_env or fall back to OPENAI_API_KEY
                let api_key = cfg
                    .embedding
                    .api_key_env
                    .as_deref()
                    .and_then(|env| std::env::var(env).ok())
                    .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
                let base_url = cfg
                    .embedding
                    .openai_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into());
                EmbeddingProvider::OpenAI { api_key, base_url }
            }
            _ => EmbeddingProvider::Ollama {
                base_url: cfg.embedding.ollama_endpoint.clone(),
            },
        };
        Self {
            provider,
            model: cfg.embedding.model.clone(),
            dimensions: cfg.embedding.dimensions,
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProvider::Ollama {
                base_url: "http://localhost:11434".into(),
            },
            model: "qwen3-embedding:0.6b".into(),
            dimensions: 1024,
        }
    }
}

/// Generate an embedding vector for the given text.
pub async fn embed(text: &str, config: &EmbeddingConfig) -> Result<Vec<f32>> {
    match &config.provider {
        EmbeddingProvider::Ollama { base_url } => embed_ollama(text, base_url, &config.model).await,
        EmbeddingProvider::OpenAI { api_key, base_url } => {
            embed_openai(text, base_url, api_key, &config.model).await
        }
    }
}

/// Batch embed multiple texts.
#[allow(dead_code)] // Public API for batch operations
pub async fn embed_batch(texts: &[String], config: &EmbeddingConfig) -> Result<Vec<Vec<f32>>> {
    // For now, sequential. Could parallelize later.
    let mut results = Vec::with_capacity(texts.len());
    for text in texts {
        results.push(embed(text, config).await?);
    }
    Ok(results)
}

// ─── Ollama ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: String,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

async fn embed_ollama(text: &str, base_url: &str, model: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/embed", base_url))
        .json(&OllamaEmbedRequest {
            model: model.into(),
            input: text.into(),
        })
        .send()
        .await
        .context("calling Ollama embed API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama API error {}: {}", status, body);
    }

    let data: OllamaEmbedResponse = resp.json().await.context("parsing Ollama response")?;
    data.embeddings
        .into_iter()
        .next()
        .context("no embedding returned")
}

// ─── OpenAI ─────────────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAIEmbedRequest {
    model: String,
    input: String,
}

#[derive(Deserialize)]
struct OpenAIEmbedResponse {
    data: Vec<OpenAIEmbedData>,
}

#[derive(Deserialize)]
struct OpenAIEmbedData {
    embedding: Vec<f32>,
}

async fn embed_openai(text: &str, base_url: &str, api_key: &str, model: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: text.into(),
        })
        .send()
        .await
        .with_context(|| format!("calling embed API at {}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Embed API error {} at {}: {}", status, url, body);
    }

    let data: OpenAIEmbedResponse = resp.json().await.context("parsing embed response")?;
    data.data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .context("no embedding returned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::config::Config;

    fn cfg_with(provider: &str, url: Option<&str>, env: Option<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.embedding.provider = provider.into();
        cfg.embedding.openai_url = url.map(str::to_string);
        cfg.embedding.api_key_env = env.map(str::to_string);
        cfg
    }

    #[test]
    fn omlx_provider_uses_custom_base_url() {
        let cfg = cfg_with("omlx", Some("http://localhost:8000/v1"), Some("OMLX_API_KEY"));
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "http://localhost:8000/v1");
            }
            _ => panic!("expected OpenAI variant for provider=omlx"),
        }
    }

    #[test]
    fn openai_provider_defaults_to_canonical_base_url() {
        let cfg = cfg_with("openai", None, Some("OPENAI_API_KEY"));
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "https://api.openai.com/v1");
            }
            _ => panic!("expected OpenAI variant"),
        }
    }

    #[test]
    fn ollama_provider_unchanged_by_openai_url() {
        let cfg = cfg_with("ollama", Some("http://example.com"), None);
        let ec = EmbeddingConfig::from_config(&cfg);
        matches!(ec.provider, EmbeddingProvider::Ollama { .. });
    }
}
