//! ChatBackend factory. Selects backend from BackendConfig (mur-common
//! schema). See spec §5.4.

#![allow(dead_code)] // wired into more call sites across P1.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use mur_common::config::BackendConfig;

use super::{ChatBackend, mock::MockBackend, ollama::OllamaBackend};

/// Build a backend from BackendConfig. Honors MUR_LLM_MOCK / MUR_OLLAMA_MOCK
/// env vars: when either is set, returns MockBackend regardless of cfg.
///
/// AnthropicBackend wiring lands in Task 5; until then "anthropic" provider
/// returns an error. Task 6 wraps the result in RetryingBackend.
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok() || std::env::var("MUR_OLLAMA_MOCK").is_ok() {
        tracing::debug!(provider = %cfg.provider, "MUR_LLM_MOCK active — using MockBackend");
        return Ok(Arc::new(MockBackend::new()));
    }
    match cfg.provider.as_str() {
        "ollama" => {
            let endpoint = cfg.endpoint.as_deref().unwrap_or("http://localhost:11434");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Ok(Arc::new(OllamaBackend::new(endpoint, timeout)))
        }
        "anthropic" => bail!("anthropic backend not yet wired (lands in P1 Task 5)"),
        other => bail!("unsupported provider: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ollama_cfg(endpoint: &str, timeout_secs: u64) -> BackendConfig {
        BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: Some(endpoint.into()),
            api_key_env: None,
            timeout_secs: Some(timeout_secs),
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_env_var_forces_mock_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_LLM_MOCK", "1") };
        let cfg = ollama_cfg("http://localhost:11434", 5);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn legacy_mur_ollama_mock_env_var_also_forces_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let cfg = ollama_cfg("http://localhost:11434", 5);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ollama_provider_returns_ollama_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let cfg = ollama_cfg("http://127.0.0.1:1", 1);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "ollama");
    }

    #[test]
    fn anthropic_provider_unwired_in_task_3() {
        // Will become a real test in Task 5 when AnthropicBackend lands.
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("not yet wired"));
    }

    #[test]
    fn unsupported_provider_errors() {
        let cfg = BackendConfig {
            provider: "openai".into(),
            model: "gpt-4".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("unsupported"));
    }
}
