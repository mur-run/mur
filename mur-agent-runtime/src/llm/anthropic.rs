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

use super::{LlmClient, LlmError, LlmRequest, LlmResponse};
use async_trait::async_trait;
use mur_common::llm::anthropic_base_url;
use serde_json::json;

const DEFAULT_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 1024;

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
        Self {
            base_url,
            api_key,
            version: DEFAULT_VERSION.to_string(),
            model,
            http: reqwest::Client::new(),
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
                &secret,
                model,
                None,
                http,
            )),
            Ok(None) => {
                let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
                    LlmError::InvalidResponse("ANTHROPIC_API_KEY not set".into())
                })?;
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

        warn_if_oauth_key_misconfigured(&self.api_key, &self.base_url);

        let url = format!("{}/v1/messages", self.base_url);
        let resp = self
            .http
            .post(url)
            .header("anthropic-version", &self.version)
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
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
