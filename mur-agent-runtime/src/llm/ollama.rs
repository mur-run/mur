//! Ollama LLM client — local model inference via Ollama HTTP API.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, RichMessage, StopReason};
use async_trait::async_trait;
use serde_json::json;

/// Total time allowed for a single LLM request (including server think time).
const LLM_REQUEST_TIMEOUT_SECS: u64 = 60;
/// Time allowed to establish a TCP connection to the LLM endpoint.
const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Convert history into Ollama's `/api/chat` message array. Ollama's
/// per-message `images` field takes raw base64 with no data-URI prefix and
/// auto-detects the format, so (unlike Anthropic's typed `source.media_type`)
/// the image's mime type isn't needed here — vision-capable models served
/// through Ollama (llava, qwen2-vl, gemma3, moondream, ...) just read it.
/// Tool-calling messages are still dropped — Ollama tool support isn't wired
/// up yet.
fn to_ollama_messages(messages: &[RichMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter_map(|m| match m {
            RichMessage::Text { role, content } => Some(json!({"role": role, "content": content})),
            RichMessage::ImageText {
                role, text, data, ..
            } => Some(json!({"role": role, "content": text, "images": [data]})),
            _ => None,
        })
        .collect()
}

pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String) -> Self {
        let http = crate::llm::llm_client_builder()
            .timeout(std::time::Duration::from_secs(LLM_REQUEST_TIMEOUT_SECS))
            .connect_timeout(std::time::Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            base_url,
            model,
            http,
        }
    }

    /// Construct with a pre-built reqwest client (e.g. carrying a HostGuard DNS resolver).
    pub fn with_http_client(base_url: String, model: String, http: reqwest::Client) -> Self {
        Self {
            base_url,
            model,
            http,
        }
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/api/chat", self.base_url);
        let messages = to_ollama_messages(&req.messages);
        let mut body = json!({"model": self.model, "messages": messages, "stream": false});
        if let Some(t) = req.temperature {
            body["options"]["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["options"]["num_predict"] = json!(m);
        }
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let resp = self
            .http
            .post(url)
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
        let text = v["message"]["content"]
            .as_str()
            .ok_or_else(|| LlmError::InvalidResponse("missing message.content".into()))?
            .to_string();
        let input_tokens = v["prompt_eval_count"].as_u64().unwrap_or(0);
        let output_tokens = v["eval_count"].as_u64().unwrap_or(0);
        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            model: self.model.clone(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<super::StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/api/chat", self.base_url);
        let messages = to_ollama_messages(&req.messages);
        let mut body = json!({"model": self.model, "messages": messages, "stream": true});
        if let Some(t) = req.temperature {
            body["options"]["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["options"]["num_predict"] = json!(m);
        }
        if let Ok(parsed) = reqwest::Url::parse(&url)
            && let Err(e) = crate::sandbox::reqwest_guard::check_request_url(&parsed)
        {
            return Err(LlmError::Http(e));
        }
        let mut resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(LlmError::from_status(status.as_u16(), body_text));
        }

        // Ollama streams newline-delimited JSON objects. Read incrementally
        // (Response::chunk needs no extra reqwest features), buffer partial
        // lines, and forward each `message.content` delta to the sink.
        let mut buf: Vec<u8> = Vec::new();
        let mut text = String::new();
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        while let Some(chunk) = resp.chunk().await.map_err(|e| LlmError::from_reqwest(&e))? {
            buf.extend_from_slice(&chunk);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = &line[..line.len().saturating_sub(1)];
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                    continue;
                };
                // Thinking models (e.g. qwen3) emit reasoning in a separate
                // `thinking` field before the answer — forward it so the user
                // sees activity immediately instead of a long silent wait.
                if let Some(think) = v["message"]["thinking"].as_str()
                    && !think.is_empty()
                {
                    let _ = sink
                        .send(super::StreamDelta {
                            text: think.to_string(),
                            thinking: true,
                        })
                        .await;
                }
                if let Some(delta) = v["message"]["content"].as_str()
                    && !delta.is_empty()
                {
                    text.push_str(delta);
                    let _ = sink
                        .send(super::StreamDelta {
                            text: delta.to_string(),
                            thinking: false,
                        })
                        .await;
                }
                if v["done"].as_bool() == Some(true) {
                    input_tokens = v["prompt_eval_count"].as_u64().unwrap_or(input_tokens);
                    output_tokens = v["eval_count"].as_u64().unwrap_or(output_tokens);
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

    #[test]
    fn to_ollama_messages_plain_text() {
        let msgs = vec![RichMessage::Text {
            role: "user".into(),
            content: "hi".into(),
        }];
        let out = to_ollama_messages(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "hi");
        assert!(out[0].get("images").is_none());
    }

    #[test]
    fn to_ollama_messages_attaches_image_as_images_array() {
        let msgs = vec![RichMessage::ImageText {
            role: "user".into(),
            text: "what is this?".into(),
            media_type: "image/png".into(),
            data: "QkFTRTY0".into(),
        }];
        let out = to_ollama_messages(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "what is this?");
        assert_eq!(out[0]["images"], json!(["QkFTRTY0"]));
    }

    #[test]
    fn to_ollama_messages_drops_tool_messages() {
        // Tool-calling isn't wired for Ollama yet — filtered out rather than
        // sent in a shape the API would reject.
        let msgs = vec![RichMessage::ToolResults { results: vec![] }];
        assert_eq!(to_ollama_messages(&msgs).len(), 0);
    }
}
