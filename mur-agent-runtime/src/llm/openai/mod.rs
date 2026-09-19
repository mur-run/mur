//! OpenAI-compatible client — inference via OpenAI Chat Completions API.
//!
//! POST $base_url/chat/completions
//!   Authorization: Bearer $OPENAI_API_KEY
//!   {"model": ..., "messages": [{"role":"system|user|assistant","content":"..."}], ...}
//!
//! Compatible with anything that speaks the OpenAI Chat Completions schema
//! (Together AI, Groq, Fireworks, vLLM, LM Studio, ...). The base URL is
//! settable so non-openai.com endpoints work out of the box.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, RichMessage, StopReason};
use async_trait::async_trait;
use serde_json::json;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Translate only documented, fixture-backed OpenAI-compatible error codes.
/// Unknown provider extensions stay `Rejected` and therefore stop fleet-wide.
pub(crate) fn map_openai_error(status: u16, body: &str) -> LlmError {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return LlmError::from_status(status, body.to_string()),
    };
    let error = &parsed["error"];
    let code = error["code"].as_str().unwrap_or_default();
    let message = error["message"].as_str().unwrap_or(body).to_string();
    match code {
        "context_length_exceeded" => LlmError::ContextExceeded(message),
        "model_not_found" | "model_unavailable" | "model_not_available" => {
            LlmError::ModelNotFound(message)
        }
        "insufficient_quota" => LlmError::InsufficientCredit,
        "content_policy_violation" | "safety_policy_violation" => {
            LlmError::SafetyPolicyRejected(message)
        }
        "permission_denied" | "insufficient_permissions" => {
            LlmError::PermissionDenied(status, message)
        }
        _ => LlmError::from_status(status, body.to_string()),
    }
}

/// Service constant used by `mur agent secret set` (mirrors agent.rs).
const MUR_AGENT_KEYCHAIN_SERVICE: &str = "mur-agent";

/// How a request authenticates. Explicit so an authless route is a
/// deliberate choice at construction, not an empty key that happens to be
/// sent as `Bearer `. `None` exists for the loopback Codex gateway, which
/// attaches the ChatGPT OAuth token itself.
#[derive(Clone)]
enum OpenAiAuth {
    Bearer(String),
    None,
}

pub struct OpenAiClient {
    base_url: String,
    auth: OpenAiAuth,
    model: String,
    http: crate::sandbox::reqwest_guard::GuardedHttpClient,
}

impl OpenAiClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let http = crate::sandbox::reqwest_guard::GuardedHttpClient::unrestricted(
            crate::llm::llm_client_builder(),
        )
        .expect("failed to build guarded reqwest client");
        Self {
            base_url,
            auth: OpenAiAuth::Bearer(api_key),
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
            auth: OpenAiAuth::Bearer(api_key),
            model,
            http,
        }
    }

    /// Chat Completions transport that sends no credential at all. Only the
    /// loopback Codex gateway route may use this (see `llm::codex`), which is
    /// why it is crate-private: the gateway owns the OAuth token, and a key
    /// here would either leak or silently switch the bill to OpenAI Platform.
    pub(crate) fn authless_with_http(
        base_url: String,
        model: String,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Self {
        Self {
            base_url,
            auth: OpenAiAuth::None,
            model,
            http,
        }
    }

    fn apply_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            OpenAiAuth::Bearer(key) => request.bearer_auth(key),
            OpenAiAuth::None => request,
        }
    }

    /// Convenience constructor reading API key from `OPENAI_API_KEY` and base
    /// URL from `OPENAI_BASE_URL` (defaults to api.openai.com/v1).
    pub fn from_env(model: String) -> Result<Self, LlmError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| LlmError::InvalidResponse("OPENAI_API_KEY not set".into()))?;
        let base_url =
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self::new(base_url, api_key, model))
    }

    /// Construct from a resolved SecretString and an optional registry base URL.
    pub fn from_secret_string(
        key: &secrecy::SecretString,
        model: String,
        base_url: Option<String>,
    ) -> Self {
        use secrecy::ExposeSecret;
        let base = base_url.unwrap_or_else(|| {
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
        });
        Self::new(base, key.expose_secret().to_string(), model)
    }

    /// Mur's agent-aware credential resolution, symmetric with
    /// [`super::anthropic::AnthropicClient::from_agent_credentials`]. Keychain
    /// at `mur-agent/{agent}/OPENAI_API_KEY` wins over the `OPENAI_API_KEY`
    /// env var; backend errors propagate rather than silently falling through.
    pub async fn from_agent_credentials(agent_name: &str, model: String) -> Result<Self, LlmError> {
        let account = format!("{agent_name}/OPENAI_API_KEY");
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

    /// Like [`from_secret_string`] but uses a pre-built reqwest client
    /// (e.g. one carrying a B1 HostGuard DNS resolver).
    pub fn from_secret_string_with_http(
        key: &secrecy::SecretString,
        model: String,
        base_url: Option<String>,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Self {
        use secrecy::ExposeSecret;
        let base = base_url.unwrap_or_else(|| {
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
        });
        Self::new_with_http_client(base, key.expose_secret().to_string(), model, http)
    }

    /// Like [`from_agent_credentials`] but injects a pre-built reqwest client
    /// (e.g. one carrying a B1 HostGuard DNS resolver).
    pub async fn from_agent_credentials_with_http(
        agent_name: &str,
        model: String,
        http: crate::sandbox::reqwest_guard::GuardedHttpClient,
    ) -> Result<Self, LlmError> {
        let account = format!("{agent_name}/OPENAI_API_KEY");
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
                let api_key = std::env::var("OPENAI_API_KEY")
                    .map_err(|_| LlmError::InvalidResponse("OPENAI_API_KEY not set".into()))?;
                let base = std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
                Ok(Self::new_with_http_client(base, api_key, model, http))
            }
            Err(e) => Err(LlmError::InvalidResponse(format!(
                "keychain backend error reading {MUR_AGENT_KEYCHAIN_SERVICE}/{account}: {e}"
            ))),
        }
    }
}

fn rich_messages_to_openai(msgs: &[RichMessage]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    for m in msgs {
        match m {
            RichMessage::Text { role, content } => {
                let r = if role == "agent" {
                    "assistant"
                } else {
                    role.as_str()
                };
                result.push(json!({"role": r, "content": content}));
            }
            RichMessage::ToolUse { text, calls } => {
                let tool_calls: Vec<serde_json::Value> = calls
                    .iter()
                    .map(|c| {
                        let args = serde_json::to_string(&c.input).unwrap_or_default();
                        json!({
                            "id": c.call_id,
                            "type": "function",
                            "function": {"name": c.tool_name, "arguments": args},
                        })
                    })
                    .collect();
                let mut msg = json!({"role": "assistant", "tool_calls": tool_calls});
                if let Some(t) = text
                    && !t.is_empty()
                {
                    msg["content"] = json!(t);
                }
                result.push(msg);
            }
            RichMessage::ToolResults { results } => {
                for r in results {
                    // `r.images` is dropped here, deliberately. The OpenAI
                    // `role: "tool"` message takes a plain string — the
                    // multimodal `image_url` block is only valid on a user
                    // message, so there is no in-protocol place to put a tool's
                    // image. The tool's text still describes it (`[image …]`),
                    // which is why the placeholder in `render_mcp_result` and
                    // `read_file` is text and not an empty string. To give an
                    // OpenAI-backed agent real vision, send the image as a user
                    // turn (`RichMessage::ImageText`, handled below) instead.
                    result.push(json!({
                        "role": "tool",
                        "tool_call_id": r.call_id,
                        "content": r.content,
                    }));
                }
            }
            // OpenAI vision: emit the image as a data-URL `image_url` content
            // block (the OpenAI-compatible multimodal shape deepseek / LM Studio
            // / Ollama's OpenAI endpoint all accept), plus the caption when
            // present. A non-vision backend now errors loudly instead of the
            // image being silently dropped.
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
                let mut parts = vec![json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{media_type};base64,{data}") },
                })];
                if !text.is_empty() {
                    parts.push(json!({"type": "text", "text": text}));
                }
                result.push(json!({"role": r, "content": parts}));
            }
            RichMessage::TurnLedger { turn, memory } => {
                result.push(json!({
                    "role": "user",
                    "content": crate::turn_ledger::render_memory(*turn, memory),
                }));
            }
        }
    }
    result
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
    let choice = &v["choices"][0];
    let msg = &choice["message"];
    let text = msg["content"].as_str().unwrap_or("").to_string();

    let tool_calls: Vec<ToolCallResult> = msg["tool_calls"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let call_id = tc["id"].as_str()?.to_string();
                    let tool_name = tc["function"]["name"].as_str()?.to_string();
                    let input: serde_json::Value = tc["function"]["arguments"]
                        .as_str()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    Some(ToolCallResult {
                        call_id,
                        tool_name,
                        input,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let stop_reason = match choice["finish_reason"].as_str() {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };

    Ok((text, tool_calls, stop_reason))
}

#[async_trait]
impl LlmClient for OpenAiClient {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let messages = rich_messages_to_openai(&req.messages);
        let mut body = json!({"model": self.model, "messages": messages});
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        // Effort, narrowed to what this model family understands. Absent for
        // local and non-reasoning models rather than sent and ignored.
        if let Some(e) = req
            .effort
            .and_then(|e| mur_common::llm::openai_reasoning_effort(&self.model, e))
        {
            body["reasoning_effort"] = json!(e);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if !req.tools.is_empty() {
            body["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(|t| json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .apply_auth(self.http.post(&url).map_err(LlmError::Http)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(map_openai_error(status.as_u16(), &body_text));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| LlmError::from_reqwest(&e))?;

        let (text, tool_calls, stop_reason) = parse_response_body(&v)?;
        let input_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let output_tokens = v["usage"]["completion_tokens"].as_u64().unwrap_or(0);
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
        let messages = rich_messages_to_openai(&req.messages);
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        // Effort, narrowed to what this model family understands. Absent for
        // local and non-reasoning models rather than sent and ignored.
        if let Some(e) = req
            .effort
            .and_then(|e| mur_common::llm::openai_reasoning_effort(&self.model, e))
        {
            body["reasoning_effort"] = json!(e);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if !req.tools.is_empty() {
            body["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(|t| json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut resp = self
            .apply_auth(self.http.post(&url).map_err(LlmError::Http)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_openai_error(status.as_u16(), &body));
        }

        // OpenAI streams Server-Sent Events: `data: {json}\n\n`, ending with
        // `data: [DONE]`. Read incrementally (Response::chunk needs no extra
        // reqwest features), buffer partial lines, forward content + reasoning.
        let mut buf: Vec<u8> = Vec::new();
        let mut text = String::new();
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        let mut stop_reason = StopReason::EndTurn;
        // Streamed tool calls arrive as fragments: `id` and `function.name`
        // usually land once, `function.arguments` is concatenated across any
        // number of chunks, and `index` is what ties the fragments of one call
        // together (a turn can open several calls at once). Accumulate by index
        // and assemble after the stream closes — reading only the final chunk
        // loses every call, which is how a model's tool call used to vanish and
        // its narration got returned as the answer (#938).
        #[derive(Default)]
        struct PartialToolCall {
            id: String,
            name: String,
            arguments: String,
        }
        let mut partial: std::collections::BTreeMap<u64, PartialToolCall> = Default::default();
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
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                let delta = &v["choices"][0]["delta"];
                // Reasoning models (DeepSeek-R1 etc.) put thinking in a separate
                // field — forward it so the UI can show a "thinking" trace.
                if let Some(r) = delta["reasoning_content"]
                    .as_str()
                    .or_else(|| delta["reasoning"].as_str())
                    && !r.is_empty()
                {
                    let _ = sink
                        .send(super::StreamDelta {
                            text: r.to_string(),
                            thinking: true,
                        })
                        .await;
                }
                if let Some(c) = delta["content"].as_str()
                    && !c.is_empty()
                {
                    text.push_str(c);
                    let _ = sink
                        .send(super::StreamDelta {
                            text: c.to_string(),
                            thinking: false,
                        })
                        .await;
                }
                if let Some(calls) = delta["tool_calls"].as_array() {
                    for tc in calls {
                        let slot = partial
                            .entry(tc["index"].as_u64().unwrap_or(0))
                            .or_default();
                        // Later fragments carry only `arguments`; never let an
                        // absent or empty field clobber what an earlier chunk
                        // already established.
                        if let Some(id) = tc["id"].as_str()
                            && !id.is_empty()
                        {
                            slot.id = id.to_string();
                        }
                        if let Some(name) = tc["function"]["name"].as_str()
                            && !name.is_empty()
                        {
                            slot.name = name.to_string();
                        }
                        if let Some(args) = tc["function"]["arguments"].as_str() {
                            slot.arguments.push_str(args);
                        }
                    }
                }
                // The final content chunk carries `finish_reason`; surface a
                // max_tokens cut (`"length"`) so the caller can mark the reply
                // as truncated instead of passing it off as complete.
                if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
                    stop_reason = match fr {
                        "length" => StopReason::MaxTokens,
                        "tool_calls" => StopReason::ToolUse,
                        _ => StopReason::EndTurn,
                    };
                }
                if v["usage"].is_object() {
                    input_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(input_tokens);
                    output_tokens = v["usage"]["completion_tokens"]
                        .as_u64()
                        .unwrap_or(output_tokens);
                }
            }
        }
        // A fragment set with no name never became a usable call (a server that
        // opened an index and then said nothing more about it); dropping it is
        // safer than inventing a nameless tool.
        let tool_calls: Vec<crate::llm::ToolCallResult> = partial
            .into_values()
            .filter(|p| !p.name.is_empty())
            .filter_map(|p| {
                let input = if p.arguments.trim().is_empty() {
                    // A no-argument call legitimately sends "".
                    serde_json::Value::Object(Default::default())
                } else {
                    match serde_json::from_str(&p.arguments) {
                        Ok(v) => v,
                        // Unparseable arguments on an INTERRUPTED stream mean
                        // the fragments stopped mid-JSON. Dropping the call
                        // tells the model plainly that it did not happen;
                        // substituting `{}` here would run a tool with
                        // arguments the model never chose.
                        Err(_) if interrupted => return None,
                        // On a normal end this stays exactly as it was: a
                        // server that sent malformed arguments gets the empty
                        // object it always got.
                        Err(_) => serde_json::Value::Object(Default::default()),
                    }
                };
                Some(crate::llm::ToolCallResult {
                    call_id: p.id,
                    tool_name: p.name,
                    input,
                })
            })
            .collect();
        if interrupted {
            if text.is_empty() && tool_calls.is_empty() {
                return Err(LlmError::Timeout);
            }
            stop_reason = StopReason::Interrupted;
        }
        // A turn that goes straight to a tool call carries no text at all, and
        // that is a complete, correct response — only a turn with neither text
        // nor calls is the blank reply this guard exists to catch.
        if text.is_empty() && tool_calls.is_empty() {
            return Err(LlmError::InvalidResponse("empty streamed response".into()));
        }
        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            model: self.model.clone(),
            tool_calls,
            stop_reason,
        })
    }
}

#[cfg(test)]
mod tests;
