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

/// Total time allowed for a single LLM request (including server think time).
const LLM_REQUEST_TIMEOUT_SECS: u64 = 60;
/// Time allowed to establish a TCP connection to the LLM endpoint.
const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Service constant used by `mur agent secret set` (mirrors agent.rs).
const MUR_AGENT_KEYCHAIN_SERVICE: &str = "mur-agent";

pub struct OpenAiClient {
    base_url: String,
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl OpenAiClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let http = crate::llm::llm_client_builder()
            .timeout(std::time::Duration::from_secs(LLM_REQUEST_TIMEOUT_SECS))
            .connect_timeout(std::time::Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            base_url,
            api_key,
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
            model,
            http,
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
        http: reqwest::Client,
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
        http: reqwest::Client,
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
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(LlmError::from_status(status.as_u16(), body_text));
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
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let mut resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated: String = body.chars().take(200).collect();
            return Err(LlmError::from_status(status.as_u16(), truncated));
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
        while let Some(chunk) = resp.chunk().await.map_err(|e| LlmError::from_reqwest(&e))? {
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
            .map(|p| crate::llm::ToolCallResult {
                call_id: p.id,
                tool_name: p.name,
                input: serde_json::from_str(&p.arguments)
                    .unwrap_or(serde_json::Value::Object(Default::default())),
            })
            .collect();
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
mod tests {
    use super::*;
    use crate::llm::{RichMessage, ToolCallResult, ToolResultEntry};
    use serde_json::json;

    #[test]
    fn rich_messages_to_openai_text_only() {
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
        let result = rich_messages_to_openai(&msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["role"], "system");
        assert_eq!(result[1]["role"], "user");
    }

    #[test]
    fn rich_messages_image_text_becomes_image_url_block() {
        let msgs = vec![RichMessage::ImageText {
            role: "user".into(),
            media_type: "image/png".into(),
            data: "QkFTRTY0".into(),
            text: "what color?".into(),
        }];
        let out = rich_messages_to_openai(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        let content = out[0]["content"].as_array().expect("multimodal array");
        assert_eq!(content[0]["type"], "image_url");
        assert_eq!(
            content[0]["image_url"]["url"],
            "data:image/png;base64,QkFTRTY0"
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "what color?");
    }

    #[test]
    fn rich_messages_image_only_omits_empty_text_block() {
        let msgs = vec![RichMessage::ImageText {
            role: "user".into(),
            media_type: "image/jpeg".into(),
            data: "QQ==".into(),
            text: String::new(),
        }];
        let out = rich_messages_to_openai(&msgs);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "no empty text block");
        assert_eq!(
            content[0]["image_url"]["url"],
            "data:image/jpeg;base64,QQ=="
        );
    }

    #[test]
    fn rich_messages_tool_use_and_results() {
        let msgs = vec![
            RichMessage::Text {
                role: "user".into(),
                content: "run it".into(),
            },
            RichMessage::ToolUse {
                text: Some("Running bash".into()),
                calls: vec![ToolCallResult {
                    call_id: "call_abc".into(),
                    tool_name: "bash".into(),
                    input: json!({"command": "echo hi"}),
                }],
            },
            RichMessage::ToolResults {
                results: vec![ToolResultEntry {
                    call_id: "call_abc".into(),
                    content: "hi\n".into(),
                    is_error: false,
                    status: crate::tools::ToolStatus::Ok,
                }],
            },
        ];
        let result = rich_messages_to_openai(&msgs);
        assert_eq!(result.len(), 3);
        // assistant message with tool_calls
        let asst = &result[1];
        assert_eq!(asst["role"], "assistant");
        let tc = &asst["tool_calls"][0];
        assert_eq!(tc["id"], "call_abc");
        assert_eq!(tc["function"]["name"], "bash");
        // tool result message
        let tool_msg = &result[2];
        assert_eq!(tool_msg["role"], "tool");
        assert_eq!(tool_msg["tool_call_id"], "call_abc");
    }

    #[test]
    fn parse_response_body_text_only() {
        let body = json!({
            "choices": [{"message": {"content": "Hello", "tool_calls": null}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "Hello");
        assert!(tool_calls.is_empty());
        assert_eq!(stop_reason, crate::llm::StopReason::EndTurn);
    }

    #[test]
    fn parse_response_body_tool_calls() {
        let args = json!({"command": "echo hi"}).to_string();
        let body = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "function": {"name": "bash", "arguments": args}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].call_id, "call_abc");
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert_eq!(stop_reason, crate::llm::StopReason::ToolUse);
    }
}
