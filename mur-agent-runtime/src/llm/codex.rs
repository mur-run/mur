//! ChatGPT-subscription provider (`provider: codex`).
//!
//! Authless OpenAI Chat Completions traffic to the loopback
//! `mur-model-gateway`, which owns the Codex OAuth token, refreshes it, and
//! translates to the Responses API. The runtime never sees a credential —
//! there is no `secret`, and no key is read from the environment or the
//! keychain — so the only thing this module has to get right is *where* the
//! traffic may go: the loopback restriction is the safety property. A remote
//! host here would either be an authless request to a stranger or, worse, a
//! route that silently lands on OpenAI Platform billing.

use super::openai::OpenAiClient;
use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StreamDelta};
use async_trait::async_trait;
use mur_common::model::ModelEntry;

/// The one path the gateway serves ChatGPT-subscription traffic on.
pub const CODEX_ROUTE_PATH: &str = "/codex/v1";

/// Accept only `http://<localhost|loopback-ip>:<port>/codex/v1` — explicit
/// port, no userinfo, no query or fragment. Everything else is an error that
/// names the offending part.
pub fn validate_codex_base_url(raw: &str) -> Result<reqwest::Url, LlmError> {
    super::loopback::validate_loopback_base_url(raw, CODEX_ROUTE_PATH)
}

pub struct CodexClient {
    inner: OpenAiClient,
}

impl CodexClient {
    pub fn with_http_client(
        base_url: String,
        model: String,
        http: reqwest::Client,
    ) -> Result<Self, LlmError> {
        let url = validate_codex_base_url(&base_url)?;
        Ok(Self {
            inner: OpenAiClient::authless_with_http(url.to_string(), model, http),
        })
    }

    /// Registry entry → client. Rejects a `secret` outright rather than
    /// ignoring it: a key on a codex entry means someone expects it to be
    /// sent, and this route never sends one.
    pub(crate) fn from_entry(entry: &ModelEntry, http: reqwest::Client) -> Result<Self, LlmError> {
        if entry.secret.is_some() {
            return Err(LlmError::Http(
                "codex entries take no secret: the loopback gateway holds the ChatGPT login".into(),
            ));
        }
        let base = entry.base_url.as_deref().ok_or_else(|| {
            LlmError::Http("codex entry needs base_url (http://127.0.0.1:<port>/codex/v1)".into())
        })?;
        Self::with_http_client(base.to_string(), entry.model.clone(), http)
    }
}

#[async_trait]
impl LlmClient for CodexClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.inner.generate(req).await
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        self.inner.generate_stream(req, sink).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::secret::SecretRef;

    #[test]
    fn accepts_only_loopback_codex_base_urls() {
        for ok in [
            "http://127.0.0.1:8088/codex/v1",
            "http://localhost:8088/codex/v1",
            "http://[::1]:8088/codex/v1",
            "http://127.0.0.1:8088/codex/v1/",
        ] {
            assert!(validate_codex_base_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://api.openai.com/v1",
            "https://127.0.0.1:8088/codex/v1",
            "http://127.0.0.1:8088/v1",
            "http://127.0.0.1/codex/v1",
            "http://localhost.evil.test:8088/codex/v1",
            "http://user@127.0.0.1:8088/codex/v1",
            "http://192.168.1.2:8088/codex/v1",
            "http://127.0.0.1:8088/codex/v1?x=1",
            "http://127.0.0.1:8088/codex/v1#f",
            "not a url",
        ] {
            assert!(validate_codex_base_url(bad).is_err(), "{bad}");
        }
    }

    fn entry(base_url: Option<&str>, secret: Option<SecretRef>) -> ModelEntry {
        ModelEntry {
            provider: "codex".into(),
            model: "gpt-5.6-sol".into(),
            base_url: base_url.map(Into::into),
            secret,
            ..Default::default()
        }
    }

    #[test]
    fn factory_builds_only_secret_free_loopback_entries() {
        let http = reqwest::Client::new();
        let ok = CodexClient::from_entry(
            &entry(Some("http://127.0.0.1:8088/codex/v1"), None),
            http.clone(),
        )
        .unwrap();
        assert_eq!(ok.model_name(), "gpt-5.6-sol");

        let missing_url = CodexClient::from_entry(&entry(None, None), http.clone());
        assert!(missing_url.err().unwrap().to_string().contains("base_url"));

        let with_secret = CodexClient::from_entry(
            &entry(
                Some("http://127.0.0.1:8088/codex/v1"),
                Some(SecretRef::Env("OPENAI_API_KEY".into())),
            ),
            http.clone(),
        );
        assert!(with_secret.err().unwrap().to_string().contains("no secret"));

        let remote = CodexClient::from_entry(&entry(Some("https://api.openai.com/v1"), None), http);
        assert!(remote.err().unwrap().to_string().contains("rejected"));
    }
}
