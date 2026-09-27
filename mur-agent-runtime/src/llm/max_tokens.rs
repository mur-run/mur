//! Per-model default output ceiling from `models.yaml` (`max_tokens:`).
//!
//! Every task-runner call leaves `LlmRequest::max_tokens` unset, so the
//! ceiling a model actually gets is whatever its provider client falls back
//! to — a constant for Anthropic, nothing at all for OpenAI-protocol clients.
//! That is too small for a thinking model at high effort (thinking and the
//! reply share the one budget) and it could not be changed without a rebuild.
//!
//! This wrapper fills the registry value into requests that did not choose
//! their own, so a call site with a real reason for a small cap (a one-line
//! translation, a short summary) keeps it. It sits in front of the provider
//! client rather than inside each one: one rule, five providers, and a new
//! provider inherits it without remembering to.

use std::sync::Arc;

use async_trait::async_trait;

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StreamDelta};

struct DefaultMaxTokens {
    inner: Arc<dyn LlmClient>,
    max_tokens: u32,
}

impl DefaultMaxTokens {
    fn fill(&self, mut req: LlmRequest) -> LlmRequest {
        req.max_tokens.get_or_insert(self.max_tokens);
        req
    }
}

#[async_trait]
impl LlmClient for DefaultMaxTokens {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.inner.generate(self.fill(req)).await
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        self.inner.generate_stream(self.fill(req), sink).await
    }
}

/// Wrap `inner` so requests without their own `max_tokens` get `configured`.
///
/// `None` returns `inner` untouched. `Some(0)` is treated as unset: no
/// provider accepts a zero ceiling (Anthropic answers 400), so honouring it
/// would turn a typo into an agent that fails every turn.
pub(crate) fn with_default_max_tokens(
    inner: Arc<dyn LlmClient>,
    configured: Option<u32>,
) -> Arc<dyn LlmClient> {
    match configured {
        Some(0) => {
            tracing::warn!(
                model = inner.model_name(),
                "models.yaml max_tokens: 0 ignored — the provider default applies"
            );
            inner
        }
        Some(max_tokens) => Arc::new(DefaultMaxTokens { inner, max_tokens }),
        None => inner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::StopReason;
    use std::sync::Mutex;

    /// Records the `max_tokens` each call arrived with.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<Option<u32>>>,
    }

    #[async_trait]
    impl LlmClient for Recorder {
        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.seen.lock().unwrap().push(req.max_tokens);
            Ok(LlmResponse {
                text: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                model: "recorder".into(),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
            })
        }

        fn model_name(&self) -> &str {
            "recorder"
        }
    }

    fn req(max_tokens: Option<u32>) -> LlmRequest {
        LlmRequest {
            max_tokens,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn fills_an_unset_request_on_both_paths() {
        let rec = Arc::new(Recorder::default());
        let client = with_default_max_tokens(rec.clone(), Some(128_000));
        client.generate(req(None)).await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        client.generate_stream(req(None), tx).await.unwrap();
        assert_eq!(*rec.seen.lock().unwrap(), vec![Some(128_000); 2]);
    }

    /// A call site that asked for a small cap on purpose keeps it: the
    /// registry value is a default, not a clamp in either direction.
    #[tokio::test]
    async fn an_explicit_request_value_wins() {
        let rec = Arc::new(Recorder::default());
        let client = with_default_max_tokens(rec.clone(), Some(128_000));
        client.generate(req(Some(400))).await.unwrap();
        assert_eq!(*rec.seen.lock().unwrap(), vec![Some(400)]);
    }

    #[tokio::test]
    async fn unset_and_zero_leave_the_request_alone() {
        for configured in [None, Some(0)] {
            let rec = Arc::new(Recorder::default());
            let client = with_default_max_tokens(rec.clone(), configured);
            client.generate(req(None)).await.unwrap();
            assert_eq!(*rec.seen.lock().unwrap(), vec![None], "{configured:?}");
        }
    }
}

/// End to end through the real builder and the real provider clients: what
/// matters is the JSON on the wire, not what the wrapper hands its inner.
#[cfg(test)]
mod wire_tests {
    use crate::llm::client_builder::build_client_from_entry;
    use crate::llm::{LlmRequest, RichMessage};
    use crate::profile::Profile;
    use mur_common::agent::AgentProfile;
    use mur_common::model::ModelEntry;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve one request, return its JSON body, answer 400 so the client
    /// gives up at once instead of parsing a reply this test does not build.
    async fn capture_body(listener: TcpListener) -> serde_json::Value {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = s.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before a full request arrived");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(split) = text.find("\r\n\r\n") {
                let len = text[..split]
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if buf.len() >= split + 4 + len {
                    let _ = s
                        .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                        .await;
                    return serde_json::from_slice(&buf[split + 4..split + 4 + len]).unwrap();
                }
            }
        }
    }

    async fn sent_max_tokens(provider: &str, path: &str, configured: Option<u32>) -> Option<u64> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let entry = ModelEntry {
            provider: provider.into(),
            model: "wire-test-model".into(),
            base_url: Some(format!("http://{addr}{path}")),
            max_tokens: configured,
            ..Default::default()
        };
        let profile = Profile {
            inner: AgentProfile::default_for_tests(),
            agent_home: std::path::PathBuf::from("/tmp/does-not-need-to-exist"),
            digest: String::new(),
            raw_yaml: String::new(),
            system_prompt: None,
        };
        let client = build_client_from_entry(&entry, &profile, std::path::Path::new("/tmp"))
            .expect("loopback subscription entry builds");
        let server = tokio::spawn(capture_body(listener));
        let _ = client
            .generate(LlmRequest {
                messages: vec![RichMessage::Text {
                    role: "user".into(),
                    content: "hi".into(),
                }],
                ..Default::default()
            })
            .await;
        let body = server.await.unwrap();
        body.get("max_tokens").and_then(|v| v.as_u64())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn claude_sends_the_registry_value_instead_of_the_builtin_default() {
        assert_eq!(
            sent_max_tokens("claude", "/v1", Some(128_000)).await,
            Some(128_000)
        );
        // Unset still falls back to the Anthropic client's own constant,
        // because that API rejects a request without the field.
        assert_eq!(sent_max_tokens("claude", "/v1", None).await, Some(32_768));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn codex_sends_a_ceiling_only_when_the_registry_names_one() {
        assert_eq!(
            sent_max_tokens("codex", "/codex/v1", Some(128_000)).await,
            Some(128_000)
        );
        assert_eq!(sent_max_tokens("codex", "/codex/v1", None).await, None);
    }
}
