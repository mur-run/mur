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
///
/// **Backwards-compatible: builds without telemetry.** Used by tests and any
/// caller that doesn't have a stage tag. Production call sites should use
/// `build_for_stage` so per-call cost telemetry is recorded.
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    build_raw(cfg)
}

/// Build a backend wrapped in `TelemetryBackend` so every call writes a
/// JSONL record under `~/.mur/telemetry/llm-calls-<YYYY-MM-DD>.jsonl`.
/// `stage` is the call-site tag (e.g. `"extractive"`, `"ask.generate"`)
/// used by the cost-report aggregator. See plan task 8.
pub fn build_for_stage(cfg: &BackendConfig, stage: &'static str) -> Result<Arc<dyn ChatBackend>> {
    let raw = build_raw(cfg)?;
    Ok(Arc::new(super::telemetry::TelemetryBackend::new(
        raw, stage,
    )))
}

fn build_raw(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
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

    // ── I3 — RetryingBackend::generate_stream connect-retry through the
    // real AnthropicBackend SSE parser. Earlier unit tests used a
    // hand-rolled inner backend; this verifies the SSE parser actually
    // survives the retry-on-connect path end-to-end (closes I3).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn factory_retries_anthropic_503_then_streams_via_real_sse_parser() {
        use crate::conversations::backend::ChatRequest;
        use futures::StreamExt;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY_I3", "k") };

        let server = MockServer::start().await;

        // First two attempts: 503. Third: real SSE stream.
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;

        let sse = "\
event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}

event: message_stop
data: {\"type\":\"message_stop\"}

";
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;

        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: Some(server.uri()),
            api_key_env: Some("MUR_TEST_ANTHROPIC_KEY_I3".into()),
            timeout_secs: Some(5),
        };
        let b = build(&cfg).unwrap();
        let req = ChatRequest {
            model: "claude-haiku-4-5",
            user: "hi",
            system: None,
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let mut stream = b.generate_stream(req).await.unwrap();
        let mut text = String::new();
        let mut final_usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            text.push_str(&chunk.delta);
            if let Some(u) = chunk.usage {
                final_usage = Some(u);
            }
        }
        assert_eq!(text, "OK");
        let u = final_usage.expect("usage from final chunk");
        assert_eq!(u.input_tokens, 3);
        assert_eq!(u.output_tokens, 1);
        unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY_I3") };
    }

    // ── I4 — factory composes TelemetryBackend → RetryingBackend →
    // AnthropicBackend in the right order. After 1× 503 + 1× 200,
    // telemetry must record exactly ONE line for the final retry success
    // (not one per attempt). Closes I4.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn factory_for_stage_records_one_telemetry_line_after_retry_succeeds() {
        use crate::conversations::backend::ChatRequest;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY_I4", "k") };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(
                        r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":3,"output_tokens":1}}"#,
                    ),
            )
            .mount(&server)
            .await;

        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: Some(server.uri()),
            api_key_env: Some("MUR_TEST_ANTHROPIC_KEY_I4".into()),
            timeout_secs: Some(5),
        };

        // Use build (returns the retry+anthropic stack), then wrap in
        // TelemetryBackend manually with a path override so the test's
        // assertions can read the JSONL without polluting ~/.mur.
        let raw = build(&cfg).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("test-telemetry.jsonl");
        let tb = super::super::telemetry::TelemetryBackend::new(raw, "extractive")
            .with_path_override(log_path.clone());

        let req = ChatRequest {
            model: "claude-haiku-4-5",
            user: "hi",
            system: None,
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let _ = tb.generate(req).await.unwrap();

        let body = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            body.lines().count(),
            1,
            "telemetry should record exactly ONE line for the final retry success, not one per attempt"
        );
        let rec: super::super::telemetry::LlmCallRecord =
            serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert!(rec.success);
        assert_eq!(rec.input_tokens, 3);
        assert_eq!(rec.output_tokens, 1);
        unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY_I4") };
    }
}
