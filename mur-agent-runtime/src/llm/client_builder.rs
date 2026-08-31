//! Client construction for a single resolved `ModelEntry` — secret
//! resolution, the guarded HTTP client, and provider dispatch. Split out of
//! `supervisor_runner.rs` (Task 7 review finding: that file exceeded the
//! 800-line mandate in CLAUDE.md §4) as a pure move — no logic change.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tracing::warn;

use crate::llm::{LlmClient, LlmError};
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
    let client = build_bare_client(entry, profile, mur_home)?;
    Ok(Arc::new(EndpointNamed {
        inner: client,
        context: endpoint_context(entry),
    }))
}

/// Describe where a request goes and which credential it carries.
///
/// `SecretRef`'s Display is the *reference* — `file:…`, `keychain:svc/acct`,
/// `env:VAR` — never the value, which is why it is safe to print here and why
/// `mur agent doctor` already prints the same form.
fn endpoint_context(entry: &ModelEntry) -> String {
    let endpoint = entry
        .base_url
        .as_deref()
        .unwrap_or("the provider's default endpoint");
    // `label`, not `to_string`: a `cmd:` reference Displays its whole command
    // line, and this string travels further than `mur agent doctor`'s stdout —
    // it lands in the turn error, the TUI and whatever persists that.
    let secret = match &entry.secret {
        Some(s) => s.label(),
        None => "the agent's own credentials (no secret ref)".to_string(),
    };
    // A net under the structural fix above: a base URL can carry userinfo and a
    // provider can echo a key back in a body we later concatenate. Catches only
    // known key shapes, which is why it is the second line of defence.
    let line = format!(
        "[auth] this request went to {endpoint} as {}/{}, carrying {secret}. \
         The endpoint rejected that credential — the model entry, the endpoint and the \
         credential are all named here so the wrong one can be found without guessing.",
        entry.provider, entry.model
    );
    mur_common::redact::redact_secrets(&line).into_owned()
}

/// Names the endpoint on an authentication failure.
///
/// A 401 body says "API key is invalid" and nothing about *which* key or
/// *which* endpoint, which leaves an operator with three suspects and no way
/// to narrow them. The facts are in the `ModelEntry` this was built from, and
/// they cannot be recovered when the error arrives: `~/.mur/models.yaml` is
/// not in an agent's read grants, so looking them up later fails exactly the
/// way the write-denial advisory did (#1087). They are captured once, here.
///
/// Only `Auth` is touched. Every other class either already names its cause or
/// is a candidate for the fallback chain, and decorating those would put this
/// paragraph in front of failures it does not explain.
struct EndpointNamed {
    inner: Arc<dyn LlmClient>,
    context: String,
}

impl EndpointNamed {
    fn name_endpoint(&self, e: LlmError) -> LlmError {
        match e {
            LlmError::Auth(status, body) => {
                LlmError::Auth(status, format!("{body}\n\n{}", self.context))
            }
            other => other,
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for EndpointNamed {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, LlmError> {
        self.inner
            .generate(req)
            .await
            .map_err(|e| self.name_endpoint(e))
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn generate_stream(
        &self,
        req: crate::llm::LlmRequest,
        sink: tokio::sync::mpsc::Sender<crate::llm::StreamDelta>,
    ) -> Result<crate::llm::LlmResponse, LlmError> {
        self.inner
            .generate_stream(req, sink)
            .await
            .map_err(|e| self.name_endpoint(e))
    }
}

fn build_bare_client(
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

#[cfg(test)]
mod endpoint_named_tests {
    use super::*;
    use mur_common::model::ModelEntry;
    use mur_common::secret::SecretRef;

    fn entry() -> ModelEntry {
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
            base_url: Some("http://127.0.0.1:8088".into()),
            secret: Some(SecretRef::File(
                "/Users/d/.mur/secrets/anthropic.key".into(),
            )),
            ..Default::default()
        }
    }

    /// The dead end this exists to remove: "API key is invalid" names neither
    /// the key nor the endpoint, so the credential, the base URL and the model
    /// entry are all suspects and the error rules out none of them.
    #[test]
    fn the_context_names_endpoint_model_and_credential() {
        let c = endpoint_context(&entry());
        assert!(c.contains("127.0.0.1:8088"), "{c}");
        assert!(c.contains("claude-opus-5"), "{c}");
        assert!(
            c.contains("file:/Users/d/.mur/secrets/anthropic.key"),
            "{c}"
        );
    }

    /// `SecretRef`'s Display is the reference, not the value — but a test is
    /// the only thing that keeps it that way if the type ever changes.
    #[test]
    fn the_context_carries_a_reference_never_a_value() {
        let c = endpoint_context(&entry());
        assert!(c.contains("file:"), "must name the ref form: {c}");
        // A resolved key would look nothing like a path; assert the shape we
        // print rather than trying to detect a secret after the fact.
        assert!(!c.contains("sk-"), "{c}");
    }

    /// This string travels further than `mur agent doctor`'s stdout — into the
    /// turn error, the TUI, and whatever persists that — so a `cmd:` reference
    /// must not bring its command line along.
    #[test]
    fn a_command_credential_does_not_carry_its_arguments_here() {
        let mut e = entry();
        e.secret = Some(SecretRef::Cmd("vault read --token=orgtok123".into()));
        let c = endpoint_context(&e);
        assert!(!c.contains("orgtok123"), "{c}");
        assert!(c.contains("cmd:vault"), "{c}");
    }

    /// The pattern net under the structural fix: a key shape reaching this
    /// string by any other route is still removed.
    #[test]
    fn a_key_shaped_string_anywhere_in_the_context_is_redacted() {
        let mut e = entry();
        e.base_url = Some("https://proxy/sk-ant-0123456789abcdefghijklmnop".into());
        let c = endpoint_context(&e);
        assert!(!c.contains("sk-ant-0123456789"), "{c}");
    }

    #[test]
    fn an_entry_without_a_secret_says_so_rather_than_printing_nothing() {
        let mut e = entry();
        e.secret = None;
        let c = endpoint_context(&e);
        assert!(c.contains("no secret ref"), "{c}");
    }

    fn named() -> EndpointNamed {
        EndpointNamed {
            inner: Arc::new(NullClient),
            context: "[auth] CONTEXT".into(),
        }
    }

    struct NullClient;
    #[async_trait::async_trait]
    impl LlmClient for NullClient {
        async fn generate(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> Result<crate::llm::LlmResponse, LlmError> {
            unreachable!("not called by these tests")
        }
        fn model_name(&self) -> &str {
            "null"
        }
    }

    #[test]
    fn an_auth_failure_gains_the_endpoint() {
        let out = named().name_endpoint(LlmError::Auth(401, "API key is invalid".into()));
        let LlmError::Auth(status, body) = out else {
            panic!("class must not change")
        };
        assert_eq!(status, 401);
        assert!(body.contains("API key is invalid"), "{body}");
        assert!(body.contains("[auth] CONTEXT"), "{body}");
    }

    /// Everything else is left alone. A rate limit or a retired model already
    /// names its own cause, and a candidate that the fallback chain will route
    /// around must not carry a paragraph about credentials.
    #[test]
    fn every_other_class_is_left_untouched() {
        let n = named();
        assert!(matches!(
            n.name_endpoint(LlmError::RateLimit),
            LlmError::RateLimit
        ));
        let out = n.name_endpoint(LlmError::Rejected(413, "too large".into()));
        let LlmError::Rejected(_, body) = out else {
            panic!("class must not change")
        };
        assert_eq!(body, "too large", "must not be decorated");
    }
}
