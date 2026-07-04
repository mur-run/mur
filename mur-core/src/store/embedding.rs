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
    /// Max texts per embedding API request (controls model-server RAM).
    pub batch_size: usize,
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
                // Resolve API key from api_key_ref, then api_key_env, then OPENAI_API_KEY
                let api_key = cfg
                    .embedding
                    .api_key_ref
                    .as_deref()
                    .and_then(|r| r.parse::<mur_common::secret::SecretRef>().ok())
                    .and_then(|s| s.resolve_to_string_blocking())
                    .or_else(|| {
                        cfg.embedding
                            .api_key_env
                            .as_deref()
                            .and_then(|env| std::env::var(env).ok())
                    })
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
            batch_size: cfg.sources_global.embedding_batch_size,
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
            batch_size: 32,
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

/// Batch embed multiple texts. Uses native batch API for Ollama, sequential for OpenAI.
pub async fn embed_batch(texts: &[String], config: &EmbeddingConfig) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    match &config.provider {
        EmbeddingProvider::Ollama { base_url } => {
            embed_ollama_batch(texts, base_url, &config.model).await
        }
        EmbeddingProvider::OpenAI { api_key, base_url } => {
            embed_openai_batch(texts, base_url, api_key, &config.model).await
        }
    }
}

// ─── Ollama ──────────────────────────────────────────────────────

/// How long Ollama should keep the embedding model resident after a request.
/// Without this, Ollama unloads the model after its default idle timeout, so
/// every `mur project search` after a pause pays a multi-second model-load cost.
/// Keeping it warm makes repeated searches feel interactive.
const EMBED_KEEP_ALIVE: &str = "15m";

#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: String,
    keep_alive: &'static str,
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
            keep_alive: EMBED_KEEP_ALIVE,
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

async fn embed_ollama_batch(
    texts: &[String],
    base_url: &str,
    model: &str,
) -> Result<Vec<Vec<f32>>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/embed", base_url))
        .json(&serde_json::json!({
            "model": model,
            "input": texts,
            "keep_alive": EMBED_KEEP_ALIVE,
        }))
        .send()
        .await
        .context("calling Ollama batch embed API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama API error {}: {}", status, body);
    }

    let data: OllamaEmbedResponse = resp.json().await.context("parsing Ollama batch response")?;
    Ok(data.embeddings)
}

// ─── OpenAI ─────────────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAIEmbedRequest {
    model: String,
    input: Vec<String>,
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
            input: vec![text.to_string()],
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

async fn embed_openai_batch(
    texts: &[String],
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::new();
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: texts.to_vec(),
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
    let embeddings: Vec<Vec<f32>> = data.data.into_iter().map(|d| d.embedding).collect();
    if embeddings.len() != texts.len() {
        anyhow::bail!(
            "Embed API returned {} embeddings but {} were requested",
            embeddings.len(),
            texts.len()
        );
    }
    Ok(embeddings)
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
        let cfg = cfg_with(
            "omlx",
            Some("http://localhost:8000/v1"),
            Some("OMLX_API_KEY"),
        );
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
        assert!(
            matches!(ec.provider, EmbeddingProvider::Ollama { .. }),
            "ollama provider must produce Ollama variant regardless of openai_url"
        );
    }

    #[test]
    fn from_config_prefers_api_key_ref() {
        unsafe { std::env::set_var("MUR_TEST_EMB_REF", "emb-key") };
        let mut cfg = mur_common::config::Config::default();
        cfg.embedding.provider = "openai".into();
        cfg.embedding.api_key_ref = Some("env:MUR_TEST_EMB_REF".into());
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { api_key, .. } => assert_eq!(api_key, "emb-key"),
            _ => panic!("expected OpenAI provider"),
        }
        unsafe { std::env::remove_var("MUR_TEST_EMB_REF") };
    }
}
