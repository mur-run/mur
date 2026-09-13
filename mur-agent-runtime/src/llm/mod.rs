//! LLM client abstraction.

use async_trait::async_trait;
use mur_common::{AgentProfile, LlmMode};

pub mod anthropic;
pub mod claude;
pub(crate) mod client_builder;
pub mod codex;
pub mod fallback;
pub mod loopback;
pub mod ollama;
pub mod openai;
pub mod stub;
pub mod switchable;

/// Shared reqwest builder for the agent's LLM clients. Built with `.no_proxy()`
/// so an LLM client NEVER inherits an ambient `HTTP_PROXY`/`HTTPS_PROXY` — its
/// destination is its `base_url` alone. This is the isolation guarantee that
/// keeps the per-MCP-server egress proxy (and a user's debug cc-proxy, which is
/// configured via base_url) from ever capturing the agent's own LLM traffic.
/// See `docs/superpowers/plans/2026-06-26-mcp-per-server-egress.md`.
/// Time allowed to establish a TCP connection to an LLM endpoint.
///
/// The only clock on this client. `read_timeout` is deliberately absent:
/// reqwest polls that timer while the response head is still outstanding
/// (`async_impl/client.rs`, `PendingRequest::poll`), so any value at all
/// becomes a hard ceiling on how long a model may think before its first
/// byte — the thing the execution-limits redesign exists to remove. A stream
/// that stops sending is bounded by [`StreamActivity`] instead, where the
/// difference between "thinking" and "dead" is actually observable, and a
/// non-stream call is bounded by its turn's deadline and by cancellation.
/// See `docs/superpowers/specs/2026-09-13-llm-idle-not-total-design.md` D2/D5.
pub(crate) const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;

pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(std::time::Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
}

/// How long a stream may take to send its FIRST chunk.
///
/// This covers a cold local model: loading weights off disk and evaluating a
/// long prompt before emitting anything is a minutes-long operation, and it
/// looks identical to a hung endpoint from out here. Generous on purpose.
pub(crate) const LLM_FIRST_CHUNK_TIMEOUT_SECS: u64 = 300;

/// How long a stream that has already sent something may go quiet.
///
/// Tight on purpose: once a model is emitting, the gap between chunks is
/// sub-second, so two minutes of silence is a dead connection rather than a
/// slow one. Two separate bounds because one number cannot serve both ends —
/// it would have to be at least the cold-start figure, which makes a dead
/// mid-stream hang for five minutes and stops the bound meaning anything.
pub(crate) const LLM_STREAM_IDLE_TIMEOUT_SECS: u64 = 120;

/// Bounds how long a stream may be SILENT — never how long it may run.
///
/// This is the whole of #1287's answer. There is no ceiling on generation
/// anywhere: a body that is still arriving is live work. What is bounded is
/// the gap between arrivals, measured here rather than at the sink, because
/// tool-call argument fragments reach the sink as nothing at all — OpenAI
/// accumulates them into a partial map and Anthropic's `input_json_delta` arm
/// returns `None`. A guard watching the sink would see a model streaming a
/// large tool invocation as idle and abort it mid-call. Arrival of bytes is
/// the liveness signal, whatever those bytes turn out to mean.
pub(crate) struct StreamActivity {
    first: std::time::Duration,
    idle: std::time::Duration,
    seen_any: bool,
}

/// Read a seconds value from the environment, keeping `default` when it is
/// absent, unparseable, or zero.
///
/// Zero is rejected rather than honoured as "no bound": a bound that can be
/// switched off by a stray `=0` is a bound nobody can rely on, and the caller
/// asking for it is far more likely to have made a mistake than to want an
/// unbounded stream.
fn env_secs(var: &str, default: u64) -> std::time::Duration {
    match std::env::var(var) {
        Err(_) => std::time::Duration::from_secs(default),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => std::time::Duration::from_secs(n),
            _ => {
                tracing::warn!(
                    var,
                    value = %raw,
                    default_secs = default,
                    "unusable value for an LLM stream idle bound; keeping the default \
                     (a typo must not disable the bound)"
                );
                std::time::Duration::from_secs(default)
            }
        },
    }
}

impl StreamActivity {
    pub(crate) fn from_env() -> Self {
        Self {
            first: env_secs(
                "MUR_LLM_FIRST_DELTA_TIMEOUT_SECS",
                LLM_FIRST_CHUNK_TIMEOUT_SECS,
            ),
            idle: env_secs("MUR_LLM_IDLE_TIMEOUT_SECS", LLM_STREAM_IDLE_TIMEOUT_SECS),
            seen_any: false,
        }
    }

    /// Await one chunk-producing future under the applicable bound.
    ///
    /// `Ok(None)` is end-of-stream and `Err(LlmError::Timeout)` is the bound
    /// elapsing; any other `Err` passes through untouched, because
    /// [`classify`] treats a transport failure and a timeout differently and
    /// relabelling one as the other would change how the fallback chain
    /// responds.
    ///
    /// Generic over the item: `Response::chunk` yields `bytes::Bytes`, `bytes`
    /// is not a direct dependency of this crate and reqwest does not re-export
    /// it, so the element type is left to inference at the three call sites.
    /// It also means the timing rule can be tested with a plain sleep instead
    /// of a socket.
    /// Fixed bounds for the timing tests, so they do not have to wait out the
    /// production five-minute cold-start figure even on a paused clock.
    #[cfg(test)]
    pub(crate) fn for_test(first_secs: u64, idle_secs: u64) -> Self {
        Self {
            first: std::time::Duration::from_secs(first_secs),
            idle: std::time::Duration::from_secs(idle_secs),
            seen_any: false,
        }
    }

    pub(crate) async fn bounded<T>(
        &mut self,
        fut: impl std::future::Future<Output = Result<Option<T>, LlmError>>,
    ) -> Result<Option<T>, LlmError> {
        let bound = if self.seen_any { self.idle } else { self.first };
        match tokio::time::timeout(bound, fut).await {
            Err(_) => Err(LlmError::Timeout),
            Ok(Err(e)) => Err(e),
            Ok(Ok(None)) => Ok(None),
            Ok(Ok(Some(v))) => {
                // Recorded HERE, before any caller looks at the bytes. That is
                // what makes an invisible SSE frame — a tool-argument
                // fragment, a usage update — count as life.
                self.seen_any = true;
                Ok(Some(v))
            }
        }
    }
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
    /// Images this tool call produced. Rendered as real `image` blocks inside
    /// the provider's `tool_result` where the provider supports it, so an
    /// agent can look at what its own tool fetched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::tools::ToolImage>,
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
    /// verdict, so unlike `Http` this retries and then advances: switching
    /// models can't mask an auth/config error the server never reported.
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
    /// The endpoint does not serve this model id (HTTP 404). Distinct from
    /// `Http` because the two need opposite handling: a 404 is permanent for
    /// this candidate — retrying it with backoff can only waste the retry
    /// budget — but it is exactly what the fallback chain exists for, so the
    /// chain must advance immediately. Providers retire and rename ids
    /// continuously; lumping this in with auth failures made a renamed model
    /// kill the turn outright while a perfectly good fallback sat unused.
    #[error("model not found: {0}")]
    ModelNotFound(String),
    /// Authentication or authorization refused (401/403). The one class that
    /// must never advance the chain: the operator configured something wrong,
    /// and routing around it converts a loud, fixable failure into a silent
    /// permanent one.
    #[error("auth refused ({0}): {1}")]
    Auth(u16, String),
    /// This endpoint refused this request, for a reason that is specific to
    /// this candidate rather than to the request itself — payload too large
    /// for its window, unsupported modality, region or tier restriction, or a
    /// provider-specific 4xx we have not enumerated. Another candidate may
    /// accept it.
    #[error("rejected ({0}): {1}")]
    Rejected(u16, String),
    /// Every candidate failed. `source` carries the most *actionable* of their
    /// errors — a configuration error the operator can fix outranks weather
    /// they cannot — so classification upstream stays correct, while `summary`
    /// lists what each candidate actually said. Reporting only the last
    /// candidate's error, which is what this replaces, made the diagnosis an
    /// accident of chain order.
    #[error("{summary}")]
    AllCandidatesFailed {
        source: Box<LlmError>,
        summary: String,
    },
}

impl LlmError {
    /// Map a non-success HTTP status into a typed error.
    ///
    /// The default for an unrecognised 4xx is `Rejected` (advance), not `Http`
    /// (stop. The set of failures that must NOT fall back is small, closed and
    /// stable across providers — it is authentication, and nothing else. The
    /// set that *should* fall back is open and still growing: every provider
    /// invents its own codes for "too big", "wrong media type", "not enabled
    /// in your region", "model not in your tier". Enumerating the open set and
    /// defaulting the tail to stop is what let a renamed model kill turns for
    /// months while a fallback sat unused.
    ///
    /// Enumerate the closed set; default to the open one.
    pub fn from_status(status: u16, body: String) -> LlmError {
        match status {
            // Closed set: never fall back. Presenting the same broken
            // credential to a second provider fails identically and buries a
            // configuration error only the operator can fix.
            401 | 403 => LlmError::Auth(status, body),
            429 => LlmError::RateLimit,
            402 => LlmError::InsufficientCredit,
            404 => LlmError::ModelNotFound(body),
            408 => LlmError::Timeout,
            500..=599 => LlmError::ServerError(status),
            // Open set: this endpoint refused this request. Another candidate
            // — different context window, different modality support,
            // different region — may well accept it.
            400..=499 => LlmError::Rejected(status, body),
            _ => LlmError::Http(format!("status {status}: {body}")),
        }
    }

    /// Map a reqwest transport error into a typed error. Central rule: an
    /// error without an HTTP status is a transport failure (`Connect`,
    /// retry-then-advance) — the server never rendered a verdict, so it can't
    /// be the auth/bad-request class. Request-builder errors (malformed
    /// URL/body) stay `Http`: that is our own bug, and no other candidate will
    /// like the request any better.
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

/// What the fallback loop should do with a failed call.
///
/// Named for the action, not the error: the previous name (`Retryability`,
/// `Retryable`) said "retry" while the variant actually meant "retry this
/// candidate `max_retries` times, cool it down, then advance" — two decisions
/// behind one word. The third state was the one missing: a failure that is
/// permanent for this candidate but may well succeed on the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Transient. Retry this candidate with backoff; advance once the budget
    /// is spent.
    RetryThenAdvance,
    /// Permanent for this candidate, plausibly fine on another. Advance
    /// immediately — retrying cannot change the answer, and the backoff
    /// sleeps are pure latency.
    AdvanceNow,
    /// Continuing is pointless or actively harmful. Auth failures live here:
    /// falling back would re-present the same broken credential to a second
    /// provider and bury a configuration error the operator has to fix.
    Stop,
}

pub fn classify(e: &LlmError) -> Disposition {
    match e {
        LlmError::RateLimit
        | LlmError::Timeout
        | LlmError::Connect(_)
        | LlmError::ServerError(_) => Disposition::RetryThenAdvance,
        // None of these gets better by asking the same endpoint again: an
        // account without credit does not acquire any within three backoffs, a
        // model id the endpoint does not serve will not appear, and a refusal
        // aimed at this candidate's limits is not a matter of timing. All three
        // may be fine on the next candidate.
        LlmError::InsufficientCredit | LlmError::ModelNotFound(_) | LlmError::Rejected(..) => {
            Disposition::AdvanceNow
        }
        // `Auth` is the closed set that must never fall back. `Http` here means
        // a malformed request we built or a response we could not parse — our
        // bug, which no other candidate will like any better.
        LlmError::Auth(..) | LlmError::Http(_) | LlmError::InvalidResponse(_) => Disposition::Stop,
        // Already exhausted; classify as whatever the operator should act on.
        LlmError::AllCandidatesFailed { source, .. } => classify(source),
    }
}

/// How much the operator can do about a failure. Used to pick which candidate's
/// error leads when the whole chain is exhausted: a wrong API key is worth
/// surfacing over a rate limit, whatever order they happened to occur in.
fn actionability(e: &LlmError) -> u8 {
    match classify(e) {
        Disposition::Stop => 2,             // config error / our bug — fix it
        Disposition::AdvanceNow => 1,       // candidate-specific — maybe fix it
        Disposition::RetryThenAdvance => 0, // weather — wait it out
    }
}

/// Fold every candidate's failure into one error: the most actionable one
/// leads (so upstream classification is right), and the summary says what each
/// candidate actually reported.
pub fn all_candidates_failed(failures: Vec<(String, LlmError)>) -> LlmError {
    let Some(lead) = failures
        .iter()
        .max_by_key(|(_, e)| actionability(e))
        .map(|(_, e)| e.clone())
    else {
        return LlmError::InvalidResponse("no model candidates".into());
    };
    let listed = failures
        .iter()
        .map(|(r, e)| format!("{r}: {e}"))
        .collect::<Vec<_>>()
        .join("; ");
    LlmError::AllCandidatesFailed {
        source: Box::new(lead),
        summary: format!("all {} model candidates failed — {listed}", failures.len()),
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
        // 400 is no longer the `Http` catch-all: an unrecognised 4xx is a
        // refusal by THIS endpoint and may well succeed on the next candidate.
        assert!(matches!(
            LlmError::from_status(400, "x".into()),
            LlmError::Rejected(400, _)
        ));
        assert!(matches!(
            LlmError::from_status(401, "x".into()),
            LlmError::Auth(401, _)
        ));
    }

    #[test]
    fn transient_failures_retry_then_advance() {
        use Disposition::*;
        assert!(matches!(classify(&LlmError::RateLimit), RetryThenAdvance));
        assert!(matches!(classify(&LlmError::Timeout), RetryThenAdvance));
        assert!(matches!(
            classify(&LlmError::ServerError(500)),
            RetryThenAdvance
        ));
        assert!(matches!(
            classify(&LlmError::Connect("connection refused".into())),
            RetryThenAdvance
        ));
    }

    /// A failure that is permanent for THIS candidate but not for the chain.
    /// Retrying either of these against the same endpoint cannot change the
    /// answer — an account does not acquire credit inside three backoffs, and
    /// a model id the endpoint does not serve will not materialise — so they
    /// must advance without spending the retry budget.
    #[test]
    fn permanent_for_this_candidate_advances_without_retrying() {
        use Disposition::*;
        assert!(matches!(
            classify(&LlmError::ModelNotFound("no such model".into())),
            AdvanceNow
        ));
        assert!(matches!(
            classify(&LlmError::InsufficientCredit),
            AdvanceNow
        ));
    }

    /// Auth must never fall back. Presenting the same broken credential to a
    /// second provider fails identically and buries a config error the operator
    /// has to fix — and could spend money under a misconfiguration nobody asked
    /// for.
    #[test]
    fn auth_and_malformed_stop_the_chain() {
        use Disposition::*;
        assert!(matches!(
            classify(&LlmError::Http("status 401: unauthorized".into())),
            Stop
        ));
        assert!(matches!(classify(&LlmError::Http("400".into())), Stop));
        assert!(matches!(
            classify(&LlmError::InvalidResponse("x".into())),
            Stop
        ));
    }

    /// 404 must not land in the `Http` catch-all, which is `Stop`. This is the
    /// status a provider returns for a renamed or retired model id, and it is
    /// precisely what a fallback chain exists to survive.
    #[test]
    fn a_404_becomes_model_not_found_not_a_generic_http_error() {
        let e = LlmError::from_status(404, "model claude-sonnet-4-6 not found".into());
        assert!(matches!(e, LlmError::ModelNotFound(_)), "{e:?}");
        assert_eq!(classify(&e), Disposition::AdvanceNow);
        // 401 shares the catch-all and must keep stopping.
        assert_eq!(
            classify(&LlmError::from_status(401, "unauthorized".into())),
            Disposition::Stop
        );
    }

    #[test]
    fn from_status_maps_408_to_timeout() {
        assert!(matches!(
            LlmError::from_status(408, String::new()),
            LlmError::Timeout
        ));
        // Auth is the one closed set that must never advance the chain.
        for status in [401, 403] {
            let e = LlmError::from_status(status, "unauthorized".into());
            assert!(matches!(e, LlmError::Auth(..)), "{status}: {e:?}");
            assert_eq!(classify(&e), Disposition::Stop, "{status}");
        }
    }
}

#[cfg(test)]
mod builder_clock_tests {
    use super::*;

    /// F5's regression guard. A `read_timeout` or a total `.timeout()` on this
    /// client is a hard ceiling on how long a model may think — reqwest polls
    /// the read timer while the response head is still outstanding, so before
    /// the first byte it cannot tell "thinking" from "hung". Either one
    /// reintroduces exactly the bug #1287 is about.
    ///
    /// Asserted through `Debug`, which prints `read_timeout` and the total
    /// timeout when they are set (`async_impl/client.rs`, `Config::fmt_fields`).
    ///
    /// Honest limitation: that same `Debug` does **not** print
    /// `connect_timeout`, so this test cannot prove the connect clock is
    /// applied — only that the two forbidden ones are absent. Proving the
    /// connect clock behaviourally needs a TCP connect that stalls rather than
    /// refuses, which means an unroutable address and a flaky test.
    #[test]
    fn the_client_carries_neither_a_total_nor_a_read_timeout() {
        let printed = format!("{:?}", llm_client_builder().build().unwrap());
        assert!(
            !printed.contains("read_timeout"),
            "a read_timeout bounds server think time before the first byte: {printed}"
        );
        assert!(
            !printed.contains("timeout: Some"),
            "a total timeout kills a live streamed response: {printed}"
        );
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

/// httpmock 0.7 recycles a small pool of servers behind one shared runtime.
/// Several `#[tokio::test]`s driving their own mock server at once make each
/// other's connections fail (`Connect`, refused — not a mock mismatch), so
/// every test that starts a `MockServer` holds this for its whole body.
/// Serial they all pass; this is the cheapest way to keep them that way
/// without `--test-threads=1` for the whole crate.
#[cfg(test)]
pub(crate) static MOCK_SERVER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod stream_activity_tests {
    use super::*;

    /// Helper: a future that "arrives" after `secs` with one item.
    async fn arrives_after(secs: u64) -> Result<Option<&'static str>, LlmError> {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        Ok(Some("chunk"))
    }

    /// The two bounds are different bounds, and which one applies depends on
    /// whether anything has arrived yet. A single number could not express
    /// this: 90 s of silence is fine before the first byte and fatal after it.
    #[tokio::test(start_paused = true)]
    async fn the_first_bound_covers_cold_start_and_the_idle_bound_takes_over_after() {
        let mut a = StreamActivity::for_test(100, 10);
        // 50 s before anything has arrived: inside the FIRST bound.
        assert!(matches!(
            a.bounded(arrives_after(50)).await,
            Ok(Some("chunk"))
        ));
        // The same 50 s now that something has arrived: past the IDLE bound.
        assert!(matches!(
            a.bounded(arrives_after(50)).await,
            Err(LlmError::Timeout)
        ));
    }

    /// Nothing at all arrived, so nothing reached the sink and a retry cannot
    /// duplicate anything. `Timeout` is `RetryThenAdvance` in `classify`,
    /// which is exactly the disposition this case wants.
    #[tokio::test(start_paused = true)]
    async fn a_stream_that_never_starts_is_a_timeout() {
        let mut a = StreamActivity::for_test(10, 5);
        assert!(matches!(
            a.bounded(arrives_after(999)).await,
            Err(LlmError::Timeout)
        ));
        assert_eq!(classify(&LlmError::Timeout), Disposition::RetryThenAdvance);
    }

    /// End-of-stream must not be reported as a timeout. This is the difference
    /// between "the model finished" and "the model stopped sending".
    #[tokio::test(start_paused = true)]
    async fn end_of_stream_passes_through_as_none() {
        let mut a = StreamActivity::for_test(10, 5);
        let end: Result<Option<&str>, LlmError> = Ok(None);
        assert!(matches!(a.bounded(async { end }).await, Ok(None)));
    }

    /// A transport failure keeps its own class. `classify` sends `Connect` and
    /// `Timeout` down different paths, and mislabelling a refused connection
    /// as a timeout would change how the fallback chain responds to it.
    #[tokio::test(start_paused = true)]
    async fn an_inner_error_is_not_relabelled_a_timeout() {
        let mut a = StreamActivity::for_test(10, 5);
        let inner: Result<Option<&str>, LlmError> = Err(LlmError::Connect("refused".into()));
        assert!(matches!(
            a.bounded(async { inner }).await,
            Err(LlmError::Connect(_))
        ));
    }

    /// Arrival is the liveness signal, not what the bytes mean. The caller
    /// never gets a chance to say "that frame was only a tool-argument
    /// fragment, ignore it" — which is the bug a sink-side guard would have.
    #[tokio::test(start_paused = true)]
    async fn any_item_counts_as_activity_whatever_it_contains() {
        let mut a = StreamActivity::for_test(100, 10);
        // An empty chunk is still bytes off the socket.
        let empty: Result<Option<&str>, LlmError> = Ok(Some(""));
        assert!(matches!(a.bounded(async { empty }).await, Ok(Some(""))));
        assert!(a.seen_any, "an empty chunk must still count as life");
    }

    #[test]
    fn env_overrides_are_honoured() {
        // SAFETY: set and cleared within this test; nextest gives each test
        // its own process, so no sibling sees this.
        unsafe { std::env::set_var("MUR_LLM_IDLE_TIMEOUT_SECS", "7") };
        assert_eq!(
            env_secs("MUR_LLM_IDLE_TIMEOUT_SECS", 120),
            std::time::Duration::from_secs(7)
        );
        unsafe { std::env::remove_var("MUR_LLM_IDLE_TIMEOUT_SECS") };
    }

    /// A typo, or a zero, must not disable the bound. Zero is the dangerous
    /// one: read as "no bound" it would silently remove the protection.
    #[test]
    fn an_unusable_env_value_keeps_the_default() {
        for bad in ["", "abc", "0", "-5", "12.5"] {
            unsafe { std::env::set_var("MUR_LLM_IDLE_TIMEOUT_SECS", bad) };
            assert_eq!(
                env_secs("MUR_LLM_IDLE_TIMEOUT_SECS", 120),
                std::time::Duration::from_secs(120),
                "value {bad:?} must fall back to the default"
            );
        }
        unsafe { std::env::remove_var("MUR_LLM_IDLE_TIMEOUT_SECS") };
    }
}
