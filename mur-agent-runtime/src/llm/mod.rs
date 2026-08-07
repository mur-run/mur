//! LLM client abstraction.

use async_trait::async_trait;
use mur_common::{AgentProfile, LlmMode};

pub mod anthropic;
pub(crate) mod client_builder;
pub mod fallback;
pub mod ollama;
pub mod openai;
pub mod stub;

/// Shared reqwest builder for the agent's LLM clients. Built with `.no_proxy()`
/// so an LLM client NEVER inherits an ambient `HTTP_PROXY`/`HTTPS_PROXY` — its
/// destination is its `base_url` alone. This is the isolation guarantee that
/// keeps the per-MCP-server egress proxy (and a user's debug cc-proxy, which is
/// configured via base_url) from ever capturing the agent's own LLM traffic.
/// See `docs/superpowers/plans/2026-06-26-mcp-per-server-egress.md`.
pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

/// Gate function that the supervisor calls before constructing any concrete
/// LLM client. Returns `Err` when `entitlements.llm.mode = off`, which
/// declares the agent a "bridge" — an LLM-less mur agent that relays chat
/// traffic to/from the A2A bus. Bridges have no model, no API key, and the
/// supervisor must not dial a provider on their behalf.
///
/// Default `mode = Allowed` (back-compat), so this is a no-op for every
/// existing agent profile.
///
/// See `mur-common::bridge::LlmEntitlement` and Track C1 task M-c1.0.
pub fn build_client(profile: &AgentProfile) -> anyhow::Result<()> {
    if profile.entitlements.llm.mode == LlmMode::Off {
        anyhow::bail!(
            "llm.mode = off — agent '{}' is a bridge and may not call an LLM",
            profile.name
        );
    }
    Ok(())
}

/// Legacy flat message type used by adapter internals (anthropic/openai/ollama).
/// Kept for backward compatibility while adapters are migrated to `RichMessage`.
#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResult {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

/// Visible marker appended to assistant text when the provider cut the
/// generation off at the output-token ceiling (Anthropic
/// `stop_reason == "max_tokens"`, OpenAI `finish_reason == "length"`, Ollama
/// `done_reason == "length"`). A truncated reply must never look complete —
/// users, delegating agents, and channel history all read this text, and a
/// silent mid-word cut is how issue #715's corrupted artifact happened.
pub const MAX_TOKENS_TRUNCATION_MARKER: &str = "\n\n[output truncated: max_tokens reached]";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResultEntry {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
    #[serde(default)]
    pub status: crate::tools::ToolStatus,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RichMessage {
    Text {
        role: String,
        content: String,
    },
    ToolUse {
        text: Option<String>,
        calls: Vec<ToolCallResult>,
    },
    ToolResults {
        results: Vec<ToolResultEntry>,
    },
    /// A user turn carrying an inline image (base64) plus its text caption —
    /// e.g. a screenshot pasted into `mur agent cli`. Rendered by the
    /// Anthropic and Ollama adapters; the OpenAI adapter still drops the
    /// image and keeps only the caption text.
    ImageText {
        role: String,
        /// e.g. "image/png" — passed straight through to the provider.
        media_type: String,
        /// Base64-encoded image bytes (no data: prefix).
        data: String,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    Scheduled,
    Companion,
    Maintenance,
}

/// Why this LLM call is being made. Interactive = user-facing (chat, A2A send,
/// fleet delegate); Background = runtime-initiated, nobody watching live —
/// eligible for Smart cheap-model routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestIntent {
    #[default]
    Interactive,
    Background(BackgroundKind),
}

#[derive(Debug, Clone, Default)]
pub struct LlmRequest {
    pub messages: Vec<RichMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Vec<ToolDef>,
    /// Routing context; defaults to Interactive (see RequestIntent).
    pub intent: RequestIntent,
    /// Force exactly this model_ref (user "re-run on smart model"); bypasses
    /// Smart/fallback candidate assembly. None = normal resolution.
    pub pin_model_ref: Option<String>,
    /// Owning task id, threaded for telemetry correlation. None outside tasks.
    pub task_id: Option<String>,
    /// How hard the model should work on THIS call. `None` leaves the field
    /// off, which is the API default (`high`) — not "no effort".
    ///
    /// Set it at the call site that knows what the call is for: a mechanical
    /// request (write a summary, emit a small structured plan) has no use for
    /// the depth an open-ended coding turn needs, and pays for it anyway when
    /// this is left unset. Narrowed to what the resolved model accepts by
    /// `mur_common::llm::supported_effort` at the client boundary.
    pub effort: Option<mur_common::llm::Effort>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub tool_calls: Vec<ToolCallResult>,
    pub stop_reason: StopReason,
}

impl LlmResponse {
    /// True when the provider stopped this generation because it hit the
    /// output-token ceiling — i.e. the text is truncated, not complete.
    pub fn truncated_by_max_tokens(&self) -> bool {
        self.stop_reason == StopReason::MaxTokens
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LlmError {
    #[error("http: {0}")]
    Http(String),
    /// Transport-level failure — the request never got an HTTP status back
    /// (connect refused, DNS, TLS, connection reset). The server rendered no
    /// verdict, so unlike `Http` this is Retryable: switching models can't
    /// mask an auth/config error the server never reported.
    #[error("connect: {0}")]
    Connect(String),
    #[error("rate limit")]
    RateLimit,
    #[error("timeout")]
    Timeout,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("server error: {0}")]
    ServerError(u16),
    #[error("insufficient credit")]
    InsufficientCredit,
}

impl LlmError {
    /// Map a non-success HTTP status into a typed error. Centralises what was
    /// previously scattered `status == 429` checks + a lumped `Http(String)`.
    pub fn from_status(status: u16, body: String) -> LlmError {
        match status {
            429 => LlmError::RateLimit,
            402 => LlmError::InsufficientCredit,
            408 => LlmError::Timeout,
            500..=599 => LlmError::ServerError(status),
            _ => LlmError::Http(format!("status {status}: {body}")),
        }
    }

    /// Map a reqwest transport error into a typed error. Central rule: an
    /// error without an HTTP status is a transport failure (`Connect`,
    /// Retryable) — the server never rendered a verdict, so it can't be the
    /// auth/bad-request class that `Http` reserves Fatal for. Request-builder
    /// errors (malformed URL/body) stay `Http`: retrying can't fix them.
    pub fn from_reqwest(e: &reqwest::Error) -> LlmError {
        if e.is_timeout() {
            LlmError::Timeout
        } else if e.is_builder() {
            LlmError::Http(e.to_string())
        } else {
            LlmError::Connect(e.to_string())
        }
    }
}

/// Whether a failed call should advance the fallback chain (Retryable) or
/// return immediately (Fatal — auth/bad-request/malformed, where switching
/// models would only hide the real problem).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    Retryable,
    Fatal,
}

pub fn classify(e: &LlmError) -> Retryability {
    match e {
        LlmError::RateLimit
        | LlmError::Timeout
        | LlmError::Connect(_)
        | LlmError::ServerError(_)
        | LlmError::InsufficientCredit => Retryability::Retryable,
        LlmError::Http(_) | LlmError::InvalidResponse(_) => Retryability::Fatal,
    }
}

/// One streamed chunk: either part of the model's hidden reasoning
/// (`thinking = true`, shown as a transient "thinking" indicator) or part of
/// the user-facing answer (`thinking = false`).
#[derive(Debug, Clone)]
pub struct StreamDelta {
    pub text: String,
    pub thinking: bool,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;
    fn model_name(&self) -> &str;

    /// Generate a reply, sending each chunk to `sink` as it arrives, and return
    /// the assembled response. The default implementation is non-streaming: it
    /// runs `generate` and emits the whole answer once, so providers without
    /// streaming still satisfy the contract.
    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        let resp = self.generate(req).await?;
        if !resp.text.is_empty() {
            let _ = sink
                .send(StreamDelta {
                    text: resp.text.clone(),
                    thinking: false,
                })
                .await;
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_message_text_roundtrip() {
        let m = RichMessage::Text {
            role: "user".into(),
            content: "hello".into(),
        };
        match m {
            RichMessage::Text { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content, "hello");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn llm_request_tools_defaults_empty() {
        let req = LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            temperature: None,
            max_tokens: None,
            tools: vec![],
            ..Default::default()
        };
        assert!(req.tools.is_empty());
    }

    #[test]
    fn llm_request_intent_defaults_interactive() {
        let r = LlmRequest::default();
        assert_eq!(r.intent, RequestIntent::Interactive);
        assert!(r.pin_model_ref.is_none());
        assert!(r.task_id.is_none());
    }

    #[test]
    fn llm_response_defaults() {
        let r = LlmResponse {
            text: "hello".into(),
            input_tokens: 5,
            output_tokens: 2,
            model: "claude-3".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        };
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn from_status_maps_http_codes() {
        assert!(matches!(
            LlmError::from_status(429, "x".into()),
            LlmError::RateLimit
        ));
        assert!(matches!(
            LlmError::from_status(402, "x".into()),
            LlmError::InsufficientCredit
        ));
        assert!(matches!(
            LlmError::from_status(503, "x".into()),
            LlmError::ServerError(503)
        ));
        assert!(matches!(
            LlmError::from_status(400, "x".into()),
            LlmError::Http(_)
        ));
        assert!(matches!(
            LlmError::from_status(401, "x".into()),
            LlmError::Http(_)
        ));
    }

    #[test]
    fn classify_retryable_vs_fatal() {
        use Retryability::*;
        assert!(matches!(classify(&LlmError::RateLimit), Retryable));
        assert!(matches!(classify(&LlmError::Timeout), Retryable));
        assert!(matches!(classify(&LlmError::ServerError(500)), Retryable));
        assert!(matches!(classify(&LlmError::InsufficientCredit), Retryable));
        assert!(matches!(
            classify(&LlmError::Connect("connection refused".into())),
            Retryable
        ));
        assert!(matches!(classify(&LlmError::Http("400".into())), Fatal));
        assert!(matches!(
            classify(&LlmError::InvalidResponse("x".into())),
            Fatal
        ));
    }

    #[test]
    fn from_status_maps_408_to_timeout() {
        assert!(matches!(
            LlmError::from_status(408, String::new()),
            LlmError::Timeout
        ));
        // 401 stays Http → Fatal: auth errors must never advance the chain.
        let e = LlmError::from_status(401, "unauthorized".into());
        assert!(matches!(e, LlmError::Http(_)));
        assert!(matches!(classify(&e), Retryability::Fatal));
    }
}

#[cfg(test)]
mod proxy_isolation_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// The cc-proxy guarantee: a client built via `llm_client_builder()` reaches
    /// its base_url DIRECTLY even when `HTTP_PROXY` points elsewhere — so the
    /// per-server egress proxy / a debug cc-proxy never captures LLM traffic.
    /// Without `.no_proxy()` this request would be routed to the dead proxy and
    /// fail, so the test guards that the builder keeps `.no_proxy()`.
    #[tokio::test]
    async fn llm_client_builder_ignores_ambient_http_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                // Drain the request so the client's send completes, then reply
                // with an explicit close + flush + graceful shutdown. Without
                // this, dropping the socket right after write_all races the OS
                // flush and Windows aborts the connection (os error 10053).
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = s.flush().await;
                let _ = s.shutdown().await;
            }
        });
        // SAFETY: set/cleared within this test; reqwest reads proxy env at build.
        unsafe {
            std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
        }
        let client = llm_client_builder().build().unwrap();
        let resp = client.get(format!("http://{addr}/")).send().await;
        unsafe {
            std::env::remove_var("HTTP_PROXY");
        }
        let resp = resp.expect("no_proxy client reaches base_url despite HTTP_PROXY");
        assert_eq!(resp.status(), 200);
    }
}
