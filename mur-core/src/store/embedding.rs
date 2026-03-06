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
    OpenAI { api_key: String },
}

impl EmbeddingConfig {
    /// Create from the global mur config.
    pub fn from_config(cfg: &mur_common::config::Config) -> Self {
        let provider = match cfg.embedding.provider.as_str() {
            "openai" | "gemini" | "anthropic" => {
                // Resolve API key from api_key_env or fall back to OPENAI_API_KEY
                let api_key = cfg
                    .embedding
                    .api_key_env
                    .as_deref()
                    .and_then(|env| std::env::var(env).ok())
                    .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
                EmbeddingProvider::OpenAI { api_key }
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
        EmbeddingProvider::OpenAI { api_key } => embed_openai(text, api_key, &config.model).await,
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

async fn embed_openai(text: &str, api_key: &str, model: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.openai.com/v1/embeddings")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: text.into(),
        })
        .send()
        .await
        .context("calling OpenAI embed API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI API error {}: {}", status, body);
    }

    let data: OpenAIEmbedResponse = resp.json().await.context("parsing OpenAI response")?;
    data.data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .context("no embedding returned")
}
