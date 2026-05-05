//! Runtime discovery: query Ollama / oMLX for what models are actually
//! pulled, what their kind (LLM vs embedding) is, and what dims they have.
//!
//! See `docs/superpowers/specs/2026-05-05-mur-embedding-omlx-dynamic-design.md`.

pub mod aggregate;
pub mod cache;
pub mod ollama;
pub mod omlx;
pub mod preference;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Ollama,
    OMlx,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Backend::Ollama => f.write_str("Ollama"),
            Backend::OMlx => f.write_str("oMLX"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelKind {
    Llm,
    Embedding,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub backend: Backend,
    pub kind: ModelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dims: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingProbe {
    pub dims: usize,
    pub latency_ms: u64,
}

#[async_trait]
pub trait Discovery: Send + Sync {
    fn backend(&self) -> Backend;
    /// Enumerate all loaded / pulled models on this runtime.
    async fn list_models(&self) -> Result<Vec<DiscoveredModel>>;
    /// 1-token probe to determine kind + dim. Used after `list_models` when
    /// `kind == Unknown`, or when the user picks a model whose dims we
    /// haven't recorded yet.
    async fn probe_embedding(&self, model_id: &str) -> Result<EmbeddingProbe>;
}

use crate::discovery::ollama::OllamaDiscovery;
use crate::discovery::omlx::OMlxDiscovery;

/// Resolve the oMLX API key from the environment. Recent oMLX versions
/// require auth even on localhost (returns 401 with `{error: "API key
/// required"}`). The convention is `OMLX_API_KEY` — anything from a real
/// secret to "local" works depending on user's oMLX config.
pub fn resolve_omlx_api_key() -> String {
    std::env::var("OMLX_API_KEY").unwrap_or_default()
}

/// Run discovery against every endpoint that resolves; merge results.
/// Backend-level failures are logged and dropped (non-fatal).
async fn run_all_inner(
    ollama_endpoint: Option<String>,
    omlx_base_url: Option<String>,
    omlx_api_key: String,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let mut out = Vec::new();
    if let Some(ep) = ollama_endpoint {
        let d = OllamaDiscovery::new(ep);
        match d.list_models().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => tracing::warn!(?e, "Ollama discovery failed"),
        }
    }
    if let Some(ep) = omlx_base_url {
        let d = OMlxDiscovery::with_api_key(ep, omlx_api_key);
        match d.list_models().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => tracing::warn!(?e, "oMLX discovery failed"),
        }
    }
    Ok(out)
}

/// Public entry point for production use. Wires runtime detection from
/// `cmd/init_local.rs` to discovery endpoints. Reads `OMLX_API_KEY` from
/// the environment.
pub async fn run_all() -> anyhow::Result<Vec<DiscoveredModel>> {
    let runtimes = crate::cmd::init_local::detect_local_runtimes();
    let ollama = runtimes
        .ollama_running
        .then(|| "http://localhost:11434".to_string());
    let omlx = runtimes
        .omlx_installed
        .then(|| "http://localhost:8000/v1".to_string());
    run_all_inner(ollama, omlx, resolve_omlx_api_key()).await
}

/// Test-only entry: caller passes endpoints directly. Defaults the oMLX
/// API key to empty (no auth header sent — wiremock tests don't enforce auth).
#[doc(hidden)]
pub async fn run_all_for_test(
    ollama_endpoint: Option<String>,
    omlx_base_url: Option<String>,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    run_all_inner(ollama_endpoint, omlx_base_url, String::new()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_model_round_trips_serde() {
        let m = DiscoveredModel {
            id: "qwen3-embedding:0.6b".into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(1024),
            family: Some("bert".into()),
            size_bytes: Some(700_000_000),
            probed_at: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        let m2: DiscoveredModel = serde_json::from_str(&s).unwrap();
        assert_eq!(m.id, m2.id);
        assert_eq!(m.backend, m2.backend);
        assert_eq!(m.kind, m2.kind);
        assert_eq!(m.dims, m2.dims);
    }

    #[test]
    fn backend_display() {
        assert_eq!(format!("{}", Backend::Ollama), "Ollama");
        assert_eq!(format!("{}", Backend::OMlx), "oMLX");
    }

    #[test]
    fn embedding_probe_has_dims_and_latency() {
        let p = EmbeddingProbe {
            dims: 1024,
            latency_ms: 120,
        };
        assert_eq!(p.dims, 1024);
        assert_eq!(p.latency_ms, 120);
    }
}
