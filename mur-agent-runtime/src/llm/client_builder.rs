//! Client construction for a single resolved `ModelEntry` — secret
//! resolution, the guarded HTTP client, and provider dispatch. Split out of
//! `supervisor_runner.rs` (Task 7 review finding: that file exceeded the
//! 800-line mandate in CLAUDE.md §4) as a pure move — no logic change.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tracing::warn;

use crate::llm::LlmClient;
use crate::llm::{anthropic::AnthropicClient, ollama::OllamaClient, openai::OpenAiClient};
use crate::profile::Profile;
use crate::sandbox::reqwest_guard::HostGuard;
use mur_common::agent::NetworkOutboundMode;
use mur_common::model::ModelEntry;

/// Marks a `guarded_http` reqwest-client build failure so callers can detect
/// it and propagate the failure directly instead of reinterpreting it via
/// per-provider fallback logic. Pre-Task-7, the guarded HTTP client was built
/// once in `build_provider_runner` BEFORE the provider `match`, so a build
/// failure propagated as a hard `Err` out of the function regardless of
/// provider. Folding that build into `build_client_from_entry` (so each
/// fallback-chain candidate gets its own client) meant the single-model call
/// site's `match entry.provider.as_str()` Err-reinterpretation started
/// mislabeling a `guarded_http` failure for `local`/`ollama` (which have no
/// arm in that reinterpretation) as "unsupported model provider". Wrapping
/// the error lets the caller `downcast_ref` on it and restore the original
/// hard-fail semantic. See `build_provider_runner` in `supervisor_runner.rs`.
#[derive(Debug)]
pub(crate) struct GuardedHttpBuildError(pub anyhow::Error);

impl std::fmt::Display for GuardedHttpBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GuardedHttpBuildError {}

/// Build the `Arc<dyn LlmClient>` for a single resolved model entry: secret
/// resolution + guarded HTTP client + provider dispatch. Pure and synchronous
/// (uses `SecretRef::resolve_blocking` / the `block_on` helper below instead
/// of `.await`) so it can double as a `ClientFactory` for fallback-chain
/// candidates, which are built from inside a synchronous closure. `Err`
/// covers a `guarded_http` build failure (see `GuardedHttpBuildError` above —
/// callers must propagate this directly), "echo" (deliberate placeholder —
/// never has a real client), and unsupported providers; callers reconstruct
/// the differentiated stub/log behaviour for those two on `Err` (see
/// `build_provider_runner` in `supervisor_runner.rs`).
pub(crate) fn build_client_from_entry(
    entry: &ModelEntry,
    profile: &Profile,
    mur_home: &Path,
) -> anyhow::Result<Arc<dyn LlmClient>> {
    let secret_value: Option<secrecy::SecretString> = match &entry.secret {
        Some(s) => match s.resolve_blocking() {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(error = %e, "secret resolution failed; falling back to echo");
                None
            }
        },
        None => None,
    };

    let outbound = &profile.inner.entitlements.network.outbound;
    let host_guard = match outbound.mode {
        NetworkOutboundMode::Unrestricted => HostGuard::unrestricted(),
        NetworkOutboundMode::Restricted | NetworkOutboundMode::ProxyOnly => {
            // Auto-allow the agent's configured LLM provider host: choosing a
            // provider implies permission to reach it, so the user never has to
            // `mur agent perm allow-host` their own model endpoint. Mirrors the
            // loopback-port auto-grant for local models (`local_llm_port`).
            // ProxyOnly shares this host governance with Restricted — it still
            // needs to resolve its own LLM endpoint; it just loses general TCP
            // egress at the OS sandbox layer.
            let mut hosts = outbound.allow_hosts.clone();
            if let Some(h) = crate::supervisor_runner::provider_host(entry)
                && !hosts.iter().any(|x| x == &h)
            {
                hosts.push(h);
            }
            HostGuard::restricted(hosts)
        }
        NetworkOutboundMode::Off => HostGuard::off(),
    };
    let guarded_http = reqwest::ClientBuilder::new()
        .dns_resolver(std::sync::Arc::new(host_guard))
        .build()
        .context("failed to build guarded HTTP client")
        .map_err(GuardedHttpBuildError)?;

    match entry.provider.as_str() {
        "local" => {
            let base = crate::supervisor_runner::resolve_local_base_url(
                entry.base_url.as_deref(),
                std::env::var("MUR_LOCAL_LLM_BASE_URL").ok(),
                mur_home,
            );
            let key = secrecy::SecretString::from(
                crate::supervisor_runner::LOCAL_LLM_PLACEHOLDER_KEY.to_string(),
            );
            Ok(Arc::new(OpenAiClient::from_secret_string_with_http(
                &key,
                entry.model.clone(),
                Some(base),
                guarded_http,
            )))
        }
        "ollama" => {
            let base = entry.base_url.clone().unwrap_or_else(|| {
                std::env::var("OLLAMA_BASE_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string())
            });
            Ok(Arc::new(OllamaClient::with_http_client(
                base,
                entry.model.clone(),
                guarded_http,
            )))
        }
        "anthropic" => {
            if let Some(key) = secret_value.as_ref() {
                Ok(Arc::new(AnthropicClient::from_secret_string_with_http(
                    key,
                    entry.model.clone(),
                    entry.base_url.clone(),
                    guarded_http,
                )))
            } else {
                block_on(AnthropicClient::from_agent_credentials_with_http(
                    &profile.inner.name,
                    entry.model.clone(),
                    guarded_http,
                ))
                .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
                .map_err(anyhow::Error::from)
            }
        }
        "openai" => {
            if let Some(key) = secret_value.as_ref() {
                Ok(Arc::new(OpenAiClient::from_secret_string_with_http(
                    key,
                    entry.model.clone(),
                    entry.base_url.clone(),
                    guarded_http,
                )))
            } else {
                block_on(OpenAiClient::from_agent_credentials_with_http(
                    &profile.inner.name,
                    entry.model.clone(),
                    guarded_http,
                ))
                .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
                .map_err(anyhow::Error::from)
            }
        }
        "echo" => Err(anyhow::anyhow!("no model configured (echo placeholder)")),
        other => Err(anyhow::anyhow!("unsupported model provider {other:?}")),
    }
}

/// Block the current thread on a future from a synchronous context — needed
/// because `ClientFactory` (the fallback chain's per-candidate builder type)
/// is synchronous but credential resolution here is async. Mirrors
/// `SecretRef::resolve_blocking`'s pattern: `block_in_place` under the
/// current runtime handle (this crate's `#[tokio::main]` is multi-thread, see
/// `main.rs`), else a scratch current-thread runtime.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build scratch runtime")
            .block_on(fut),
    }
}
