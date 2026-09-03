//! Claude-subscription provider (`provider: claude`).
//!
//! Authless Anthropic Messages traffic to the loopback `mur-model-gateway`,
//! which holds the Claude Code OAuth token and attaches it. The runtime
//! never sees a credential — there is no `secret`, and neither
//! `ANTHROPIC_API_KEY` nor the agent keychain is consulted — so the only
//! thing this module has to get right is *where* the traffic may go. That
//! is what separates it from `provider: anthropic` pointed at the same
//! port: one `base_url` edit there lands on API billing; here it is refused
//! at startup.

use super::anthropic::AnthropicClient;
use super::loopback::validate_loopback_base_url;
use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StreamDelta};
use async_trait::async_trait;
use mur_common::model::ModelEntry;

/// The gateway's Anthropic route. `/v1/messages` is appended by the client.
pub const CLAUDE_ROUTE_PATH: &str = "/v1";

pub struct ClaudeClient {
    inner: AnthropicClient,
}

impl ClaudeClient {
    pub fn with_http_client(
        base_url: String,
        model: String,
        http: reqwest::Client,
    ) -> Result<Self, LlmError> {
        let url = validate_loopback_base_url(&base_url, CLAUDE_ROUTE_PATH)?;
        // `AnthropicClient` appends `/v1/messages` itself, so hand it the
        // origin, not the validated `/v1` path.
        let origin = url[..url::Position::BeforePath].to_string();
        Ok(Self {
            inner: AnthropicClient::authless_with_http(origin, model, http),
        })
    }

    /// Registry entry → client. Rejects a `secret` outright rather than
    /// ignoring it: a key on a claude entry means someone expects it to be
    /// sent, and this route never sends one.
    pub(crate) fn from_entry(entry: &ModelEntry, http: reqwest::Client) -> Result<Self, LlmError> {
        if entry.secret.is_some() {
            return Err(LlmError::Http(
                "claude entries take no secret: the loopback gateway holds the Claude Code login"
                    .into(),
            ));
        }
        let base = entry.base_url.as_deref().ok_or_else(|| {
            LlmError::Http("claude entry needs base_url (http://127.0.0.1:<port>/v1)".into())
        })?;
        Self::with_http_client(base.to_string(), entry.model.clone(), http)
    }
}

#[async_trait]
impl LlmClient for ClaudeClient {
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
    fn accepts_only_loopback_v1_base_urls() {
        for ok in [
            "http://127.0.0.1:8088/v1",
            "http://localhost:8088/v1",
            "http://[::1]:8088/v1",
            "http://127.0.0.1:8088/v1/",
        ] {
            assert!(
                validate_loopback_base_url(ok, CLAUDE_ROUTE_PATH).is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "https://api.anthropic.com/v1",
            "https://127.0.0.1:8088/v1",
            "http://127.0.0.1:8088",
            "http://127.0.0.1:8088/codex/v1",
            "http://127.0.0.1/v1",
            "http://localhost.evil.test:8088/v1",
            "http://user@127.0.0.1:8088/v1",
            "http://192.168.1.2:8088/v1",
            "http://127.0.0.1:8088/v1?x=1",
            "not a url",
        ] {
            assert!(
                validate_loopback_base_url(bad, CLAUDE_ROUTE_PATH).is_err(),
                "{bad}"
            );
        }
    }

    fn entry(base_url: Option<&str>, secret: Option<SecretRef>) -> ModelEntry {
        ModelEntry {
            provider: "claude".into(),
            model: "claude-opus-5".into(),
            base_url: base_url.map(Into::into),
            secret,
            ..Default::default()
        }
    }

    #[test]
    fn factory_builds_only_secret_free_loopback_entries() {
        let http = reqwest::Client::new();
        let ok =
            ClaudeClient::from_entry(&entry(Some("http://127.0.0.1:8088/v1"), None), http.clone())
                .unwrap();
        assert_eq!(ok.model_name(), "claude-opus-5");

        let missing_url = ClaudeClient::from_entry(&entry(None, None), http.clone());
        assert!(missing_url.err().unwrap().to_string().contains("base_url"));

        let with_secret = ClaudeClient::from_entry(
            &entry(
                Some("http://127.0.0.1:8088/v1"),
                Some(SecretRef::Env("ANTHROPIC_API_KEY".into())),
            ),
            http.clone(),
        );
        assert!(with_secret.err().unwrap().to_string().contains("no secret"));

        let remote =
            ClaudeClient::from_entry(&entry(Some("https://api.anthropic.com/v1"), None), http);
        assert!(remote.err().unwrap().to_string().contains("rejected"));
    }

    /// The gateway route is `/v1`; the client appends `/v1/messages` to its
    /// base, so the base handed down must be the origin alone or the request
    /// would go to `/v1/v1/messages`.
    #[tokio::test]
    async fn requests_land_on_the_gateway_v1_messages_route() {
        let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
        let server = httpmock::MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path("/v1/messages");
                then.status(200).json_body(serde_json::json!({
                    "content": [{"type": "text", "text": "hi"}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }));
            })
            .await;
        let client = ClaudeClient::with_http_client(
            format!("{}/v1", server.base_url()),
            "claude-opus-5".into(),
            reqwest::Client::new(),
        )
        .unwrap();
        let req = LlmRequest {
            messages: vec![super::super::RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            ..Default::default()
        };
        assert_eq!(client.generate(req).await.unwrap().text, "hi");
        m.assert_async().await;
    }
}
