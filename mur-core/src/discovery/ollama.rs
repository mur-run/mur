//! `Discovery` impl for Ollama via REST API at `${endpoint}/api/{tags,show,embed}`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::{Backend, Discovery, DiscoveredModel, EmbeddingProbe, ModelKind};

#[derive(Debug, Clone)]
pub struct OllamaDiscovery {
    endpoint: String,
    client: reqwest::Client,
}

impl OllamaDiscovery {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(&self, suffix: &str) -> String {
        format!("{}{}", self.endpoint.trim_end_matches('/'), suffix)
    }
}

#[derive(Deserialize)]
struct TagsResp {
    models: Vec<TagsEntry>,
}

#[derive(Deserialize)]
struct TagsEntry {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    details: TagsDetails,
}

#[derive(Deserialize, Default)]
struct TagsDetails {
    #[serde(default)]
    family: Option<String>,
}

#[derive(Deserialize, Default)]
struct ShowResp {
    #[serde(default)]
    capabilities: Vec<String>,
}

/// Classify a model's kind from its `capabilities` array (authoritative) or,
/// when absent, from a family + name heuristic (fallback for older Ollama).
///
/// Heuristic families:
/// - `bert | nomic-bert | jina-bert` → Embedding
/// - `qwen3` with name containing "embedding" → Embedding (disambiguation)
/// - `qwen3 | llama | gemma` → Llm
/// - anything else → Unknown
fn classify(family: Option<&str>, name: &str, capabilities: &[String]) -> ModelKind {
    if capabilities.iter().any(|c| c == "embedding") {
        return ModelKind::Embedding;
    }
    if capabilities.iter().any(|c| c == "completion") {
        return ModelKind::Llm;
    }
    // Fallback heuristic when /api/show fails or returns no capabilities.
    match family {
        Some("bert" | "nomic-bert" | "jina-bert") => ModelKind::Embedding,
        Some("qwen3") if name.contains("embedding") => ModelKind::Embedding,
        Some("qwen3" | "llama" | "gemma") => ModelKind::Llm,
        _ => ModelKind::Unknown,
    }
}

#[async_trait]
impl Discovery for OllamaDiscovery {
    fn backend(&self) -> Backend {
        Backend::Ollama
    }

    async fn list_models(&self) -> Result<Vec<DiscoveredModel>> {
        let resp = self
            .client
            .get(self.url("/api/tags"))
            .send()
            .await
            .context("GET /api/tags")?;
        let tags: TagsResp = resp.json().await.context("parse /api/tags")?;

        let mut out = Vec::with_capacity(tags.models.len());
        for entry in tags.models {
            // /api/show may fail (500, network, etc.); fallback to heuristic.
            let caps = match self
                .client
                .post(self.url("/api/show"))
                .json(&serde_json::json!({ "name": entry.name }))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    r.json::<ShowResp>().await.unwrap_or_default().capabilities
                }
                _ => Vec::new(),
            };
            let kind = classify(entry.details.family.as_deref(), &entry.name, &caps);
            out.push(DiscoveredModel {
                id: entry.name,
                backend: Backend::Ollama,
                kind,
                dims: None,
                family: entry.details.family,
                size_bytes: entry.size,
                probed_at: None,
            });
        }
        Ok(out)
    }

    async fn probe_embedding(&self, model_id: &str) -> Result<EmbeddingProbe> {
        let started = Instant::now();
        let resp = self
            .client
            .post(self.url("/api/embed"))
            .json(&serde_json::json!({ "model": model_id, "input": "." }))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("POST /api/embed probe")?;

        if !resp.status().is_success() {
            anyhow::bail!("/api/embed returned {}", resp.status());
        }

        #[derive(Deserialize)]
        struct EmbedResp {
            embeddings: Vec<Vec<f32>>,
        }

        let er: EmbedResp = resp.json().await.context("parse /api/embed response")?;
        let dims = er.embeddings.first().map(Vec::len).unwrap_or(0);
        if dims == 0 {
            anyhow::bail!("/api/embed returned empty embeddings");
        }
        Ok(EmbeddingProbe {
            dims,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}
