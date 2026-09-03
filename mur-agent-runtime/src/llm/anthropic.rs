//! Anthropic Claude client — remote inference via Anthropic Messages API.
//!
//! POST $ANTHROPIC_BASE_URL/v1/messages
//!   x-api-key: $ANTHROPIC_API_KEY
//!   anthropic-version: 2023-06-01
//!   {"model": ..., "max_tokens": ..., "system": "...", "messages": [...]}
//!
//! Subscription-OAuth tokens (sk-ant-oat*) need different auth + headers
//! than this provider-neutral client supplies. Point `ANTHROPIC_BASE_URL`
//! at a local OAuth bridge (e.g. cc-proxy) for that path.
//!
//! The Anthropic API has a top-level `system` field rather than a system role
//! in `messages`. We translate `LlmMessage{role:"system"}` -> top-level system.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, RichMessage, StopReason};
use async_trait::async_trait;
use mur_common::llm::anthropic_base_url;
use mur_common::llm::supported_effort;
use serde_json::json;

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

/// Total time allowed for a single LLM request (including server think time).
///
/// This is a TOTAL timeout — reqwest applies it until the response body has
/// finished, so it bounds streamed responses too, and it is the constraint
/// that actually binds. At roughly 50-80 output tokens/sec, 60s ran out
/// somewhere around 3-5k tokens, well under `DEFAULT_MAX_TOKENS`; raising the
/// token ceiling alone would have changed nothing. Adaptive thinking (now on
/// by default — see above) spends part of that same wall clock before any
/// text is emitted, which tightened it further.
///
/// 180s is chosen to make the token ceiling reachable while still failing a
/// wedged request inside a few minutes rather than holding the slot open.
const LLM_REQUEST_TIMEOUT_SECS: u64 = 180;
/// Time allowed to establish a TCP connection to the LLM endpoint.
const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Service constant used by `mur agent secret set` / `mur agent secret delete`.
/// Account format is `{agent_name}/{KEY}` (e.g. `kelp/ANTHROPIC_API_KEY`).
/// Must stay in sync with `mur-core/src/cmd/agent.rs::SECRET_SERVICE`.
const MUR_AGENT_KEYCHAIN_SERVICE: &str = "mur-agent";

/// Warn once per process if the resolved API key looks like a Claude
/// subscription OAuth token (`sk-ant-oat*`) but the configured base URL
/// still points at api.anthropic.com — Anthropic will reject the call.
fn warn_if_oauth_key_misconfigured(api_key: &str, base_url: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !api_key.contains("sk-ant-oat") {
        return;
    }
    if !base_url.starts_with("https://api.anthropic.com") {
        return;
    }
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        base_url = %base_url,
        "ANTHROPIC_API_KEY looks like an OAuth subscription token (sk-ant-oat*), \
         but base URL is api.anthropic.com — Anthropic will reject the request. \
         Point ANTHROPIC_BASE_URL at a local OAuth bridge."
    );
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
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let http = crate::llm::llm_client_builder()
            .timeout(std::time::Duration::from_secs(LLM_REQUEST_TIMEOUT_SECS))
            .connect_timeout(std::time::Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
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
        http: reqwest::Client,
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
        http: reqwest::Client,
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
        http: reqwest::Client,
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
        http: reqwest::Client,
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

/// Convert `RichMessage` list to Anthropic wire format.
/// Returns `(system_text, conversation_messages, agent_text_for_stream)`.
/// Agent_text is the last assistant text for streaming (unused in non-streaming).
fn rich_messages_to_anthropic(
    msgs: &[RichMessage],
) -> (Option<String>, Vec<serde_json::Value>, Option<String>) {
    /// Push a `{role, content}` entry, merging into the last entry when roles
    /// match (Anthropic 400s on consecutive same-role messages).
    fn push_coalesced(convo: &mut Vec<serde_json::Value>, role: &str, content: serde_json::Value) {
        /// Normalize message content to an array of content blocks.
        fn blocks(content: serde_json::Value) -> Vec<serde_json::Value> {
            match content {
                serde_json::Value::Array(a) => a,
                serde_json::Value::String(s) => vec![json!({"type": "text", "text": s})],
                other => vec![other],
            }
        }
        if let Some(last) = convo.last_mut()
            && last["role"] == role
        {
            let mut merged = blocks(last["content"].take());
            merged.extend(blocks(content));
            last["content"] = json!(merged);
            return;
        }
        convo.push(json!({"role": role, "content": content}));
    }

    let mut system_chunks: Vec<String> = Vec::new();
    let mut convo: Vec<serde_json::Value> = Vec::new();

    for m in msgs {
        match m {
            RichMessage::Text { role, content } => {
                if role == "system" {
                    system_chunks.push(content.clone());
                } else {
                    let r = if role == "agent" {
                        "assistant"
                    } else {
                        role.as_str()
                    };
                    push_coalesced(&mut convo, r, json!(content));
                }
            }
            RichMessage::ToolUse { text, calls } => {
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if let Some(t) = text
                    && !t.is_empty()
                {
                    parts.push(json!({"type": "text", "text": t}));
                }
                for c in calls {
                    parts.push(json!({
                        "type": "tool_use",
                        "id": c.call_id,
                        "name": c.tool_name,
                        "input": c.input,
                    }));
                }
                push_coalesced(&mut convo, "assistant", json!(parts));
            }
            RichMessage::ToolResults { results } => {
                let parts: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        // `content` stays a bare string when there is no image,
                        // so the overwhelmingly common shape is byte-identical
                        // to what this adapter always sent — a tool result that
                        // gained an empty `images` vec must not change the
                        // request (and must not break the prompt cache).
                        let content = if r.images.is_empty() {
                            json!(r.content)
                        } else {
                            let mut blocks = vec![json!({
                                "type": "text",
                                "text": r.content,
                            })];
                            blocks.extend(r.images.iter().map(|img| {
                                json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": img.media_type,
                                        "data": img.data,
                                    },
                                })
                            }));
                            json!(blocks)
                        };
                        json!({
                            "type": "tool_result",
                            "tool_use_id": r.call_id,
                            "content": content,
                            "is_error": r.is_error,
                        })
                    })
                    .collect();
                push_coalesced(&mut convo, "user", json!(parts));
            }
            RichMessage::ImageText {
                role,
                media_type,
                data,
                text,
            } => {
                let r = if role == "agent" {
                    "assistant"
                } else {
                    role.as_str()
                };
                // Image block first (Anthropic's recommended ordering), then
                // the caption — skipped when empty so an image-only paste works.
                let mut parts = vec![json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data},
                })];
                if !text.is_empty() {
                    parts.push(json!({"type": "text", "text": text}));
                }
                push_coalesced(&mut convo, r, json!(parts));
            }
        }
    }

    let system = if system_chunks.is_empty() {
        None
    } else {
        Some(system_chunks.join("\n\n"))
    };
    (system, convo, None)
}

/// Accumulator for an Anthropic SSE response while it streams.
struct StreamAccum {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    tool_calls: Vec<crate::llm::ToolCallResult>,
    stop_reason: StopReason,
    /// The in-progress tool_use block: (id, name, partial-JSON args buffer).
    cur_tool: Option<(String, String, String)>,
}

impl Default for StreamAccum {
    fn default() -> Self {
        Self {
            text: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            cur_tool: None,
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
            acc.input_tokens = v["message"]["usage"]["input_tokens"]
                .as_u64()
                .unwrap_or(acc.input_tokens);
            None
        }
        Some("message_delta") => {
            if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                acc.stop_reason = match sr {
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
            }
            acc.output_tokens = v["usage"]["output_tokens"]
                .as_u64()
                .unwrap_or(acc.output_tokens);
            None
        }
        _ => None,
    }
}

/// Turn a fully-drained `StreamAccum` into the final result. A tool-only
/// response legitimately has empty text — only error when BOTH answer text
/// and tool calls are empty. A response that hit the max_tokens ceiling while
/// still inside a thinking block (no text or tool_use ever started) is also
/// legitimate, not malformed — let it through so task_runner's MaxTokens
/// retry path can nudge the model toward a shorter answer instead of
/// surfacing a raw protocol error.
fn finish_stream(acc: StreamAccum, model: String) -> Result<LlmResponse, LlmError> {
    if acc.text.is_empty() && acc.tool_calls.is_empty() && acc.stop_reason != StopReason::MaxTokens
    {
        return Err(LlmError::InvalidResponse("empty streamed response".into()));
    }
    Ok(LlmResponse {
        text: acc.text,
        input_tokens: acc.input_tokens,
        output_tokens: acc.output_tokens,
        model,
        tool_calls: acc.tool_calls,
        stop_reason: acc.stop_reason,
    })
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

#[async_trait]
impl LlmClient for AnthropicClient {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let (system, convo, _) = rich_messages_to_anthropic(&req.messages);

        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": convo,
        });
        if let Some(s) = system {
            body["system"] = json!(s);
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
            body["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    }))
                    .collect::<Vec<_>>()
            );
        }

        if let AnthropicAuth::ApiKey(key) = &self.auth {
            warn_if_oauth_key_misconfigured(key, &self.base_url);
        }

        let url = format!("{}/v1/messages", self.base_url);
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let resp = self
            .apply_auth(
                self.http
                    .post(url)
                    .header("anthropic-version", &self.version)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| LlmError::from_reqwest(&e))?;
        if !status.is_success() {
            tracing::warn!(status = %status, body = %body_text, "anthropic non-2xx");
            return Err(LlmError::from_status(status.as_u16(), body_text));
        }
        let v: serde_json::Value = serde_json::from_str(&body_text)
            .map_err(|e| LlmError::Http(format!("parse response: {e}")))?;

        let (text, tool_calls, stop_reason) = parse_response_body(&v)?;
        let input_tokens = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            model: self.model.clone(),
            tool_calls,
            stop_reason,
        })
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<super::StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        let (system, convo, _) = rich_messages_to_anthropic(&req.messages);
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": convo,
            "stream": true,
        });
        if let Some(s) = system {
            body["system"] = json!(s);
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
            body["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    }))
                    .collect::<Vec<_>>()
            );
        }
        if let AnthropicAuth::ApiKey(key) = &self.auth {
            warn_if_oauth_key_misconfigured(key, &self.base_url);
        }

        let url = format!("{}/v1/messages", self.base_url);
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let mut resp = self
            .apply_auth(
                self.http
                    .post(url)
                    .header("anthropic-version", &self.version)
                    .header("content-type", "application/json"),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(LlmError::from_status(status.as_u16(), body_text));
        }

        // Anthropic streams SSE: `event: <type>` + `data: {json}`. Each data
        // line carries a `type` (content_block_delta / message_start / …); we
        // parse those via `apply_sse_event` which accumulates text, tool_use
        // blocks, token counts, and stop_reason, and returns `StreamDelta`
        // chunks to forward to the sink.
        let mut buf: Vec<u8> = Vec::new();
        let mut acc = StreamAccum::default();
        while let Some(chunk) = resp.chunk().await.map_err(|e| LlmError::from_reqwest(&e))? {
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
        finish_stream(acc, self.model.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{RichMessage, ToolCallResult, ToolResultEntry};
    use serde_json::json;

    fn make_client() -> AnthropicClient {
        AnthropicClient::new(
            "http://localhost".into(),
            "test-key".into(),
            "claude-3-5-sonnet-20241022".into(),
        )
    }

    #[test]
    fn effort_is_narrowed_against_the_client_model() {
        // The client's own model decides what may be sent. The fixture client
        // above runs a model with no effort parameter, so a request asking for
        // one must produce no `output_config` at all rather than a 400.
        assert_eq!(
            supported_effort(&make_client().model, mur_common::llm::Effort::Low),
            None
        );
        // A current model takes the level as-is.
        let c = AnthropicClient::new(
            "http://localhost".into(),
            "k".into(),
            "claude-opus-5".into(),
        );
        assert_eq!(
            supported_effort(&c.model, mur_common::llm::Effort::Xhigh),
            Some(mur_common::llm::Effort::Xhigh)
        );
    }

    #[test]
    fn rich_messages_to_anthropic_text_only() {
        let msgs = vec![
            RichMessage::Text {
                role: "system".into(),
                content: "Be helpful".into(),
            },
            RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            },
        ];
        let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(sys, Some("Be helpful".to_string()));
        assert_eq!(convo.len(), 1);
        assert_eq!(convo[0]["role"], "user");
        assert_eq!(convo[0]["content"], "hi");
    }

    #[test]
    fn image_text_becomes_image_block_then_caption() {
        let msgs = vec![RichMessage::ImageText {
            role: "user".into(),
            media_type: "image/png".into(),
            data: "QkFTRTY0".into(),
            text: "what is this?".into(),
        }];
        let (_, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(convo.len(), 1);
        let content = convo[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "QkFTRTY0");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "what is this?");
    }

    #[test]
    fn image_text_without_caption_is_image_only() {
        let msgs = vec![RichMessage::ImageText {
            role: "user".into(),
            media_type: "image/png".into(),
            data: "QQ==".into(),
            text: String::new(),
        }];
        let (_, convo, _) = rich_messages_to_anthropic(&msgs);
        let content = convo[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "no empty text block");
        assert_eq!(content[0]["type"], "image");
    }

    #[test]
    fn rich_messages_tool_use_and_results() {
        let msgs = vec![
            RichMessage::Text {
                role: "user".into(),
                content: "run".into(),
            },
            RichMessage::ToolUse {
                text: Some("Running bash".into()),
                calls: vec![ToolCallResult {
                    call_id: "id1".into(),
                    tool_name: "bash".into(),
                    input: json!({"command": "echo hi"}),
                }],
            },
            RichMessage::ToolResults {
                results: vec![ToolResultEntry {
                    call_id: "id1".into(),
                    content: "hi\n".into(),
                    is_error: false,
                    status: crate::tools::ToolStatus::Ok,
                    images: Vec::new(),
                }],
            },
        ];
        let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
        assert!(sys.is_none());
        assert_eq!(convo.len(), 3);
        // assistant message with tool_use
        let asst = &convo[1];
        assert_eq!(asst["role"], "assistant");
        let content = asst["content"].as_array().unwrap();
        let has_tool_use = content.iter().any(|b| b["type"] == "tool_use");
        assert!(has_tool_use, "expected tool_use block");
        // user message with tool_result
        let result_msg = &convo[2];
        assert_eq!(result_msg["role"], "user");
        let result_content = result_msg["content"].as_array().unwrap();
        assert_eq!(result_content[0]["type"], "tool_result");
        assert_eq!(result_content[0]["tool_use_id"], "id1");
    }

    /// The whole point of the feature: an image on a tool result must reach
    /// the wire as a real `image` block inside `tool_result.content`, because
    /// that is the only shape the model can actually see.
    #[test]
    fn tool_result_image_becomes_a_wire_image_block() {
        let msgs = vec![RichMessage::ToolResults {
            results: vec![ToolResultEntry {
                call_id: "id1".into(),
                content: "[image /tmp/car.jpg — image/jpeg, 9 bytes]".into(),
                is_error: false,
                status: Default::default(),
                images: vec![crate::tools::ToolImage {
                    media_type: "image/jpeg".into(),
                    data: "QUJD".into(),
                }],
            }],
        }];
        let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
        let content = convo[0]["content"][0]["content"]
            .as_array()
            .expect("with an image, tool_result.content must be a block array, not a bare string");
        assert_eq!(content[0]["type"], "text", "text block leads");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[1]["source"]["data"], "QUJD");
    }

    /// Negative control for the test above, and a compatibility guard: with no
    /// image the request must be byte-identical to what this adapter always
    /// sent — a bare string, not a one-element block array. A silent change
    /// here would invalidate every cached prefix in the wild.
    #[test]
    fn tool_result_without_images_stays_a_bare_string() {
        let msgs = vec![RichMessage::ToolResults {
            results: vec![ToolResultEntry {
                call_id: "id1".into(),
                content: "15 degrees".into(),
                is_error: false,
                status: Default::default(),
                images: vec![],
            }],
        }];
        let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(
            convo[0]["content"][0]["content"], "15 degrees",
            "no image must mean no shape change"
        );
    }

    #[test]
    fn serializes_system_to_top_level() {
        let msgs = vec![
            RichMessage::Text {
                role: "system".into(),
                content: "Be helpful".into(),
            },
            RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            },
        ];
        let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(sys, Some("Be helpful".to_string()));
        assert_eq!(convo[0]["role"], "user");
    }

    #[test]
    fn parse_response_body_text_only() {
        let body = json!({
            "content": [{"type": "text", "text": "Done."}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "Done.");
        assert!(tool_calls.is_empty());
        assert_eq!(stop_reason, crate::llm::StopReason::EndTurn);
    }

    #[test]
    fn parse_response_body_tool_use() {
        let body = json!({
            "content": [
                {"type": "text", "text": "I'll run that."},
                {"type": "tool_use", "id": "call_1", "name": "bash", "input": {"command": "echo hi"}}
            ],
            "stop_reason": "tool_use"
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "I'll run that.");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].call_id, "call_1");
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert_eq!(stop_reason, crate::llm::StopReason::ToolUse);
    }

    #[test]
    fn rich_to_anthropic_coalesces_consecutive_user_messages() {
        use crate::llm::ToolResultEntry;
        // tool_results (user) immediately followed by an injected user steer.
        let msgs = vec![
            RichMessage::ToolResults {
                results: vec![ToolResultEntry {
                    call_id: "c1".into(),
                    content: "ok".into(),
                    is_error: false,
                    status: crate::tools::ToolStatus::Ok,
                    images: Vec::new(),
                }],
            },
            RichMessage::Text {
                role: "user".into(),
                content: "(steering) use ripgrep".into(),
            },
        ];
        let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
        // Must be ONE user message, not two (Anthropic forbids consecutive same-role).
        assert_eq!(
            convo.len(),
            1,
            "consecutive user messages must coalesce: {convo:?}"
        );
        assert_eq!(convo[0]["role"], "user");
        let content = convo[0]["content"].as_array().expect("content array");
        // tool_result block + the steering text block
        assert!(content.iter().any(|b| b["type"] == "tool_result"));
        assert!(
            content.iter().any(
                |b| b["type"] == "text" && b["text"].as_str() == Some("(steering) use ripgrep")
            )
        );
    }

    #[test]
    fn rich_to_anthropic_keeps_alternating_roles_separate() {
        let msgs = vec![
            RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            },
            RichMessage::Text {
                role: "agent".into(),
                content: "hello".into(),
            },
            RichMessage::Text {
                role: "user".into(),
                content: "bye".into(),
            },
        ];
        let (_s, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(convo.len(), 3);
        assert_eq!(convo[1]["role"], "assistant");
    }

    #[test]
    fn apply_sse_event_streams_text_and_reasoning() {
        let mut acc = StreamAccum::default();
        let d = apply_sse_event(
            &mut acc,
            &json!({
                "type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}
            }),
        );
        assert_eq!(
            d.as_ref().map(|x| (x.text.as_str(), x.thinking)),
            Some(("hello", false))
        );
        assert_eq!(acc.text, "hello");

        let d = apply_sse_event(
            &mut acc,
            &json!({
                "type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}
            }),
        );
        assert_eq!(
            d.as_ref().map(|x| (x.text.as_str(), x.thinking)),
            Some(("hmm", true))
        );
        // reasoning streamed but NOT accumulated into answer text
        assert_eq!(acc.text, "hello");
    }

    #[test]
    fn apply_sse_event_reconstructs_tool_use_and_stop_reason() {
        let mut acc = StreamAccum::default();
        apply_sse_event(
            &mut acc,
            &json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"call_1","name":"bash","input":{}}
            }),
        );
        apply_sse_event(
            &mut acc,
            &json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"command\":"}
            }),
        );
        apply_sse_event(
            &mut acc,
            &json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"\"echo hi\"}"}
            }),
        );
        apply_sse_event(&mut acc, &json!({"type":"content_block_stop","index":0}));
        apply_sse_event(
            &mut acc,
            &json!({
                "type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}
            }),
        );

        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].call_id, "call_1");
        assert_eq!(acc.tool_calls[0].tool_name, "bash");
        assert_eq!(acc.tool_calls[0].input, json!({"command":"echo hi"}));
        assert_eq!(acc.stop_reason, crate::llm::StopReason::ToolUse);
        assert_eq!(acc.output_tokens, 7);
    }

    #[test]
    fn apply_sse_event_no_arg_tool_defaults_to_empty_object() {
        let mut acc = StreamAccum::default();
        apply_sse_event(
            &mut acc,
            &json!({
                "type":"content_block_start",
                "content_block":{"type":"tool_use","id":"c","name":"now","input":{}}
            }),
        );
        // No input_json_delta events — tool has no args
        apply_sse_event(&mut acc, &json!({"type":"content_block_stop","index":0}));

        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].input, json!({}));
    }

    #[test]
    fn apply_sse_event_two_sequential_tool_blocks() {
        let mut acc = StreamAccum::default();
        // block 0: tool A
        apply_sse_event(
            &mut acc,
            &serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"a","name":"read","input":{}}
            }),
        );
        apply_sse_event(
            &mut acc,
            &serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"x\"}"}
            }),
        );
        apply_sse_event(
            &mut acc,
            &serde_json::json!({"type":"content_block_stop","index":0}),
        );
        // block 1: tool B
        apply_sse_event(
            &mut acc,
            &serde_json::json!({
                "type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"b","name":"bash","input":{}}
            }),
        );
        apply_sse_event(
            &mut acc,
            &serde_json::json!({
                "type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}
            }),
        );
        apply_sse_event(
            &mut acc,
            &serde_json::json!({"type":"content_block_stop","index":1}),
        );

        assert_eq!(acc.tool_calls.len(), 2);
        assert_eq!(acc.tool_calls[0].call_id, "a");
        assert_eq!(acc.tool_calls[0].tool_name, "read");
        assert_eq!(acc.tool_calls[0].input, serde_json::json!({"path":"x"}));
        assert_eq!(acc.tool_calls[1].call_id, "b");
        assert_eq!(acc.tool_calls[1].input, serde_json::json!({"command":"ls"}));
    }

    #[test]
    fn finish_stream_errors_on_truly_empty_response() {
        let acc = StreamAccum::default();
        let err = finish_stream(acc, "claude-x".into()).unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)));
    }

    #[test]
    fn finish_stream_allows_empty_text_when_truncated_mid_thinking() {
        // Regression: a turn that spends its whole max_tokens budget inside a
        // thinking block, before any text or tool_use block starts, must not
        // be treated as a malformed response — it should come back as a
        // (textless) MaxTokens result so task_runner can retry with guidance.
        let acc = StreamAccum {
            stop_reason: StopReason::MaxTokens,
            ..StreamAccum::default()
        };
        let resp = finish_stream(acc, "claude-x".into()).expect("should not error");
        assert_eq!(resp.text, "");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.stop_reason, StopReason::MaxTokens);
    }

    #[test]
    fn finish_stream_keeps_erroring_on_empty_text_for_normal_stop() {
        let acc = StreamAccum {
            stop_reason: StopReason::EndTurn,
            ..StreamAccum::default()
        };
        assert!(finish_stream(acc, "claude-x".into()).is_err());
    }

    fn ok_message() -> serde_json::Value {
        json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
    }

    fn hello() -> LlmRequest {
        LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            ..Default::default()
        }
    }

    /// The authless constructor sends no `x-api-key` and no `Authorization`
    /// at all. The gateway picks its mode by header *presence*: an absent
    /// header means "attach the keychain token", an empty one means
    /// "pass through untouched" — and a 401 from Anthropic.
    #[tokio::test]
    async fn authless_client_sends_no_credential_header() {
        let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
        let server = httpmock::MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/v1/messages")
                    .matches(|req| {
                        !req.headers.as_ref().is_some_and(|h| {
                            h.iter().any(|(k, _)| {
                                k.eq_ignore_ascii_case("x-api-key")
                                    || k.eq_ignore_ascii_case("authorization")
                            })
                        })
                    });
                then.status(200).json_body(ok_message());
            })
            .await;
        let client = AnthropicClient::authless_with_http(
            server.base_url(),
            "claude-opus-5".into(),
            reqwest::Client::new(),
        );
        let resp = client.generate(hello()).await.unwrap();
        assert_eq!(resp.text, "hi");
        m.assert_async().await;
    }

    /// Existing constructors are unchanged: `new` still sends the key.
    #[tokio::test]
    async fn keyed_client_still_sends_x_api_key() {
        let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
        let server = httpmock::MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/v1/messages")
                    .header("x-api-key", "test-key");
                then.status(200).json_body(ok_message());
            })
            .await;
        let client =
            AnthropicClient::new(server.base_url(), "test-key".into(), "claude-opus-5".into());
        client.generate(hello()).await.unwrap();
        m.assert_async().await;
    }
}
