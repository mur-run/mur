//! Anthropic Claude client — remote inference via Anthropic Messages API.
//!
//! POST $ANTHROPIC_BASE_URL/v1/messages
//!   x-api-key: $ANTHROPIC_API_KEY
//!   anthropic-version: 2023-06-01
//!   {"model": ..., "max_tokens": ..., "system": [{"type": "text", ..., "cache_control": ...}],
//!    "messages": [...]}
//!
//! Subscription-OAuth tokens (sk-ant-oat*) need different auth + headers
//! than this provider-neutral client supplies. Point `ANTHROPIC_BASE_URL`
//! at a local OAuth bridge (e.g. cc-proxy) for that path.
//!
//! The Anthropic API has a top-level `system` field rather than a system role
//! in `messages`. We translate `LlmMessage{role:"system"}` -> top-level system.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StopReason};
use async_trait::async_trait;
use mur_common::llm::anthropic_base_url;
use mur_common::llm::supported_effort;
use serde_json::json;

mod convert;
use convert::{mark_cache_breakpoint, rich_messages_to_anthropic};

const DEFAULT_VERSION: &str = "2023-06-01";
/// Output-token ceiling when a request leaves `max_tokens` unset. This is a
/// CEILING, not a target — the model only generates what it needs, so cost
/// rises only when output is genuinely large. Coding agents routinely write
/// whole source files via large `bash` heredocs; a 1024 cap truncated those
/// responses mid-tool_use, leaving the tool_use `input` JSON incomplete and
/// the call unparseable.
///
/// Raised from 16384 when `claude-opus-5` became the default: this request
/// never sends a `thinking` field, which meant "no thinking" on Opus 4.6 but
/// means "adaptive thinking, on" from Opus 5 onward — and `max_tokens` caps
/// thinking AND response text together. Without the extra room the same
/// mid-tool_use truncation returns, now caused by thinking eating the budget.
const DEFAULT_MAX_TOKENS: u32 = 32768;

// Anthropic currently supplies no structured context-overflow code. Keep this
// exact, versioned fixture shape local to this adapter; broad substring matching
// would confuse spend-limit and unrelated invalid-request failures.
const PROMPT_TOO_LONG_MESSAGE_V1: &str = "prompt is too long";

/// `headers` is threaded through to every `from_status` tail — including the
/// parse-failure early return, which is the path a bare-text 429 takes — so a
/// rate limit keeps the server's own `retry-after`.
pub(crate) fn map_anthropic_error(
    status: u16,
    body: &str,
    headers: &reqwest::header::HeaderMap,
) -> LlmError {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return LlmError::from_status_with_headers(status, body.to_string(), headers),
    };
    let error = &parsed["error"];
    let error_type = error["type"].as_str().unwrap_or_default();
    let code = error["code"].as_str().unwrap_or_default();
    let message = error["message"].as_str().unwrap_or(body).to_string();

    if status == 400
        && error_type == "invalid_request_error"
        && message == PROMPT_TOO_LONG_MESSAGE_V1
    {
        return LlmError::ContextExceeded(message);
    }
    match (error_type, code) {
        ("permission_error", _) => LlmError::PermissionDenied(status, message),
        (_, "content_policy_violation" | "safety_policy_violation") => {
            LlmError::SafetyPolicyRejected(message)
        }
        ("not_found_error", _) => LlmError::ModelNotFound(message),
        _ => LlmError::from_status_with_headers(status, body.to_string(), headers),
    }
}

// There was a TOTAL request timeout here (60s, then 180s). It is gone: reqwest
// applies `.timeout()` until the response body finishes, so it bounded streamed
// responses too, and at roughly 50-80 output tokens/sec it ran out somewhere
// around 3-5k tokens — well under `DEFAULT_MAX_TOKENS`, so raising the token
// ceiling alone changed nothing. Adaptive thinking spends part of the same wall
// clock before any text is emitted, tightening it further. Raising the number
// again would only move the cliff, so the clock was removed: liveness is now
// the gap between streamed chunks (`crate::llm::StreamActivity`), and the
// connect clock lives in `llm_client_builder()`. See spec
// docs/superpowers/specs/2026-09-13-llm-idle-not-total-design.md.

/// Service constant used by `mur agent secret set` / `mur agent secret delete`.
/// Account format is `{agent_name}/{KEY}` (e.g. `kelp/ANTHROPIC_API_KEY`).
/// Must stay in sync with `mur-core/src/cmd/agent.rs::SECRET_SERVICE`.
const MUR_AGENT_KEYCHAIN_SERVICE: &str = "mur-agent";

/// What is wrong with an `sk-ant-oat*` key at a given base URL, if anything.
///
/// Split out of `warn_if_oauth_key_misconfigured` so the rule is unit-testable
/// without a tracing subscriber and without the process-global "warn once"
/// latch, which makes the warning observable exactly once per test binary.
#[derive(Debug, PartialEq, Eq)]
enum OauthKeyMisuse {
    /// Sent straight at Anthropic, which does not accept subscription tokens.
    RejectedUpstream,
    /// Sent at a loopback bridge. `mur-model-gateway` keys its mode on the
    /// *shape* of the inbound credential: an `sk-ant-oat*` value in `x-api-key`
    /// is read as "this client wants OAuth", and the bridge answers by
    /// attaching its own, fresher Claude Code token and dropping the one that
    /// arrived. The configured secret is therefore never sent anywhere, and a
    /// model entry that looks like an independent credential path silently
    /// shares whatever credential the bridge holds — so switching to it does
    /// not escape an outage on that credential.
    IgnoredByBridge,
}

/// `None` when the key is not a subscription token, or when the base URL is
/// some other remote host whose credential policy we cannot know.
///
/// ponytail: loopback detection is a small host allowlist, not the whole of
/// 127.0.0.0/8. Widen it if someone actually binds a bridge to 127.0.0.2.
fn classify_oauth_key(api_key: &str, base_url: &str) -> Option<OauthKeyMisuse> {
    if !api_key.contains("sk-ant-oat") {
        return None;
    }
    if base_url.starts_with("https://api.anthropic.com") {
        return Some(OauthKeyMisuse::RejectedUpstream);
    }
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    match host.as_deref() {
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]") => Some(OauthKeyMisuse::IgnoredByBridge),
        _ => None,
    }
}

/// Warn once per process, per kind, if the resolved API key is a Claude
/// subscription OAuth token in a place that cannot use it.
///
/// The loopback arm exists because the other arm's advice used to end the
/// story: it told the reader to point the base URL at a local OAuth bridge,
/// and doing exactly that produced no warning at all — while the bridge went
/// on ignoring the key. A real config kept an `sk-ant-oat*` token in
/// `~/.mur/secrets/anthropic.key` for months believing it was a second,
/// independent credential; it was the same credential the whole time, which
/// only surfaced when switching models during an outage changed nothing.
///
/// The two kinds latch separately. One shared flag would let whichever route
/// ran first silence the other, and they are different diagnoses with
/// different fixes.
fn warn_if_oauth_key_misconfigured(api_key: &str, base_url: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    match classify_oauth_key(api_key, base_url) {
        Some(OauthKeyMisuse::RejectedUpstream) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if WARNED.swap(true, Ordering::Relaxed) {
                return;
            }
            tracing::warn!(
                base_url = %base_url,
                "ANTHROPIC_API_KEY looks like an OAuth subscription token (sk-ant-oat*), \
                 but base URL is api.anthropic.com — Anthropic will reject the request. \
                 Point ANTHROPIC_BASE_URL at a local OAuth bridge, which supplies its \
                 own credential and does not need this key."
            );
        }
        Some(OauthKeyMisuse::IgnoredByBridge) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if WARNED.swap(true, Ordering::Relaxed) {
                return;
            }
            tracing::warn!(
                base_url = %base_url,
                "ANTHROPIC_API_KEY is an OAuth subscription token (sk-ant-oat*) pointed at \
                 a loopback bridge. The bridge attaches its own Claude Code credential and \
                 drops this one, so this key is never sent and this model entry is not an \
                 independent credential path. Use an sk-ant-api03 key if it was meant to be."
            );
        }
        None => {}
    }
}

/// How a request authenticates. Explicit so an authless route is a
/// deliberate choice at construction, not an empty key that happens to be
/// sent as `x-api-key: `. `None` exists for the loopback gateway route
/// (`provider: claude`), where the gateway attaches the Claude Code OAuth
/// token itself — and picks that mode by the header being *absent*.
#[derive(Clone)]
enum AnthropicAuth {
    ApiKey(String),
    None,
}

pub struct AnthropicClient {
    base_url: String,
    auth: AnthropicAuth,
    version: String,
    model: String,
    http: crate::sandbox::reqwest_guard::GuardedHttpClient,
}

impl AnthropicClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let http = crate::sandbox::reqwest_guard::GuardedHttpClient::unrestricted(
            crate::llm::llm_client_builder(),
        )
        .expect("failed to build guarded reqwest client");
        Self {
            base_url,
            auth: AnthropicAuth::ApiKey(api_key),
            version: DEFAULT_VERSION.to_string(),
            model,
            http,
        }
    }

    /// Construct with a pre-built reqwest client (e.g. carrying a HostGuard DNS resolver).
    pub fn new_with_http_client(
        base_url: String,
        api_key: String,
        model: String,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Self {
        Self {
            base_url,
            auth: AnthropicAuth::ApiKey(api_key),
            version: DEFAULT_VERSION.to_string(),
            model,
            http,
        }
    }

    /// Messages transport that sends no credential at all. Only the loopback
    /// gateway route may use this (see `llm::claude`), which is why it is
    /// crate-private: the gateway owns the OAuth token, and a key here would
    /// either leak or silently switch the bill to the Anthropic API.
    pub(crate) fn authless_with_http(
        base_url: String,
        model: String,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Self {
        Self {
            base_url,
            auth: AnthropicAuth::None,
            version: DEFAULT_VERSION.to_string(),
            model,
            http,
        }
    }

    /// Absent, never empty: the gateway keys its mode on header presence.
    fn apply_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AnthropicAuth::ApiKey(key) => request.header("x-api-key", key),
            AnthropicAuth::None => request,
        }
    }

    /// Convenience constructor reading API key from `ANTHROPIC_API_KEY`.
    pub fn from_env(model: String) -> Result<Self, LlmError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| LlmError::InvalidResponse("ANTHROPIC_API_KEY not set".into()))?;
        Ok(Self::new(anthropic_base_url(), api_key, model))
    }

    /// Resolve credentials using mur's agent-aware precedence (no `model_ref`):
    ///
    ///   1. OS keychain at service=`mur-agent`, account=`{agent}/ANTHROPIC_API_KEY`
    ///      — i.e. what `mur agent secret set <agent> ANTHROPIC_API_KEY <token>` writes.
    ///   2. The `ANTHROPIC_API_KEY` env var — only when no keychain entry exists.
    ///
    /// This inverts Claude Code's official precedence (env beats subscription
    /// OAuth) and mirrors `gh auth token`'s keychain-first model. Rationale:
    /// a per-agent keychain entry the user explicitly stored is far stronger
    /// evidence of intent than a process-wide env var, which is often a
    /// stale leftover from a prior shell session and silently swaps the
    /// caller's billing identity from subscription to per-token API.
    ///
    /// Keychain backend errors (locked keychain, permission denied, etc.)
    /// propagate as a hard error rather than silently falling through to
    /// the env var — masking those would defeat the whole purpose.
    pub async fn from_agent_credentials(agent_name: &str, model: String) -> Result<Self, LlmError> {
        let account = format!("{agent_name}/ANTHROPIC_API_KEY");
        // Through `SecretRef`, so a value cached before the sandbox sealed is
        // used. Reaching the backend here is what fails after an upgrade: the
        // Keychain grant binds to the signing identity and a background agent
        // cannot re-prompt (#866). The supervisor pre-caches this exact ref.
        let pre = mur_common::secret::SecretRef::Keychain {
            service: MUR_AGENT_KEYCHAIN_SERVICE.to_string(),
            account: account.clone(),
        };
        if let Some(v) = pre.resolve_preseal_cached() {
            return Ok(Self::from_secret_string(&v, model, None));
        }
        match mur_common::secret::keychain_get(MUR_AGENT_KEYCHAIN_SERVICE, &account).await {
            Ok(Some(secret)) => Ok(Self::from_secret_string(&secret, model, None)),
            Ok(None) => Self::from_env(model),
            Err(e) => Err(LlmError::InvalidResponse(format!(
                "keychain backend error reading {MUR_AGENT_KEYCHAIN_SERVICE}/{account}: {e}"
            ))),
        }
    }

    /// Construct from a resolved SecretString and an optional registry-supplied
    /// base URL. Used by the supervisor when a model_ref provides the secret
    /// (so we don't have to round-trip through ANTHROPIC_API_KEY).
    pub fn from_secret_string(
        key: &secrecy::SecretString,
        model: String,
        base_url: Option<String>,
    ) -> Self {
        use secrecy::ExposeSecret;
        let base = base_url.unwrap_or_else(anthropic_base_url);
        Self::new(base, key.expose_secret().to_string(), model)
    }

    /// Like [`from_secret_string`] but uses a pre-built reqwest client
    /// (e.g. one carrying a B1 HostGuard DNS resolver).
    pub fn from_secret_string_with_http(
        key: &secrecy::SecretString,
        model: String,
        base_url: Option<String>,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Self {
        use secrecy::ExposeSecret;
        let base = base_url.unwrap_or_else(anthropic_base_url);
        Self::new_with_http_client(base, key.expose_secret().to_string(), model, http)
    }

    /// Like [`from_agent_credentials`] but injects a pre-built reqwest client
    /// (e.g. one carrying a B1 HostGuard DNS resolver).
    pub async fn from_agent_credentials_with_http(
        agent_name: &str,
        model: String,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Result<Self, LlmError> {
        let account = format!("{agent_name}/ANTHROPIC_API_KEY");
        // Through `SecretRef`, so a value cached before the sandbox sealed is
        // used. Reaching the backend here is what fails after an upgrade: the
        // Keychain grant binds to the signing identity and a background agent
        // cannot re-prompt (#866). The supervisor pre-caches this exact ref.
        let pre = mur_common::secret::SecretRef::Keychain {
            service: MUR_AGENT_KEYCHAIN_SERVICE.to_string(),
            account: account.clone(),
        };
        if let Some(v) = pre.resolve_preseal_cached() {
            return Ok(Self::from_secret_string_with_http(&v, model, None, http));
        }
        match mur_common::secret::keychain_get(MUR_AGENT_KEYCHAIN_SERVICE, &account).await {
            Ok(Some(secret)) => Ok(Self::from_secret_string_with_http(
                &secret, model, None, http,
            )),
            Ok(None) => {
                let api_key = std::env::var("ANTHROPIC_API_KEY")
                    .map_err(|_| LlmError::InvalidResponse("ANTHROPIC_API_KEY not set".into()))?;
                Ok(Self::new_with_http_client(
                    anthropic_base_url(),
                    api_key,
                    model,
                    http,
                ))
            }
            Err(e) => Err(LlmError::InvalidResponse(format!(
                "keychain backend error reading {MUR_AGENT_KEYCHAIN_SERVICE}/{account}: {e}"
            ))),
        }
    }
}

/// The `usage` object of a response. Every field is optional on the wire:
/// a non-streaming body carries them all, `message_start` carries the
/// prompt side, `message_delta` carries `output_tokens` and on newer API
/// versions repeats the rest — so `merge` only overwrites what is present.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Usage {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

impl Usage {
    fn merge(&mut self, u: &serde_json::Value) {
        let take = |k: &str, slot: &mut u64| {
            if let Some(n) = u[k].as_u64() {
                *slot = n;
            }
        };
        take("input_tokens", &mut self.input_tokens);
        take(
            "cache_creation_input_tokens",
            &mut self.cache_creation_input_tokens,
        );
        take("cache_read_input_tokens", &mut self.cache_read_input_tokens);
        take("output_tokens", &mut self.output_tokens);
    }

    /// `input_tokens` on the wire is only the uncached remainder; the
    /// response reports the whole prompt (see `LlmResponse::input_tokens`).
    fn into_response(
        self,
        text: String,
        model: String,
        tool_calls: Vec<crate::llm::ToolCallResult>,
        stop_reason: StopReason,
    ) -> LlmResponse {
        LlmResponse {
            text,
            input_tokens: self.input_tokens
                + self.cache_creation_input_tokens
                + self.cache_read_input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            model,
            tool_calls,
            stop_reason,
        }
    }
}

/// Accumulator for an Anthropic SSE response while it streams.
struct StreamAccum {
    text: String,
    usage: Usage,
    tool_calls: Vec<crate::llm::ToolCallResult>,
    stop_reason: StopReason,
    /// The in-progress tool_use block: (id, name, partial-JSON args buffer).
    cur_tool: Option<(String, String, String)>,
    /// Evidence for an empty response, so the error can say WHY it was
    /// empty: a model that chose `end_turn` with nothing to say reads very
    /// differently from a stream cut short or an in-band `error` event.
    diag: StreamDiag,
}

/// What the stream looked like, kept only to explain an empty result.
#[derive(Default)]
struct StreamDiag {
    /// `stop_reason` exactly as sent; `refusal` and friends collapse to
    /// `EndTurn` in `StreamAccum::stop_reason`.
    raw_stop_reason: Option<String>,
    /// The terminal `message_stop` event arrived.
    message_stop: bool,
    /// `content_block_start` events seen, of any type.
    blocks: usize,
    thinking_chars: usize,
    /// An in-band `{"type":"error"}` event: `<type>: <message>`.
    error: Option<String>,
}

impl Default for StreamAccum {
    fn default() -> Self {
        Self {
            text: String::new(),
            usage: Usage::default(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            cur_tool: None,
            diag: StreamDiag::default(),
        }
    }
}

/// Apply one parsed SSE `data:` event to `acc`. Returns a `StreamDelta` to
/// forward to the sink iff this event carried answer text or reasoning.
/// Mirrors the non-stream `parse_response_body` for tool_use + stop_reason.
fn apply_sse_event(acc: &mut StreamAccum, v: &serde_json::Value) -> Option<super::StreamDelta> {
    use super::{StopReason, StreamDelta, ToolCallResult};
    match v["type"].as_str() {
        Some("content_block_start") => {
            acc.diag.blocks += 1;
            let cb = &v["content_block"];
            if cb["type"].as_str() == Some("tool_use") {
                acc.cur_tool = Some((
                    cb["id"].as_str().unwrap_or("").to_string(),
                    cb["name"].as_str().unwrap_or("").to_string(),
                    String::new(),
                ));
            } else {
                acc.cur_tool = None;
            }
            None
        }
        Some("content_block_delta") => {
            let d = &v["delta"];
            match d["type"].as_str() {
                Some("text_delta") => {
                    let t = d["text"].as_str().unwrap_or("");
                    if t.is_empty() {
                        return None;
                    }
                    acc.text.push_str(t);
                    Some(StreamDelta {
                        text: t.to_string(),
                        thinking: false,
                    })
                }
                Some("thinking_delta") => {
                    let t = d["thinking"].as_str().unwrap_or("");
                    if t.is_empty() {
                        return None;
                    }
                    acc.diag.thinking_chars += t.chars().count();
                    Some(StreamDelta {
                        text: t.to_string(),
                        thinking: true,
                    })
                }
                Some("input_json_delta") => {
                    match (acc.cur_tool.as_mut(), d["partial_json"].as_str()) {
                        (Some((_, _, buf)), Some(pj)) => buf.push_str(pj),
                        (None, Some(_)) => {
                            tracing::warn!(
                                "anthropic stream: input_json_delta with no open tool_use block — dropping args fragment"
                            );
                        }
                        _ => {}
                    }
                    None
                }
                _ => None,
            }
        }
        Some("content_block_stop") => {
            if let Some((id, name, buf)) = acc.cur_tool.take() {
                let input = if buf.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&buf).unwrap_or_else(|_| serde_json::json!({}))
                };
                acc.tool_calls.push(ToolCallResult {
                    call_id: id,
                    tool_name: name,
                    input,
                });
            }
            None
        }
        Some("message_start") => {
            acc.usage.merge(&v["message"]["usage"]);
            None
        }
        Some("message_delta") => {
            if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                acc.diag.raw_stop_reason = Some(sr.to_string());
                acc.stop_reason = match sr {
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
            }
            acc.usage.merge(&v["usage"]);
            None
        }
        Some("message_stop") => {
            acc.diag.message_stop = true;
            None
        }
        Some("error") => {
            let e = &v["error"];
            acc.diag.error = Some(format!(
                "{}: {}",
                e["type"].as_str().unwrap_or("unknown"),
                e["message"].as_str().unwrap_or("")
            ));
            None
        }
        _ => None,
    }
}

/// `empty streamed response (…)` with the evidence of why. The prefix is
/// load-bearing: task_runner matches on it to decide retries.
fn empty_stream_error(acc: &StreamAccum) -> String {
    let d = &acc.diag;
    let mut msg = format!(
        "empty streamed response (stop_reason={}, message_stop={}, output_tokens={}, blocks={}, thinking_chars={}",
        d.raw_stop_reason.as_deref().unwrap_or("none"),
        d.message_stop,
        acc.usage.output_tokens,
        d.blocks,
        d.thinking_chars,
    );
    if let Some(e) = &d.error {
        msg.push_str(", error=");
        msg.push_str(e);
    }
    msg.push(')');
    msg
}

/// Turn a fully-drained `StreamAccum` into the final result. A tool-only
/// response legitimately has empty text — only error when BOTH answer text
/// and tool calls are empty. A response that hit the max_tokens ceiling while
/// still inside a thinking block (no text or tool_use ever started) is also
/// legitimate, not malformed — let it through so task_runner's MaxTokens
/// retry path can nudge the model toward a shorter answer instead of
/// surfacing a raw protocol error.
/// `interrupted` = the stream stopped sending rather than ending (#1287).
///
/// Nothing special is needed to protect a half-emitted tool call: `cur_tool`
/// is committed to `acc.tool_calls` only at `content_block_stop`, so a call
/// interrupted mid-arguments is already absent. The model is then told plainly
/// that its call did not happen, instead of being handed arguments it never
/// finished choosing.
fn finish_stream(
    acc: StreamAccum,
    model: String,
    interrupted: bool,
) -> Result<LlmResponse, LlmError> {
    if acc.text.is_empty() && acc.tool_calls.is_empty() && acc.stop_reason != StopReason::MaxTokens
    {
        // An interruption with nothing assembled is a timeout, not a malformed
        // response: `classify` makes `Timeout` RetryThenAdvance and
        // `InvalidResponse` Stop, and nothing reached the sink to duplicate.
        if interrupted {
            tracing::warn!(
                blocks = acc.diag.blocks,
                "anthropic stream went idle before any content; reporting a timeout"
            );
            return Err(LlmError::Timeout);
        }
        let msg = empty_stream_error(&acc);
        // Warn, not debug: this is the line that tells a silent `end_turn`
        // from a failed stream, and the default filter drops debug.
        tracing::warn!(model = %model, "{msg}");
        return Err(LlmError::InvalidResponse(msg));
    }
    let stop_reason = if interrupted {
        StopReason::Interrupted
    } else {
        acc.stop_reason
    };
    Ok(acc
        .usage
        .into_response(acc.text, model, acc.tool_calls, stop_reason))
}

fn parse_response_body(
    v: &serde_json::Value,
) -> Result<
    (
        String,
        Vec<crate::llm::ToolCallResult>,
        crate::llm::StopReason,
    ),
    LlmError,
> {
    use crate::llm::{StopReason, ToolCallResult};

    let content = v["content"]
        .as_array()
        .ok_or_else(|| LlmError::InvalidResponse("missing content array".into()))?;

    let text = content
        .iter()
        .filter_map(|b| {
            if b["type"].as_str() == Some("text") {
                b["text"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    let tool_calls: Vec<ToolCallResult> = content
        .iter()
        .filter(|b| b["type"].as_str() == Some("tool_use"))
        .map(|b| ToolCallResult {
            call_id: b["id"].as_str().unwrap_or("").to_string(),
            tool_name: b["name"].as_str().unwrap_or("").to_string(),
            input: b["input"].clone(),
        })
        .collect();

    let stop_reason = match v["stop_reason"].as_str() {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };

    Ok((text, tool_calls, stop_reason))
}

impl AnthropicClient {
    /// Build the `/v1/messages` body with two prompt-cache breakpoints (the
    /// API allows four): the system prompt — the prefix every call of a turn
    /// shares; tools render before it and ride along — and the last content
    /// block of the last message, so each call of an agentic loop re-reads
    /// the whole prior conversation at the cached rate instead of paying for
    /// it again. A turn that makes 70 calls resends its history 70 times;
    /// cache reads are what make that affordable. Nothing here is beta: the
    /// `2023-06-01` version header accepts `cache_control`.
    ///
    /// ponytail: the system prompt is one block, so a turn whose per-turn
    /// skill injection changed misses the system block once (the messages
    /// breakpoint behind it still hits within the turn). Splitting the
    /// stable base from the injected layer, or moving the injection to a
    /// mid-conversation `role: system` message, is the upgrade path when
    /// cross-turn misses show up in `cache_creation_input_tokens`.
    fn request_body(&self, req: &LlmRequest, stream: bool) -> serde_json::Value {
        let (system, mut convo, _) = rich_messages_to_anthropic(&req.messages);
        mark_cache_breakpoint(&mut convo);
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": convo,
        });
        if stream {
            body["stream"] = json!(true);
        }
        if let Some(s) = system.filter(|s| !s.is_empty()) {
            body["system"] = json!([
                {"type": "text", "text": s, "cache_control": {"type": "ephemeral"}}
            ]);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        // Effort is per-call, narrowed to what this model actually accepts —
        // an unsupported level is a 400. Absent = the API default (`high`).
        if let Some(e) = req.effort.and_then(|e| supported_effort(&self.model, e)) {
            body["output_config"] = json!({ "effort": e.as_str() });
        }
        if !req.tools.is_empty() {
            body["tools"] = json!(
                req.tools
                    .iter()
                    .map(|t| json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    }))
                    .collect::<Vec<_>>()
            );
        }
        body
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = self.request_body(&req, false);

        if let AnthropicAuth::ApiKey(key) = &self.auth {
            warn_if_oauth_key_misconfigured(key, &self.base_url);
        }

        let url = format!("{}/v1/messages", self.base_url);
        let resp = self
            .apply_auth(
                self.http
                    .post(&url)
                    .map_err(LlmError::Http)?
                    .header("anthropic-version", &self.version)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;

        let status = resp.status();
        // The header map must be taken before `text()` consumes the response:
        // the status check below happens after the body is already read, so a
        // 429's `retry-after` is otherwise gone by the time it is mapped.
        let headers = resp.headers().clone();
        let body_text = resp.text().await.map_err(|e| LlmError::from_reqwest(&e))?;
        if !status.is_success() {
            tracing::warn!(status = %status, body = %body_text, "anthropic non-2xx");
            return Err(map_anthropic_error(status.as_u16(), &body_text, &headers));
        }
        let v: serde_json::Value = serde_json::from_str(&body_text)
            .map_err(|e| LlmError::Http(format!("parse response: {e}")))?;

        let (text, tool_calls, stop_reason) = parse_response_body(&v)?;
        let mut usage = Usage::default();
        usage.merge(&v["usage"]);
        Ok(usage.into_response(text, self.model.clone(), tool_calls, stop_reason))
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<super::StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.request_body(&req, true);
        if let AnthropicAuth::ApiKey(key) = &self.auth {
            warn_if_oauth_key_misconfigured(key, &self.base_url);
        }

        let url = format!("{}/v1/messages", self.base_url);
        let mut resp = self
            .apply_auth(
                self.http
                    .post(&url)
                    .map_err(LlmError::Http)?
                    .header("anthropic-version", &self.version)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let headers = resp.headers().clone();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(map_anthropic_error(status.as_u16(), &body_text, &headers));
        }

        // Anthropic streams SSE: `event: <type>` + `data: {json}`. Each data
        // line carries a `type` (content_block_delta / message_start / …); we
        // parse those via `apply_sse_event` which accumulates text, tool_use
        // blocks, token counts, and stop_reason, and returns `StreamDelta`
        // chunks to forward to the sink.
        let mut buf: Vec<u8> = Vec::new();
        let mut acc = StreamAccum::default();
        let mut activity = crate::llm::StreamActivity::from_env();
        let mut interrupted = false;
        loop {
            // Bounded by silence, never by total time (#1287). The bound is
            // applied to the arrival of BYTES, not to anything reaching the
            // sink: a chunk carrying only tool-argument fragments or a usage
            // frame is life, and a sink-side guard would have called it idle.
            let next = activity
                .bounded(async { resp.chunk().await.map_err(|e| LlmError::from_reqwest(&e)) })
                .await;
            let chunk = match next {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(LlmError::Timeout) => {
                    interrupted = true;
                    break;
                }
                Err(e) => return Err(e),
            };
            buf.extend_from_slice(&chunk);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=nl).collect();
                let line = std::str::from_utf8(&raw).unwrap_or("").trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                if let Some(delta) = apply_sse_event(&mut acc, &v) {
                    let _ = sink.send(delta).await;
                }
            }
        }
        finish_stream(acc, self.model.clone(), interrupted)
    }
}

#[cfg(test)]
mod tests;
