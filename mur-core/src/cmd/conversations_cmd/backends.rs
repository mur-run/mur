//! Per-stage backend listing + cloud-provider probing for
//! `mur conversations doctor`.
//!
//! Split out of `doctor.rs` into its own sibling module (rather than grown
//! inline) to keep `doctor.rs` and `mod.rs` under the repo's 800-line cap —
//! `mod.rs` is already over it. Two independent features live here:
//!
//! 1. `stage_backend_rows` / `render_stage_backends_table` — names all six
//!    real call sites (`ask.generate`, `ask.rewriter`, `compact.extractive`,
//!    `compact.abstractive`, `rollup.extractive`, `rollup.abstractive`) next
//!    to the provider/model/endpoint each resolves to today, and whether
//!    that's an explicit per-stage override (`[pinned]`) or inherited from
//!    the smart slot `config.llm` (`[follows smart]`).
//! 2. `probe_and_print_cloud_backends` — extends doctor's cloud-provider
//!    probing beyond anthropic-only (previously `doctor.rs` filtered
//!    `|b| b.provider == "anthropic"`, so an `openai`-provider backend
//!    pointed at a local runtime such as omlx was silently skipped and
//!    misreported as "no cloud providers in active config"). `openai` and
//!    `openrouter` now get a live `GET {endpoint}/models` listing check;
//!    `anthropic` keeps its existing key-check + live reachability probe;
//!    `gemini` gets the key-check only — deliberately no live call, to avoid
//!    a billable API hit from a health check.
//!
//! Both features derive their input from the SAME already-computed
//! `collect_backend_configs(&cfg)` list `doctor.rs` already builds — the
//! deduped set of the six real per-stage resolved backends — never from
//! `cfg.llm` (the smart slot) directly. That is what keeps an unused
//! provider or endpoint from ever turning doctor red (fix round 1, finding
//! 1's sibling guarantee, extended to cloud providers here).

use std::time::Duration;

use mur_common::config::{BackendConfig, Config};

use crate::conversations::backend::factory::{default_endpoint, resolve_api_key};
use crate::model_discovery::parse_models_response;

/// One row of the `conversations backends` listing: which real call site
/// (`stage`), what it resolves to (`backend`), and whether that's an
/// explicit per-stage override (`pinned = true`) or inherited from the
/// smart slot (`config.llm`, `pinned = false`).
#[derive(Debug)]
pub(super) struct StageBackendRow {
    pub(super) stage: &'static str,
    pub(super) backend: BackendConfig,
    pub(super) pinned: bool,
}

/// Resolves all six conversations-pipeline call sites against `cfg`, using
/// the same `effective_*_backend` resolvers the pipeline itself calls at
/// runtime (`mur-common::config`) — so this listing can never drift from
/// what the pipeline actually dials.
pub(super) fn stage_backend_rows(cfg: &Config) -> Vec<StageBackendRow> {
    vec![
        StageBackendRow {
            stage: "ask.generate",
            pinned: cfg.conversations.ask.backend.is_some(),
            backend: cfg.conversations.ask.effective_backend(&cfg.llm),
        },
        StageBackendRow {
            stage: "ask.rewriter",
            pinned: cfg.conversations.ask.rewriter_backend.is_some(),
            backend: cfg.conversations.ask.effective_rewriter_backend(&cfg.llm),
        },
        StageBackendRow {
            stage: "compact.extractive",
            pinned: cfg.conversations.compact.extractive_backend.is_some(),
            backend: cfg
                .conversations
                .compact
                .effective_extractive_backend(&cfg.llm),
        },
        StageBackendRow {
            stage: "compact.abstractive",
            pinned: cfg.conversations.compact.abstractive_backend.is_some(),
            backend: cfg
                .conversations
                .compact
                .effective_abstractive_backend(&cfg.llm),
        },
        StageBackendRow {
            stage: "rollup.extractive",
            pinned: cfg.conversations.rollup.extractive_backend.is_some(),
            backend: cfg
                .conversations
                .rollup
                .effective_extractive_backend(&cfg.llm),
        },
        StageBackendRow {
            stage: "rollup.abstractive",
            pinned: cfg.conversations.rollup.abstractive_backend.is_some(),
            backend: cfg
                .conversations
                .rollup
                .effective_abstractive_backend(&cfg.llm),
        },
    ]
}

/// The endpoint a row's backend will actually dial when `BackendConfig.endpoint`
/// is `None`. Delegates entirely to `factory::default_endpoint` — the single
/// source of truth `build_raw` itself dials through — rather than keeping a
/// second copy of the provider match here. (Fix round 1: the two used to be
/// separate copies of the same match; a provider added to one without the
/// other let doctor silently print a fallback string for a provider that
/// actually dials fine.)
fn resolved_endpoint(b: &BackendConfig) -> String {
    b.endpoint.clone().unwrap_or_else(|| {
        default_endpoint(&b.provider)
            .map(str::to_string)
            .unwrap_or_else(|| format!("(no default endpoint for {})", b.provider))
    })
}

/// Renders the `conversations backends` table: header line + one line per
/// row, column widths sized to the widest value actually present so
/// arbitrary provider/model/endpoint strings never get truncated or
/// misaligned. Each returned line (including the last) ends in `\n`, so
/// callers should `print!` rather than `println!` the result.
pub(super) fn render_stage_backends_table(rows: &[StageBackendRow]) -> String {
    let endpoints: Vec<String> = rows.iter().map(|r| resolved_endpoint(&r.backend)).collect();

    let stage_w = rows.iter().map(|r| r.stage.len()).max().unwrap_or(0) + 2;
    let provider_w = rows
        .iter()
        .map(|r| r.backend.provider.len())
        .max()
        .unwrap_or(0)
        + 2;
    let model_w = rows
        .iter()
        .map(|r| r.backend.model.len())
        .max()
        .unwrap_or(0)
        + 2;
    let endpoint_w = endpoints.iter().map(|e| e.len()).max().unwrap_or(0) + 2;

    let mut out = String::from("conversations backends\n");
    for (row, endpoint) in rows.iter().zip(endpoints.iter()) {
        let marker = if row.pinned {
            "[pinned]"
        } else {
            "[follows smart]"
        };
        let stage = row.stage;
        let provider = row.backend.provider.as_str();
        let model = row.backend.model.as_str();
        out.push_str(&format!(
            "  {stage:stage_w$}{provider:provider_w$}{model:model_w$}{endpoint:endpoint_w$}{marker}\n"
        ));
    }
    out
}

/// Cloud-provider backends among `backends` (already scoped to the six real
/// per-stage resolved backends via `collect_backend_configs` — see this
/// module's doc comment). Filtering THIS list, rather than probing every
/// known provider unconditionally, is what keeps an unused provider from
/// ever being reported as a doctor failure.
fn cloud_backends(backends: &[BackendConfig]) -> Vec<&BackendConfig> {
    backends
        .iter()
        .filter(|b| {
            matches!(
                b.provider.as_str(),
                "anthropic" | "gemini" | "openai" | "openrouter"
            )
        })
        .collect()
}

/// Matches the pre-existing anthropic probe's budget — doctor is a quick
/// health check, not a place to hang on a dead endpoint.
const CLOUD_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Prints one probe result per cloud-provider backend actually routed to by
/// a conversations stage. No-ops (with a `·` note) when none are — an idle
/// cloud provider nobody references must not turn doctor red, same
/// discipline as the existing Ollama-endpoint probing.
pub(super) async fn probe_and_print_cloud_backends(backends: &[BackendConfig]) {
    let cloud = cloud_backends(backends);
    if cloud.is_empty() {
        println!("  · no cloud providers in active config (skipping cloud probes)");
        return;
    }
    for b in cloud {
        match b.provider.as_str() {
            "anthropic" => probe_and_print_anthropic(b).await,
            "gemini" => probe_and_print_gemini(b),
            _ => probe_and_print_openai_compatible(b).await,
        }
    }
}

async fn probe_and_print_anthropic(b: &BackendConfig) {
    let key = match resolve_api_key(b) {
        Ok(k) => {
            println!("  ✓ anthropic API key resolved for {}", b.model);
            k
        }
        Err(e) => {
            println!("  ✗ anthropic API key not resolved for {}: {e}", b.model);
            return;
        }
    };
    // Reachability + model-existence probe (non-fatal; doctor never exits
    // non-zero on a failed probe here — only preflight gates on `ok`).
    let endpoint = resolved_endpoint(b);
    let url = format!("{}/v1/models/{}", endpoint.trim_end_matches('/'), b.model);
    let probe = tokio::time::timeout(
        CLOUD_PROBE_TIMEOUT,
        reqwest::Client::new()
            .get(&url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send(),
    )
    .await;
    match probe {
        Ok(Ok(r)) if r.status().is_success() => {
            println!("  ✓ anthropic model {} reachable at {endpoint}", b.model);
        }
        Ok(Ok(r)) => {
            println!(
                "  ✗ anthropic model {} returned {} at {endpoint}",
                b.model,
                r.status()
            );
        }
        Ok(Err(e)) => {
            println!("  ✗ anthropic probe for {} failed: {e}", b.model);
        }
        Err(_) => {
            println!(
                "  · anthropic probe for {} timed out at {endpoint} ({}s)",
                b.model,
                CLOUD_PROBE_TIMEOUT.as_secs()
            );
        }
    }
}

/// Key-resolution check only — deliberately no live network call. Gemini's
/// API can be billable per-request, and doctor is a read-only health check;
/// unlike anthropic's `/v1/models/{model}` metadata probe (pre-existing,
/// kept as-is), we don't add a new live Gemini call here.
fn probe_and_print_gemini(b: &BackendConfig) {
    match resolve_api_key(b) {
        Ok(_) => println!("  ✓ gemini API key resolved for {}", b.model),
        Err(e) => println!("  ✗ gemini API key not resolved for {}: {e}", b.model),
    }
}

/// Covers `openai` and `openrouter` — including local OpenAI-compatible
/// runtimes (omlx, LM Studio, etc.) dialed via `provider: openai` +
/// `openai_url`. A missing/unresolved key is not fatal here: local runtimes
/// typically need none, so the probe still attempts the call unauthenticated
/// rather than failing closed.
async fn probe_and_print_openai_compatible(b: &BackendConfig) {
    let endpoint = resolved_endpoint(b);
    let key = resolve_api_key(b).ok();
    match probe_openai_compatible(&endpoint, &b.model, key.as_deref(), CLOUD_PROBE_TIMEOUT).await {
        OpenAiCompatProbe::Listed {
            model_found: true, ..
        } => {
            println!("  ✓ {} model {} listed at {endpoint}", b.provider, b.model);
        }
        OpenAiCompatProbe::Listed {
            model_found: false,
            total_models,
        } => {
            println!(
                "  ✗ {} model {} NOT found among {total_models} models listed at {endpoint}",
                b.provider, b.model
            );
        }
        OpenAiCompatProbe::BadStatus(status) => {
            println!("  ✗ {} {endpoint}/models returned {status}", b.provider);
        }
        OpenAiCompatProbe::NetworkError(e) => {
            println!("  ✗ {} probe for {} failed: {e}", b.provider, b.model);
        }
        OpenAiCompatProbe::TimedOut => {
            println!(
                "  · {} probe for {} timed out at {endpoint} ({}s)",
                b.provider,
                b.model,
                CLOUD_PROBE_TIMEOUT.as_secs()
            );
        }
    }
}

/// Outcome of probing one OpenAI-compatible endpoint's `/models` listing.
#[derive(Debug)]
enum OpenAiCompatProbe {
    /// Endpoint reachable and returned a parseable model list.
    Listed {
        model_found: bool,
        total_models: usize,
    },
    /// Endpoint reachable but returned a non-2xx status.
    BadStatus(reqwest::StatusCode),
    /// Connection/request-level failure (refused, DNS, TLS, ...).
    NetworkError(String),
    /// No response within the probe budget.
    TimedOut,
}

/// GETs `{endpoint}/models` and reports whether `model` is in the returned
/// list. `api_key`, if present, is sent as `Authorization: Bearer` (the
/// convention `conversations::backend::openai::OpenAIBackend` itself uses).
/// Async + `reqwest::Client` (not the blocking `model_discovery::discover_models_for`)
/// specifically so this stays safely callable from a `#[tokio::test]` +
/// `wiremock::MockServer` test without deadlocking on a blocking `.join()`.
async fn probe_openai_compatible(
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
    timeout: Duration,
) -> OpenAiCompatProbe {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let mut req = reqwest::Client::new().get(&url);
    if let Some(k) = api_key {
        req = req.header("Authorization", format!("Bearer {k}"));
    }
    match tokio::time::timeout(timeout, req.send()).await {
        Ok(Ok(resp)) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            let models = parse_models_response(&body);
            let model_found = models.iter().any(|m| m == model);
            OpenAiCompatProbe::Listed {
                model_found,
                total_models: models.len(),
            }
        }
        Ok(Ok(resp)) => OpenAiCompatProbe::BadStatus(resp.status()),
        Ok(Err(e)) => OpenAiCompatProbe::NetworkError(e.to_string()),
        Err(_) => OpenAiCompatProbe::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::config::LlmConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The exact real-world case this whole change set exists for: a local
    /// omlx runtime configured as the smart slot (`provider: omlx` with no
    /// built-in handling, `openai_url` set), one stage explicitly pinned
    /// elsewhere. Every un-pinned stage must surface the omlx-aliased
    /// `openai` provider at the omlx endpoint, not "no provider"/"unknown".
    #[test]
    fn stage_backend_rows_reports_pinned_vs_follows_smart_with_omlx_provider_alias() {
        let mut cfg = Config {
            llm: LlmConfig {
                provider: "omlx".to_string(),
                model: "Qwen3.5-4B-MLX-4bit".to_string(),
                api_key_env: None,
                api_key_ref: None,
                openai_url: Some("http://127.0.0.1:8000/v1".to_string()),
            },
            ..Default::default()
        };
        cfg.conversations.ask.backend = Some(BackendConfig {
            provider: "openai".to_string(),
            model: "Qwen3.5-4B-MLX-4bit".to_string(),
            endpoint: Some("http://127.0.0.1:8000/v1".to_string()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: None,
        });

        let rows = stage_backend_rows(&cfg);
        assert_eq!(rows.len(), 6);
        let by_stage = |s: &str| rows.iter().find(|r| r.stage == s).unwrap();

        let ask_generate = by_stage("ask.generate");
        assert!(ask_generate.pinned, "ask.generate has an explicit override");
        assert_eq!(ask_generate.backend.provider, "openai");
        assert_eq!(
            ask_generate.backend.endpoint.as_deref(),
            Some("http://127.0.0.1:8000/v1")
        );

        for stage in [
            "ask.rewriter",
            "compact.extractive",
            "compact.abstractive",
            "rollup.extractive",
            "rollup.abstractive",
        ] {
            let row = by_stage(stage);
            assert!(!row.pinned, "{stage} should follow smart, not be pinned");
            assert_eq!(
                row.backend.provider, "openai",
                "{stage} must alias omlx -> openai via LlmConfig::to_backend_config"
            );
            assert_eq!(
                row.backend.endpoint.as_deref(),
                Some("http://127.0.0.1:8000/v1"),
                "{stage} must inherit the omlx endpoint"
            );
        }

        let table = render_stage_backends_table(&rows);
        assert!(table.starts_with("conversations backends\n"));

        let ask_generate_line = table.lines().find(|l| l.contains("ask.generate")).unwrap();
        assert!(ask_generate_line.contains("[pinned]"));
        assert!(ask_generate_line.contains("openai"));
        assert!(ask_generate_line.contains("http://127.0.0.1:8000/v1"));

        let ask_rewriter_line = table.lines().find(|l| l.contains("ask.rewriter")).unwrap();
        assert!(ask_rewriter_line.contains("[follows smart]"));
        assert!(ask_rewriter_line.contains("openai"));
    }

    /// Mirrors the pre-existing Ollama-endpoint guarantee (fix round 1,
    /// finding 1): a config where every stage routes to Ollama must never
    /// surface a cloud-provider probe target — an unused provider can't
    /// turn doctor red if it's never in the probe set to begin with.
    #[test]
    fn cloud_backends_empty_when_no_stage_routes_to_a_cloud_provider() {
        let mut cfg = Config::default();
        cfg.llm.provider = "ollama".to_string();
        cfg.llm.model = "qwen3:4b".to_string();
        cfg.llm.api_key_env = None;
        let backends = super::super::collect_backend_configs(&cfg);
        assert!(
            cloud_backends(&backends).is_empty(),
            "an all-ollama config must not surface any cloud-provider probe target"
        );
    }

    /// The other half of the same guarantee: once a stage DOES route to
    /// openai/openrouter, both must show up (this is the extension over the
    /// old anthropic-only filter).
    #[test]
    fn cloud_backends_includes_openai_and_openrouter_alongside_anthropic() {
        let mut cfg = Config::default();
        cfg.llm.provider = "openai".to_string();
        cfg.llm.model = "gpt-4o-mini".to_string();
        cfg.llm.api_key_env = Some("OPENAI_API_KEY".to_string());
        cfg.conversations.compact.extractive_backend = Some(BackendConfig {
            provider: "openrouter".to_string(),
            model: "meta-llama/llama-4".to_string(),
            endpoint: None,
            api_key_env: Some("OPENROUTER_API_KEY".to_string()),
            api_key_ref: None,
            timeout_secs: None,
        });
        let backends = super::super::collect_backend_configs(&cfg);
        let cloud = cloud_backends(&backends);
        assert!(cloud.iter().any(|b| b.provider == "openai"));
        assert!(cloud.iter().any(|b| b.provider == "openrouter"));
    }

    #[tokio::test]
    async fn probe_openai_compatible_reports_model_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"data":[{"id":"Qwen3.5-4B-MLX-4bit"},{"id":"other-model"}]}"#,
                ),
            )
            .mount(&server)
            .await;

        let endpoint = format!("{}/v1", server.uri());
        let outcome = probe_openai_compatible(
            &endpoint,
            "Qwen3.5-4B-MLX-4bit",
            None,
            Duration::from_secs(2),
        )
        .await;

        match outcome {
            OpenAiCompatProbe::Listed {
                model_found,
                total_models,
            } => {
                assert!(model_found, "configured model must be reported present");
                assert_eq!(total_models, 2);
            }
            other => panic!("expected Listed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_openai_compatible_reports_model_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"data":[{"id":"some-other-model"}]}"#),
            )
            .mount(&server)
            .await;

        let endpoint = format!("{}/v1", server.uri());
        let outcome =
            probe_openai_compatible(&endpoint, "not-there", None, Duration::from_secs(2)).await;

        match outcome {
            OpenAiCompatProbe::Listed {
                model_found,
                total_models,
            } => {
                assert!(!model_found, "an unlisted model must be reported missing");
                assert_eq!(total_models, 1);
            }
            other => panic!("expected Listed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_openai_compatible_reports_bad_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let endpoint = format!("{}/v1", server.uri());
        let outcome =
            probe_openai_compatible(&endpoint, "whatever", None, Duration::from_secs(2)).await;

        match outcome {
            OpenAiCompatProbe::BadStatus(status) => assert_eq!(status.as_u16(), 401),
            other => panic!("expected BadStatus, got {other:?}"),
        }
    }
}
