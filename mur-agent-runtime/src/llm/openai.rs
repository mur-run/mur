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
        let http = reqwest::Client::builder()
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
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout
                } else {
                    LlmError::Http(e.to_string())
                }
            })?;

        let status = resp.status();
        if status == 429 {
            return Err(LlmError::RateLimit);
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout
                } else {
                    LlmError::Http(e.to_string())
                }
            })?;
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
