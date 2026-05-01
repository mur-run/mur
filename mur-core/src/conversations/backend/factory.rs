//! ChatBackend factory. Selects backend from a thin BackendSpec
//! (P0 minimal — full BackendConfig schema lands in P1).
//!
//! See spec §5.4.

#![allow(dead_code)] // wired into ask::rewriter in Task 6.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};

use super::{ChatBackend, mock::MockBackend, ollama::OllamaBackend};

/// P0 minimal backend specification. Will be replaced by the full
/// BackendConfig struct from `mur-common` in P1, when per-stage
/// schema lands. Kept local here so P0 doesn't touch mur-common.
#[derive(Debug, Clone)]
pub struct BackendSpec {
    pub provider: String,
    pub endpoint: Option<String>,
    pub timeout_secs: Option<u64>,
}

impl BackendSpec {
    pub fn ollama(endpoint: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            provider: "ollama".into(),
            endpoint: Some(endpoint.into()),
            timeout_secs: Some(timeout_secs),
        }
    }
}

/// Build a backend from spec. Honors MUR_LLM_MOCK / MUR_OLLAMA_MOCK
/// env vars: when either is set, returns MockBackend regardless of spec.
pub fn build(spec: &BackendSpec) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok() || std::env::var("MUR_OLLAMA_MOCK").is_ok() {
        tracing::debug!(provider = %spec.provider, "MUR_LLM_MOCK active — using MockBackend");
        return Ok(Arc::new(MockBackend::new()));
    }
    match spec.provider.as_str() {
        "ollama" => {
            let endpoint = spec.endpoint.as_deref().unwrap_or("http://localhost:11434");
            let timeout = Duration::from_secs(spec.timeout_secs.unwrap_or(120));
            Ok(Arc::new(OllamaBackend::new(endpoint, timeout)))
        }
        other => bail!("unsupported provider in P0: {other} (anthropic lands in P1)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_env_var_forces_mock_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_LLM_MOCK", "1") };
        let spec = BackendSpec::ollama("http://localhost:11434", 5);
        let b = build(&spec).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn legacy_mur_ollama_mock_env_var_also_forces_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let spec = BackendSpec::ollama("http://localhost:11434", 5);
        let b = build(&spec).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ollama_provider_returns_ollama_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let spec = BackendSpec::ollama("http://127.0.0.1:1", 1);
        let b = build(&spec).unwrap();
        assert_eq!(b.provider_name(), "ollama");
    }

    #[test]
    fn unsupported_provider_errors() {
        let spec = BackendSpec {
            provider: "openai".into(),
            endpoint: None,
            timeout_secs: None,
        };
        let r = build(&spec);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("unsupported"));
    }
}
