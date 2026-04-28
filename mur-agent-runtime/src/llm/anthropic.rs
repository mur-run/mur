//! Anthropic Claude client — remote inference via Anthropic Messages API.
//!
//! POST https://api.anthropic.com/v1/messages
//!   x-api-key: $ANTHROPIC_API_KEY
//!   anthropic-version: 2023-06-01
//!   {"model": ..., "max_tokens": ..., "system": "...", "messages": [...]}
//!
//! The Anthropic API has a top-level `system` field rather than a system role
//! in `messages`. We translate `LlmMessage{role:"system"}` -> top-level system.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::json;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 1024;

pub struct AnthropicClient {
    base_url: String,
    api_key: String,
    version: String,
    model: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            base_url,
            api_key,
            version: DEFAULT_VERSION.to_string(),
            model,
            http: reqwest::Client::new(),
        }
    }

    /// Convenience constructor reading API key from `ANTHROPIC_API_KEY`.
    pub fn from_env(model: String) -> Result<Self, LlmError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| LlmError::InvalidResponse("ANTHROPIC_API_KEY not set".into()))?;
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self::new(base_url, api_key, model))
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        // Split out system messages — Anthropic puts them at the top level.
        let mut system_chunks: Vec<String> = Vec::new();
        let mut convo: Vec<serde_json::Value> = Vec::new();
        for m in &req.messages {
            if m.role == "system" {
                system_chunks.push(m.content.clone());
            } else {
                // Anthropic accepts roles "user" and "assistant" only.
                let role = if m.role == "agent" {
                    "assistant"
                } else {
                    m.role.as_str()
                };
                convo.push(json!({"role": role, "content": m.content}));
            }
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": convo,
        });
        if !system_chunks.is_empty() {
            body["system"] = json!(system_chunks.join("\n\n"));
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }

        let url = format!("{}/v1/messages", self.base_url);
        let resp = self
            .http
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let status = resp.status();
        if status == 429 {
            return Err(LlmError::RateLimit);
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;
        if !status.is_success() {
            let msg = v["error"]["message"].as_str().unwrap_or("unknown");
            return Err(LlmError::Http(format!("status {status}: {msg}")));
        }

        // Extract text from `content[0..n]` array of blocks; concatenate text blocks.
        let text = v["content"]
            .as_array()
            .ok_or_else(|| LlmError::InvalidResponse("missing content array".into()))?
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
        let input_tokens = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            model: self.model.clone(),
        })
    }
}
