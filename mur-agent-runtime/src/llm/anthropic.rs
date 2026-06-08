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
use serde_json::json;

const DEFAULT_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Total time allowed for a single LLM request (including server think time).
const LLM_REQUEST_TIMEOUT_SECS: u64 = 60;
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

pub struct AnthropicClient {
    base_url: String,
    api_key: String,
    version: String,
    model: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(LLM_REQUEST_TIMEOUT_SECS))
            .connect_timeout(std::time::Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            base_url,
            api_key,
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
            api_key,
            version: DEFAULT_VERSION.to_string(),
            model,
            http,
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
    let mut system_chunks: Vec<String> = Vec::new();
    let mut convo: Vec<serde_json::Value> = Vec::new();

    for m in msgs {
        match m {
            RichMessage::Text { role, content } => {
                if role == "system" {
                    system_chunks.push(content.clone());
                } else {
                    let r = if role == "agent" { "assistant" } else { role.as_str() };
                    convo.push(json!({"role": r, "content": content}));
                }
            }
            RichMessage::ToolUse { text, calls } => {
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if let Some(t) = text {
                    if !t.is_empty() {
                        parts.push(json!({"type": "text", "text": t}));
                    }
                }
                for c in calls {
                    parts.push(json!({
                        "type": "tool_use",
                        "id": c.call_id,
                        "name": c.tool_name,
                        "input": c.input,
                    }));
                }
                convo.push(json!({"role": "assistant", "content": parts}));
            }
            RichMessage::ToolResults { results } => {
                let parts: Vec<serde_json::Value> = results.iter().map(|r| json!({
                    "type": "tool_result",
                    "tool_use_id": r.call_id,
                    "content": r.content,
                    "is_error": r.is_error,
                })).collect();
                convo.push(json!({"role": "user", "content": parts}));
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

fn parse_response_body(
    v: &serde_json::Value,
) -> Result<(String, Vec<crate::llm::ToolCallResult>, crate::llm::StopReason), LlmError> {
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
        if !req.tools.is_empty() {
            body["tools"] = serde_json::json!(
                req.tools.iter().map(|t| serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })).collect::<Vec<_>>()
            );
        }

        warn_if_oauth_key_misconfigured(&self.api_key, &self.base_url);

        let url = format!("{}/v1/messages", self.base_url);
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let resp = self
            .http
            .post(url)
            .header("anthropic-version", &self.version)
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout
                } else {
                    LlmError::Http(e.to_string())
                }
            })?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Http(e.to_string())
            }
        })?;
        if !status.is_success() {
            tracing::warn!(status = %status, body = %body_text, "anthropic non-2xx");
            if status == 429 {
                return Err(LlmError::Http(format!("rate limit: {body_text}")));
            }
            return Err(LlmError::Http(format!("status {status}: {body_text}")));
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
        if !req.tools.is_empty() {
            body["tools"] = serde_json::json!(
                req.tools.iter().map(|t| serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })).collect::<Vec<_>>()
            );
        }
        warn_if_oauth_key_misconfigured(&self.api_key, &self.base_url);

        let url = format!("{}/v1/messages", self.base_url);
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let mut resp = self
            .http
            .post(url)
            .header("anthropic-version", &self.version)
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout
                } else {
                    LlmError::Http(e.to_string())
                }
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Http(format!("status {status}: {body_text}")));
        }

        // Anthropic streams SSE: `event: <type>` + `data: {json}`. Each data
        // line carries a `type` (content_block_delta / message_start / …); we
        // parse those and forward `text_delta` (answer) + `thinking_delta`.
        let mut buf: Vec<u8> = Vec::new();
        let mut text = String::new();
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Http(e.to_string())
            }
        })? {
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
                match v["type"].as_str() {
                    Some("content_block_delta") => {
                        let d = &v["delta"];
                        match d["type"].as_str() {
                            Some("text_delta") => {
                                if let Some(t) = d["text"].as_str()
                                    && !t.is_empty()
                                {
                                    text.push_str(t);
                                    let _ = sink
                                        .send(super::StreamDelta {
                                            text: t.to_string(),
                                            thinking: false,
                                        })
                                        .await;
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(t) = d["thinking"].as_str()
                                    && !t.is_empty()
                                {
                                    let _ = sink
                                        .send(super::StreamDelta {
                                            text: t.to_string(),
                                            thinking: true,
                                        })
                                        .await;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("message_start") => {
                        input_tokens = v["message"]["usage"]["input_tokens"]
                            .as_u64()
                            .unwrap_or(input_tokens);
                    }
                    Some("message_delta") => {
                        output_tokens = v["usage"]["output_tokens"]
                            .as_u64()
                            .unwrap_or(output_tokens);
                    }
                    _ => {}
                }
            }
        }
        if text.is_empty() {
            return Err(LlmError::InvalidResponse("empty streamed response".into()));
        }
        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            model: self.model.clone(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
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
    fn rich_messages_to_anthropic_text_only() {
        let msgs = vec![
            RichMessage::Text { role: "system".into(), content: "Be helpful".into() },
            RichMessage::Text { role: "user".into(), content: "hi".into() },
        ];
        let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(sys, Some("Be helpful".to_string()));
        assert_eq!(convo.len(), 1);
        assert_eq!(convo[0]["role"], "user");
        assert_eq!(convo[0]["content"], "hi");
    }

    #[test]
    fn rich_messages_tool_use_and_results() {
        let msgs = vec![
            RichMessage::Text { role: "user".into(), content: "run".into() },
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

    #[test]
    fn serializes_system_to_top_level() {
        let msgs = vec![
            RichMessage::Text { role: "system".into(), content: "Be helpful".into() },
            RichMessage::Text { role: "user".into(), content: "hi".into() },
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
}
