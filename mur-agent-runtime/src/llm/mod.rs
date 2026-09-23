//! LLM client abstraction.

use async_trait::async_trait;
use mur_common::{AgentProfile, LlmMode};
use std::time::Duration;

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

/// Every LLM HTTP client comes from [`llm_client_builder`] — tests included.
///
/// `.no_proxy()` and the connect timeout live in that one function, so a
/// client built any other way silently has neither. In production that would
/// break the isolation guarantee documented above; in a test it did something
/// subtler and worse. Five test clients were built bare, which made them the
/// only clients in the crate that DID read an ambient `HTTP_PROXY` — so the
/// three tests that set one process-wide to prove it is ignored were poisoning
/// them, from another thread, in a way that looked like flakiness: the failing
/// set changed between runs and every one of them passed alone.
///
/// Serializing those tests would have ordered the collision. Building the
/// client production builds removes it, and makes the test exercise the real
/// thing at the same time. This guard is here because the next bare builder
/// would reopen it silently.
#[test]
fn llm_clients_are_never_built_bare() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/llm");
    let mut bare = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read llm dir") {
            let path = e.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read source");
            for (i, line) in body.lines().enumerate() {
                // The definition itself is the one permitted use.
                let is_definition = path.file_name().is_some_and(|f| f == "mod.rs")
                    && body
                        .lines()
                        .nth(i.saturating_sub(1))
                        .is_some_and(|prev| prev.contains("fn llm_client_builder"));
                if line.contains("reqwest::Client::builder()") && !is_definition {
                    bare.push(format!("{}:{}", path.display(), i + 1));
                }
            }
        }
    }
    assert!(
        bare.is_empty(),
        "build LLM clients with llm_client_builder(), not bare — \
         these inherit an ambient HTTP_PROXY and have no connect timeout: {bare:?}"
    );
}

mod stream_activity;
pub(crate) use stream_activity::StreamActivity;

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
    /// The stream stopped sending and the partial reply was kept.
    ///
    /// Distinct from `MaxTokens`, which is the provider deciding to stop at a
    /// ceiling it told us about. This one is the connection going quiet: no
    /// final frame arrived, so token usage is unknown and any tool call that
    /// was mid-arguments is gone. Truncated for every purpose `MaxTokens` is
    /// truncated for. See `StreamActivity` and spec
    /// docs/superpowers/specs/2026-09-13-llm-idle-not-total-design.md D6.
    Interrupted,
}

/// Visible marker appended to assistant text when the provider cut the
/// generation off at the output-token ceiling (Anthropic
/// `stop_reason == "max_tokens"`, OpenAI `finish_reason == "length"`, Ollama
/// `done_reason == "length"`). A truncated reply must never look complete —
/// users, delegating agents, and channel history all read this text, and a
/// silent mid-word cut is how issue #715's corrupted artifact happened.
pub const MAX_TOKENS_TRUNCATION_MARKER: &str = "\n\n[output truncated: max_tokens reached]";

/// Visible marker appended when a stream stopped sending and the partial reply
/// was kept ([`StopReason::Interrupted`]). Same rule as
/// [`MAX_TOKENS_TRUNCATION_MARKER`] and the same reason (#715): a truncated
/// reply must never look complete, because users, delegating agents and channel
/// history all read this text.
pub const STREAM_IDLE_TRUNCATION_MARKER: &str = "\n\n[output truncated: the model stopped sending]";

/// Visible marker appended when a later model call in the turn failed after
/// the user had already been shown text. The shown text is kept as the reply
/// (so memory and context threading carry it) and this says it was cut short.
/// Same rule as [`MAX_TOKENS_TRUNCATION_MARKER`] (#715).
pub const LLM_FAILED_TRUNCATION_MARKER: &str =
    "\n\n[output truncated: the model call failed after this was shown]";

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
    /// The runtime's record of what the preceding assistant turn did (spec
    /// 2026-09-19-turn-ledger-memory). Written by `remember_turn`, never by a
    /// provider; rendered as a user-role text block under a fixed header so
    /// the model reads it as testimony about itself, not as its own prose.
    TurnLedger {
        turn: u32,
        memory: crate::turn_ledger::TurnMemory,
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
    /// Everything the model read for this call, cached or not. On the wire
    /// Anthropic reports only the uncached remainder under this name and
    /// splits the rest into the two cache fields below; the adapter sums
    /// them, because every consumer of this number — context-fill for skill
    /// injection, the fleet spend guard, the `context_tokens` a client sees —
    /// wants the prompt size, and a cache hit does not make the prompt
    /// smaller. Backends without a cache report the two splits as zero.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens written to the provider cache this call (billed ~1.25x).
    pub cache_creation_input_tokens: u64,
    /// Prompt tokens served from the provider cache this call (billed ~0.1x).
    /// Zero on every call of a turn means caching is silently off — the API
    /// never errors for a missed cache, the bill just stays high.
    pub cache_read_input_tokens: u64,
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

/// Ceiling on a server-supplied `retry-after`. A fleet step's own deadline is
/// the next thing to fire, and a caller that obediently sleeps ten minutes has
/// simply chosen a worse failure than the one it was avoiding: past this point
/// the honest move is to fail fast and let the fallback chain route to another
/// candidate. Providers do send hour-long values during an outage.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(120);

/// Parse an HTTP `retry-after` header value (RFC 9110 §10.2.3), which is
/// either delta-seconds or an HTTP-date.
///
/// Returns `None` when the value is absent-in-effect — unparseable, negative,
/// or empty — so the caller keeps its own backoff schedule rather than
/// inventing a number. A date already in the past yields `ZERO` ("retry now"),
/// not `None`: the server did answer, it just answered with a stale clock.
/// The result is clamped to [`RETRY_AFTER_MAX`].
///
/// Deliberately *not* shared with
/// [`crate::durable::rate_limit::parse_anthropic_429`], which reads the same
/// header for a different question. That one decides when a suspended run
/// resumes: it also consults `anthropic-ratelimit-*-reset`, multiplies by 6 on
/// a 529, returns an absolute timestamp, and must not be clamped — a durable
/// run is allowed to wait an hour. This one decides whether to sleep *inside*
/// a live turn, where anything past [`RETRY_AFTER_MAX`] should fail over
/// instead. Folding them together would force one of those two answers to be
/// wrong.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // delta-seconds: a bare non-negative integer. `u64::from_str` already
    // rejects "-1", "5.5" and "soon".
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs).min(RETRY_AFTER_MAX));
    }
    // HTTP-date. chrono is already a direct dependency; `httpdate` exists only
    // transitively in the lockfile and promoting it would be a new dep for one
    // parse. RFC 2822 covers the IMF-fixdate form servers actually send.
    let when = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let delta = when.signed_duration_since(chrono::Utc::now());
    Some(
        delta
            .to_std()
            .unwrap_or(Duration::ZERO)
            .min(RETRY_AFTER_MAX),
    )
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
    /// The provider refused this request for now and may accept it later.
    /// `Some(d)` is the server's own `retry-after`, already parsed and
    /// clamped; `None` means it did not say and the caller falls back to its
    /// own backoff schedule. The delay is advice about *when*, never about
    /// *whether*: [`classify`] treats both the same.
    #[error("{}", match .0 {
        Some(d) => format!("rate limit (retry after {}s)", d.as_secs()),
        None => "rate limit".to_string(),
    })]
    RateLimit(Option<Duration>),
    #[error("timeout")]
    Timeout,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("server error: {0}")]
    ServerError(u16),
    #[error("insufficient credit")]
    InsufficientCredit,
    #[error("context window exceeded: {0}")]
    ContextExceeded(String),
    #[error("permission denied ({0}): {1}")]
    PermissionDenied(u16, String),
    #[error("safety policy rejected: {0}")]
    SafetyPolicyRejected(String),
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
    /// Map a non-success HTTP status into a typed error. This status-only
    /// layer is deliberately conservative: provider adapters promote only
    /// tested structured codes (or Anthropic's anchored message shapes).
    /// Unknown 4xx remain typed `Rejected` errors, but stop fleet-wide.
    ///
    /// Prefer [`LlmError::from_status_with_headers`] where the response
    /// headers are still in hand: a 429 mapped through here carries no
    /// `retry-after` and the caller is left guessing.
    pub fn from_status(status: u16, body: String) -> LlmError {
        LlmError::from_status_with_headers(status, body, &reqwest::header::HeaderMap::new())
    }

    /// [`LlmError::from_status`] with the response headers, so a 429 can carry
    /// the server's own `retry-after`. Every other status ignores them.
    pub fn from_status_with_headers(
        status: u16,
        body: String,
        headers: &reqwest::header::HeaderMap,
    ) -> LlmError {
        match status {
            401 | 403 => LlmError::Auth(status, body),
            429 => LlmError::RateLimit(
                headers
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after),
            ),
            402 => LlmError::InsufficientCredit,
            404 => LlmError::ModelNotFound(body),
            408 => LlmError::Timeout,
            500..=599 => LlmError::ServerError(status),
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

/// Fleet-wide policy: this governs every Agent using `FallbackLlmClient`, not
/// only the official orchestrator. Keep #947 model-rename and streaming guards.
pub fn classify(e: &LlmError) -> Disposition {
    match e {
        LlmError::RateLimit(_)
        | LlmError::Timeout
        | LlmError::Connect(_)
        | LlmError::ServerError(_) => Disposition::RetryThenAdvance,
        // These are proven candidate-specific by status or provider parser.
        LlmError::ModelNotFound(_) | LlmError::ContextExceeded(_) => Disposition::AdvanceNow,
        // Unknown refusals and policy/account failures are fail-closed. A
        // provider adapter may promote only an enumerated, tested code above.
        LlmError::InsufficientCredit
        | LlmError::PermissionDenied(..)
        | LlmError::SafetyPolicyRejected(_)
        | LlmError::Rejected(..)
        | LlmError::Auth(..)
        | LlmError::Http(_)
        | LlmError::InvalidResponse(_) => Disposition::Stop,
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
    fn retry_after_parses_delta_seconds() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("  5  "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_parses_http_date() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(30);
        let header = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let got = parse_retry_after(&header).expect("an HTTP-date is a valid retry-after");
        // The clock moves between formatting and parsing; 2s of slack keeps
        // this from flaking on a loaded machine.
        assert!(
            got >= Duration::from_secs(28) && got <= Duration::from_secs(30),
            "expected ~30s, got {got:?}"
        );
    }

    /// A date already in the past means "retry now", not "never" — the server
    /// is entitled to send a stale one and we must not turn that into a hang.
    #[test]
    fn retry_after_past_date_is_zero() {
        assert_eq!(
            parse_retry_after("Sat, 01 Feb 2020 00:00:00 GMT"),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn retry_after_garbage_is_none() {
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("   "), None);
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after("5.5"), None);
    }

    #[test]
    fn retry_after_is_clamped() {
        assert_eq!(parse_retry_after("99999"), Some(RETRY_AFTER_MAX));
    }

    /// The delay is advice about *when* to retry, never about *whether*. A
    /// 429 stays retry-then-advance no matter what the header said.
    #[test]
    fn classify_ignores_retry_after() {
        use Disposition::*;
        assert!(matches!(
            classify(&LlmError::RateLimit(None)),
            RetryThenAdvance
        ));
        assert!(matches!(
            classify(&LlmError::RateLimit(Some(Duration::from_secs(60)))),
            RetryThenAdvance
        ));
    }

    /// The error text reaches task JSON, where a human reads it. When the
    /// server told us how long to wait, that number belongs in the message.
    #[test]
    fn rate_limit_renders_the_delay_when_known() {
        assert_eq!(LlmError::RateLimit(None).to_string(), "rate limit");
        assert_eq!(
            LlmError::RateLimit(Some(Duration::from_secs(42))).to_string(),
            "rate limit (retry after 42s)"
        );
    }

    #[test]
    fn from_status_with_headers_reads_retry_after() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert!(matches!(
            LlmError::from_status_with_headers(429, "x".into(), &headers),
            LlmError::RateLimit(Some(d)) if d == Duration::from_secs(7)
        ));
        // A header on a non-429 is not our business.
        assert!(matches!(
            LlmError::from_status_with_headers(503, "x".into(), &headers),
            LlmError::ServerError(503)
        ));
        // No header at all is the pre-existing behaviour, byte for byte.
        let empty = reqwest::header::HeaderMap::new();
        assert!(matches!(
            LlmError::from_status_with_headers(429, "x".into(), &empty),
            LlmError::RateLimit(None)
        ));
    }

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
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
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
            LlmError::RateLimit(None)
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
        assert!(matches!(
            classify(&LlmError::RateLimit(None)),
            RetryThenAdvance
        ));
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

    /// A missing model is proven candidate-specific; exhausted account credit
    /// is not and must not silently route to a different billing path.
    #[test]
    fn only_proven_candidate_failures_advance_without_retrying() {
        use Disposition::*;
        assert!(matches!(
            classify(&LlmError::ModelNotFound("no such model".into())),
            AdvanceNow
        ));
        assert!(matches!(classify(&LlmError::InsufficientCredit), Stop));
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

    #[test]
    fn fleet_wide_unknown_client_errors_and_credit_stop() {
        use Disposition::*;
        for status in [400, 409, 413, 422] {
            let error = LlmError::from_status(status, "provider-specific refusal".into());
            assert!(matches!(error, LlmError::Rejected(s, _) if s == status));
            assert_eq!(classify(&error), Stop, "status {status}");
        }
        assert_eq!(classify(&LlmError::InsufficientCredit), Stop);
    }

    #[test]
    fn typed_request_failures_have_bounded_dispositions() {
        use Disposition::*;
        assert_eq!(
            classify(&LlmError::ContextExceeded("too many tokens".into())),
            AdvanceNow
        );
        assert_eq!(
            classify(&LlmError::PermissionDenied(403, "denied".into())),
            Stop
        );
        assert_eq!(
            classify(&LlmError::SafetyPolicyRejected("unsafe".into())),
            Stop
        );
    }
}

#[cfg(test)]
mod stream_idle_tests;

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
        // reqwest reads proxy env at build time, so this needs the real
        // variable — see the guard's own docs for why one lock covers all.
        let _env = mur_common::test_env::EnvGuard::set([("HTTP_PROXY", "http://127.0.0.1:1")]);
        let client = llm_client_builder().build().unwrap();
        let resp = client.get(format!("http://{addr}/")).send().await;
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
