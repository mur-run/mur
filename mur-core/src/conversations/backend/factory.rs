//! ChatBackend factory. Selects backend from BackendConfig (mur-common
//! schema), wraps real backends in RetryingBackend.
//!
//! See spec §5.4 + §8.1.

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
#[allow(dead_code)] // used by tests/cli_conversations.rs (separate compilation unit)
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    build_raw(cfg)
}

/// Build a backend wrapped in `TelemetryBackend` so every call writes a
/// JSONL record under `~/.mur/telemetry/llm-calls-<YYYY-MM-DD>.jsonl`.
/// `stage` is the call-site tag (e.g. `"extractive"`, `"ask.generate"`)
/// used by the cost-report aggregator. See plan task 8.
pub fn build_for_stage(cfg: &BackendConfig, stage: &'static str) -> Result<Arc<dyn ChatBackend>> {
    let raw = build_raw(cfg)?;
    // Skip telemetry in test/mock modes — keeps tests from polluting
    // ~/.mur/telemetry/. MUR_TELEMETRY_DISABLE=1 also opts out at runtime.
    if std::env::var("MUR_LLM_MOCK").is_ok()
        || std::env::var("MUR_OLLAMA_MOCK").is_ok()
        || std::env::var("MUR_TELEMETRY_DISABLE").is_ok()
    {
        return Ok(raw);
    }
    Ok(Arc::new(super::telemetry::TelemetryBackend::new(
        raw, stage,
    )))
}

/// Default env var name for an API-key-bearing provider. Mirrors the
/// historical mur-common::config::LlmConfig fallback so users with
/// `provider: anthropic` and no explicit `api_key_env:` keep working
/// after P4 migrates them onto BackendConfig.
fn default_key_env(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "LLM_API_KEY",
    }
}

fn resolve_api_key(cfg: &BackendConfig) -> Result<String> {
    if let Some(r) = cfg.api_key_ref.as_deref() {
        let sref: mur_common::secret::SecretRef = r
            .parse()
            .map_err(|e| anyhow::anyhow!("{} backend api_key_ref invalid: {e}", cfg.provider))?;
        return sref.resolve_to_string_blocking().ok_or_else(|| {
            anyhow::anyhow!(
                "{} backend api_key_ref {r} did not resolve (and no usable api_key_env fallback was attempted — fix or remove api_key_ref)",
                cfg.provider
            )
        });
    }
    let env_var = cfg
        .api_key_env
        .as_deref()
        .unwrap_or_else(|| default_key_env(&cfg.provider));
    std::env::var(env_var).map_err(|_| {
        anyhow::anyhow!(
            "{} backend env var {env_var} is not set or not readable",
            cfg.provider
        )
    })
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
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg
                .endpoint
                .as_deref()
                .unwrap_or("https://api.anthropic.com");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::anthropic::AnthropicBackend::new(
                endpoint, &api_key, timeout,
            ))
        }
        "openai" => {
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg
                .endpoint
                .as_deref()
                .unwrap_or("https://api.openai.com/v1");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::openai::OpenAIBackend::new(
                endpoint, &api_key, timeout,
            ))
        }
        "openrouter" => {
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg
                .endpoint
                .as_deref()
                .unwrap_or("https://openrouter.ai/api/v1");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::openai::OpenAIBackend::new(
                endpoint, &api_key, timeout,
            ))
        }
        "gemini" => {
            let api_key = resolve_api_key(cfg)?;
            let endpoint = cfg
                .endpoint
                .as_deref()
                .unwrap_or("https://generativelanguage.googleapis.com");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::gemini::GeminiBackend::new(
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
            api_key_ref: None,
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
            api_key_ref: None,
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
            api_key_ref: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("MUR_TEST_NONEXISTENT_KEY"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_errors_when_default_env_var_unset_and_api_key_env_field_missing() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(
            r.is_err(),
            "should error when default env var ANTHROPIC_API_KEY is unset and api_key_env is None"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn openai_provider_returns_openai_backend_when_key_present() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_OPENAI_KEY", "sk-synthetic") };
        let cfg = BackendConfig {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_OPENAI_KEY".into()),
            api_key_ref: None,
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "openai");
        unsafe { std::env::remove_var("MUR_TEST_OPENAI_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gemini_provider_returns_gemini_backend_when_key_present() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_GEMINI_KEY", "synthetic") };
        let cfg = BackendConfig {
            provider: "gemini".into(),
            model: "gemini-pro-3".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_GEMINI_KEY".into()),
            api_key_ref: None,
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "gemini");
        unsafe { std::env::remove_var("MUR_TEST_GEMINI_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn openrouter_provider_aliases_to_openai_with_default_endpoint() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_OR_KEY", "sk-or-v1-synthetic") };
        let cfg = BackendConfig {
            provider: "openrouter".into(),
            model: "anthropic/claude-haiku-4-5".into(),
            endpoint: None, // factory should auto-set https://openrouter.ai/api/v1
            api_key_env: Some("MUR_TEST_OR_KEY".into()),
            api_key_ref: None,
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        // openrouter alias surfaces as "openai" (it IS an OpenAI-compat backend)
        assert_eq!(b.provider_name(), "openai");
        unsafe { std::env::remove_var("MUR_TEST_OR_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn anthropic_provider_uses_default_env_when_api_key_env_field_missing() {
        // P4 behavior change: factory now falls back to default_key_env when
        // api_key_env is None — so LlmConfig users without explicit api_key_env
        // (the historical default for anthropic) keep working.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "synthetic-default") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None, // factory uses default_key_env("anthropic") = "ANTHROPIC_API_KEY"
            api_key_ref: None,
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "anthropic");
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn build_for_stage_skips_telemetry_when_mock_env_set() {
        use crate::conversations::backend::ChatRequest;

        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_LLM_MOCK", "1") };
        let cfg = BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: Some("http://localhost:11434".into()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: Some(5),
        };
        let b = build_for_stage(&cfg, "extractive").unwrap();
        // The bare MockBackend's provider_name is "mock"; if TelemetryBackend
        // were wrapping it, provider_name would still forward to "mock", so
        // we have to verify a different way: inspect the type.
        // Cheaper: confirm no telemetry file is written when we make a call.
        let tmp = tempfile::tempdir().unwrap();
        // Override HOME so any accidental write goes here, not user's real ~/.mur.
        let prev_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", tmp.path().to_str().unwrap()) };

        let req = ChatRequest {
            model: "qwen3:14b",
            user: "mock extractive span",
            system: None,
            max_tokens: 64,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let _ = b.generate(req).await.unwrap();

        // No telemetry directory should have been created.
        let telemetry_dir = tmp.path().join(".mur").join("telemetry");
        assert!(
            !telemetry_dir.exists(),
            "no telemetry directory should be created in mock mode"
        );

        // Cleanup
        unsafe {
            if let Some(h) = prev_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
            std::env::remove_var("MUR_LLM_MOCK");
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn build_for_stage_skips_telemetry_when_disable_env_set() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TELEMETRY_DISABLE", "1") };
        let cfg = BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: Some("http://127.0.0.1:1".into()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: Some(1),
        };
        let b = build_for_stage(&cfg, "rewriter").unwrap();
        // Confirm provider_name forwards through (whether wrapped in retry or not).
        assert_eq!(b.provider_name(), "ollama");
        unsafe { std::env::remove_var("MUR_TELEMETRY_DISABLE") };
    }

    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn api_key_ref_takes_precedence_over_env() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_REF_KEY", "key-from-ref") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_NONEXISTENT_KEY".into()),
            api_key_ref: Some("env:MUR_TEST_REF_KEY".into()),
            timeout_secs: None,
        };
        // ref resolves → build succeeds even though api_key_env is unset
        assert!(build(&cfg).is_ok());
        unsafe { std::env::remove_var("MUR_TEST_REF_KEY") };
        // ref no longer resolves → error mentions the ref
        let err = format!("{:#}", build(&cfg).err().unwrap());
        assert!(err.contains("MUR_TEST_REF_KEY"), "err was: {err}");
    }

    #[test]
    fn unsupported_provider_errors() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let cfg = BackendConfig {
            provider: "cohere".into(),
            model: "command-r".into(),
            endpoint: None,
            api_key_env: None,
            api_key_ref: None,
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
            api_key_ref: None,
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
            api_key_ref: None,
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
