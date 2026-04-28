//! OpenAI-compatible client — inference via OpenAI Chat Completions API.
//!
//! POST $base_url/chat/completions
//!   Authorization: Bearer $OPENAI_API_KEY
//!   {"model": ..., "messages": [{"role":"system|user|assistant","content":"..."}], ...}
//!
//! Compatible with anything that speaks the OpenAI Chat Completions schema
//! (Together AI, Groq, Fireworks, vLLM, LM Studio, ...). The base URL is
//! settable so non-openai.com endpoints work out of the box.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::json;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiClient {
    base_url: String,
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl OpenAiClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            base_url,
            api_key,
            model,
            http: reqwest::Client::new(),
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
}

#[async_trait]
impl LlmClient for OpenAiClient {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let messages: Vec<_> = req
            .messages
            .iter()
            .map(|m| {
                // OpenAI uses {system,user,assistant}. Mur internally may use "agent".
                let role = if m.role == "agent" {
                    "assistant"
                } else {
                    m.role.as_str()
                };
                json!({"role": role, "content": m.content})
            })
            .collect();
        let mut body = json!({"model": self.model, "messages": messages});
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
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

        let text = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| LlmError::InvalidResponse("missing choices[0].message.content".into()))?
            .to_string();
        let input_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let output_tokens = v["usage"]["completion_tokens"].as_u64().unwrap_or(0);
        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            model: self.model.clone(),
        })
    }
}
