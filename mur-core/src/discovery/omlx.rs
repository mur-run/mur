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
    /// Bearer token for the `Authorization` header. Empty string means no
    /// header is sent. oMLX in recent versions enforces auth even on
    /// localhost (returns 401 with `{"error":{"message":"API key required",...}}`),
    /// so this is required in practice — the env-var resolution lives in
    /// `discovery::run_all` and `init.rs::select_local_embedding`.
    api_key: String,
    client: reqwest::Client,
}

impl OMlxDiscovery {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_api_key(base_url, String::new())
    }

    pub fn with_api_key(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Apply the `Authorization: Bearer <key>` header to a request builder
    /// when the api_key is non-empty. Helper to keep auth wiring DRY across
    /// `list_models` and `probe_embedding`.
    fn with_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            rb
        } else {
            rb.header("Authorization", format!("Bearer {}", self.api_key))
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
struct ModelEntry {
    id: String,
}

/// `/v1/models` response shape. oMLX has shipped multiple shapes across
/// versions, so we accept several and pick the first that parses.
#[derive(Deserialize)]
#[serde(untagged)]
enum ModelsResp {
    /// Strict OpenAI envelope: `{ "object": "list", "data": [{"id": ...}] }`.
    OpenAi { data: Vec<ModelEntry> },
    /// Ollama-style: `{ "models": [{"id": ...}] }`.
    Ollama { models: Vec<ModelEntry> },
    /// Bare array: `[{"id": ...}, ...]`.
    Bare(Vec<ModelEntry>),
}

impl ModelsResp {
    fn into_entries(self) -> Vec<ModelEntry> {
        match self {
            ModelsResp::OpenAi { data } => data,
            ModelsResp::Ollama { models } => models,
            ModelsResp::Bare(v) => v,
        }
    }
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
            .with_auth(self.client.get(self.url("/v1/models")))
            .send()
            .await
            .context("GET /v1/models")?;
        let status = resp.status();
        // Read the body once into a String so the multi-shape parse below
        // can give a useful error message that includes the actual payload.
        let body = resp.text().await.context("read /v1/models body")?;
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                anyhow::bail!(
                    "GET /v1/models returned 401 — set OMLX_API_KEY to your oMLX API key (export OMLX_API_KEY=<key>). Body: {}",
                    body.chars().take(200).collect::<String>()
                );
            }
            anyhow::bail!(
                "GET /v1/models returned {}: {}",
                status,
                body.chars().take(300).collect::<String>()
            );
        }
        let mr: ModelsResp = serde_json::from_str(&body).with_context(|| {
            format!(
                "parse /v1/models — none of {{data:[]}} / {{models:[]}} / [{{}}] matched. Body: {}",
                body.chars().take(300).collect::<String>()
            )
        })?;

        Ok(mr
            .into_entries()
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
            .with_auth(self.client.post(self.url("/v1/embeddings")))
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
