//! Ollama LLM client — local model inference via Ollama HTTP API.

use super::{LlmClient, LlmError, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::json;

/// Total time allowed for a single LLM request (including server think time).
const LLM_REQUEST_TIMEOUT_SECS: u64 = 60;
/// Time allowed to establish a TCP connection to the LLM endpoint.
const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;

pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String) -> Self {
        let http = reqwest::Client::builder()
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
        let messages: Vec<_> = req
            .messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();
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
        let resp = self.http.post(url).json(&body).send().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Http(e.to_string())
            }
        })?;
        if resp.status() == 429 {
            return Err(LlmError::RateLimit);
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Http(e.to_string())
            }
        })?;
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
        })
    }
}
