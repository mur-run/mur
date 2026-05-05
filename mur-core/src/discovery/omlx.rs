//! `Discovery` impl for oMLX via OpenAI-compatible REST API at
//! `${base_url}/v1/{models,embeddings}`. oMLX serves on `localhost:8000`
//! by default.
//!
//! See `docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md` § 2.3.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::{Backend, Discovery, DiscoveredModel, EmbeddingProbe, ModelKind};

/// oMLX issue #266: graph recompiles on first call after >3s idle. Budget
/// 10s for the probe; subsequent probes settle to <500ms.
pub(crate) const OMLX_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct OMlxDiscovery {
    base_url: String,
    client: reqwest::Client,
}

impl OMlxDiscovery {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Build a URL from a `/v1/...` suffix.
    ///
    /// Handles the case where `base_url` already ends in `/v1` (e.g.
    /// `http://localhost:8000/v1`) to avoid producing a doubled `/v1/v1/…`
    /// path. Trailing slashes on `base_url` are stripped first.
    fn url(&self, suffix: &str) -> String {
        let trimmed = self.base_url.trim_end_matches('/');
        // If base_url already ends in "/v1", strip the leading "/v1" from
        // suffix so we don't produce a doubled "/v1/v1/...".
        let suffix = if trimmed.ends_with("/v1") {
            suffix.strip_prefix("/v1").unwrap_or(suffix)
        } else {
            suffix
        };
        format!("{}{}", trimmed, suffix)
    }
}

/// Infer a family string from an oMLX model id.
///
/// oMLX /v1/models returns flat HF-style ids (e.g.
/// `mlx-community/Qwen3-Embedding-0.6B-8bit`). This heuristic covers the
/// models in the mlx-embeddings registry as of May 2026.
///
/// Private — callers observe the result through `DiscoveredModel.family`.
fn family_from_id(id: &str) -> Option<String> {
    let lower = id.to_ascii_lowercase();
    if lower.contains("qwen3") {
        Some("qwen3".into())
    } else if lower.contains("bge-") || lower.contains("bge_") {
        Some("bge".into())
    } else if lower.contains("modernbert") {
        Some("modernbert".into())
    } else if lower.contains("nomic") {
        Some("nomic-bert".into())
    } else if lower.contains("jina") {
        Some("jina-bert".into())
    } else {
        None
    }
}

// ── Wire types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ModelsResp {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Deserialize)]
struct EmbedResp {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

// ── Discovery impl ───────────────────────────────────────────────────────────

#[async_trait]
impl Discovery for OMlxDiscovery {
    fn backend(&self) -> Backend {
        Backend::OMlx
    }

    /// List all models loaded in oMLX.
    ///
    /// oMLX's `/v1/models` has no `type` field — every entry comes back with
    /// `kind = Unknown`. Call `probe_embedding` to discriminate after the
    /// user selects a model (or to populate the discovery menu).
    async fn list_models(&self) -> Result<Vec<DiscoveredModel>> {
        let resp = self
            .client
            .get(self.url("/v1/models"))
            .send()
            .await
            .context("GET /v1/models")?;
        let mr: ModelsResp = resp.json().await.context("parse /v1/models")?;

        Ok(mr
            .data
            .into_iter()
            .map(|e| DiscoveredModel {
                family: family_from_id(&e.id),
                id: e.id,
                backend: Backend::OMlx,
                // /v1/models carries no type discriminator; probing resolves this.
                kind: ModelKind::Unknown,
                dims: None,
                size_bytes: None,
                probed_at: None,
            })
            .collect())
    }

    /// Issue a 1-token `POST /v1/embeddings` to confirm the model supports
    /// embeddings and learn the output dimension.
    ///
    /// Uses `OMLX_PROBE_TIMEOUT` (10s) to accommodate oMLX issue #266's
    /// first-call graph-recompile latency spike.
    async fn probe_embedding(&self, model_id: &str) -> Result<EmbeddingProbe> {
        let started = Instant::now();
        let resp = self
            .client
            .post(self.url("/v1/embeddings"))
            .json(&serde_json::json!({ "model": model_id, "input": "." }))
            .timeout(OMLX_PROBE_TIMEOUT)
            .send()
            .await
            .with_context(|| format!("POST /v1/embeddings probe for model {:?}", model_id))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!(
                model_id = %model_id,
                %status,
                body = %body,
                "oMLX probe_embedding failed"
            );
            anyhow::bail!(
                "/v1/embeddings returned {} for model {:?}: {}",
                status,
                model_id,
                body
            );
        }

        let er: EmbedResp = resp
            .json()
            .await
            .with_context(|| format!("parse /v1/embeddings response for model {:?}", model_id))?;

        let dims = er.data.first().map(|d| d.embedding.len()).unwrap_or(0);
        if dims == 0 {
            tracing::debug!(
                model_id = %model_id,
                "/v1/embeddings returned empty data array for model"
            );
            anyhow::bail!(
                "/v1/embeddings returned empty data array for model {:?}",
                model_id
            );
        }

        Ok(EmbeddingProbe {
            dims,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod family_tests {
    use super::family_from_id;

    #[test]
    fn qwen3_variants() {
        assert_eq!(
            family_from_id("mlx-community/Qwen3-Embedding-0.6B-8bit"),
            Some("qwen3".into())
        );
        assert_eq!(
            family_from_id("Qwen3-Embedding-8B-4bit-DWQ"),
            Some("qwen3".into())
        );
    }

    #[test]
    fn bge_dash_or_underscore() {
        assert_eq!(family_from_id("mlx-community/bge-m3"), Some("bge".into()));
        assert_eq!(family_from_id("BAAI/bge_large"), Some("bge".into()));
    }

    #[test]
    fn modernbert_family() {
        assert_eq!(
            family_from_id("lightonai/modernbert-embed-large"),
            Some("modernbert".into())
        );
    }

    #[test]
    fn nomic_family() {
        assert_eq!(
            family_from_id("nomic-ai/nomic-embed-text-v1.5"),
            Some("nomic-bert".into())
        );
    }

    #[test]
    fn jina_family() {
        assert_eq!(
            family_from_id("jinaai/jina-embeddings-v3"),
            Some("jina-bert".into())
        );
    }

    #[test]
    fn unknown_id_returns_none() {
        assert_eq!(family_from_id("unknown/foo"), None);
        assert_eq!(family_from_id(""), None);
    }

    #[test]
    fn case_insensitive_via_lowercase() {
        // family_from_id calls to_ascii_lowercase first, so capitalized
        // tags like "Qwen3" still match.
        assert_eq!(family_from_id("Some/QWEN3-Whatever"), Some("qwen3".into()));
    }
}
