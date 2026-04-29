//! Anthropic Claude client — remote inference via Anthropic Messages API.
//!
//! POST https://api.anthropic.com/v1/messages
//!   x-api-key: $ANTHROPIC_API_KEY        (regular API key, sk-ant-api03-*)
//!   Authorization: Bearer $ANTHROPIC_API_KEY  (OAuth token, sk-ant-oat01-*)
//!   anthropic-version: 2023-06-01
//!   anthropic-beta: claude-code-20250219,oauth-2025-04-20,...  (OAuth only)
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

/// Beta flags required when authenticating with a Claude subscription
/// OAuth token (sk-ant-oat01-*) instead of a console API key.
const OAUTH_BETAS: &str =
    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,compact-2026-01-12";

/// Billing identifier prepended to the system prompt on OAuth requests.
/// Required for Anthropic to accept the call as Claude Code-shaped;
/// without it the OAuth path returns 429 rate_limit_error immediately.
/// Mirrors the same constant in mur-commander.
const OAUTH_BILLING_HEADER: &str =
    "x-anthropic-billing-header: cc_version=2.1.77; cc_entrypoint=sdk-cli;";

fn is_oauth_token(key: &str) -> bool {
    key.contains("sk-ant-oat")
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
        let oauth = is_oauth_token(&self.api_key);
        let system_text = if oauth {
            let mut chunks = vec![OAUTH_BILLING_HEADER.to_string()];
            chunks.extend(system_chunks);
            Some(chunks.join("\n\n"))
        } else if !system_chunks.is_empty() {
            Some(system_chunks.join("\n\n"))
        } else {
            None
        };
        if let Some(s) = system_text {
            body["system"] = json!(s);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }

        let url = format!("{}/v1/messages", self.base_url);
        let mut builder = self
            .http
            .post(url)
            .header("anthropic-version", &self.version)
            .header("content-type", "application/json");
        builder = if is_oauth_token(&self.api_key) {
            builder
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("anthropic-beta", OAUTH_BETAS)
        } else {
            builder.header("x-api-key", &self.api_key)
        };
        let resp = builder
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;
        if !status.is_success() {
            tracing::warn!(status = %status, body = %body_text, "anthropic non-2xx");
            if status == 429 {
                return Err(LlmError::Http(format!("rate limit: {body_text}")));
            }
            return Err(LlmError::Http(format!("status {status}: {body_text}")));
        }
        let v: serde_json::Value = serde_json::from_str(&body_text)
            .map_err(|e| LlmError::Http(format!("parse response: {e}")))?;

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
