//! ChatBackend factory. Selects backend from BackendConfig (mur-common
//! schema), wraps real backends in RetryingBackend.
//!
//! See spec §5.4 + §8.1.

#![allow(dead_code)] // wired into more call sites across P1.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use mur_common::config::BackendConfig;

use super::{ChatBackend, mock::MockBackend, ollama::OllamaBackend};

/// Build a backend from BackendConfig. Honors MUR_LLM_MOCK / MUR_OLLAMA_MOCK
/// env vars: when either is set, returns a bare MockBackend (no retry wrapper)
/// for deterministic test timing.
///
/// Real providers (ollama, anthropic) are wrapped in
/// `RetryingBackend::with_default_policy` so all callers inherit retries
/// on `BackendError::{Timeout, ServerError(5xx), RateLimited}`.
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok() || std::env::var("MUR_OLLAMA_MOCK").is_ok() {
        tracing::debug!(provider = %cfg.provider, "MUR_LLM_MOCK active — using MockBackend");
        return Ok(Arc::new(MockBackend::new()));
    }
    let inner: Arc<dyn ChatBackend> = match cfg.provider.as_str() {
        "ollama" => {
            let endpoint = cfg.endpoint.as_deref().unwrap_or("http://localhost:11434");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(OllamaBackend::new(endpoint, timeout))
        }
        "anthropic" => {
            let api_key_env = cfg.api_key_env.as_deref().ok_or_else(|| {
                anyhow::anyhow!("anthropic backend requires api_key_env in BackendConfig")
            })?;
            let api_key = std::env::var(api_key_env).map_err(|_| {
                anyhow::anyhow!(
                    "anthropic backend env var {api_key_env} is not set or not readable"
                )
            })?;
            let endpoint = cfg
                .endpoint
                .as_deref()
                .unwrap_or("https://api.anthropic.com");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::anthropic::AnthropicBackend::new(
                endpoint, &api_key, timeout,
            ))
        }
        other => bail!("unsupported provider: {other}"),
    };
    Ok(Arc::new(
        super::retry::RetryingBackend::with_default_policy(inner),
    ))
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
    async fn ollama_provider_returns_ollama_backend_through_retry_wrapper() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let cfg = ollama_cfg("http://127.0.0.1:1", 1);
        let b = build(&cfg).unwrap();
        // RetryingBackend forwards provider_name() to inner.
        assert_eq!(b.provider_name(), "ollama");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_returns_anthropic_backend_when_key_present() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        // Use a synthetic env var so the test doesn't depend on ANTHROPIC_API_KEY.
        unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY", "synthetic-key") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_ANTHROPIC_KEY".into()),
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "anthropic");
        unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_errors_when_key_env_missing() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("MUR_TEST_NONEXISTENT_KEY") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_NONEXISTENT_KEY".into()),
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("MUR_TEST_NONEXISTENT_KEY"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_errors_when_api_key_env_field_missing() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("api_key_env"));
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
