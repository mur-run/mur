//! LLM client abstraction.

use async_trait::async_trait;
use mur_common::{AgentProfile, LlmMode};

pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod stub;

/// Gate function that the supervisor calls before constructing any concrete
/// LLM client. Returns `Err` when `entitlements.llm.mode = off`, which
/// declares the agent a "bridge" — an LLM-less mur agent that relays chat
/// traffic to/from the A2A bus. Bridges have no model, no API key, and the
/// supervisor must not dial a provider on their behalf.
///
/// Default `mode = Allowed` (back-compat), so this is a no-op for every
/// existing agent profile.
///
/// See `mur-common::bridge::LlmEntitlement` and Track C1 task M-c1.0.
pub fn build_client(profile: &AgentProfile) -> anyhow::Result<()> {
    if profile.entitlements.llm.mode == LlmMode::Off {
        anyhow::bail!(
            "llm.mode = off — agent '{}' is a bridge and may not call an LLM",
            profile.name
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http: {0}")]
    Http(String),
    #[error("rate limit")]
    RateLimit,
    #[error("timeout")]
    Timeout,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

/// One streamed chunk: either part of the model's hidden reasoning
/// (`thinking = true`, shown as a transient "thinking" indicator) or part of
/// the user-facing answer (`thinking = false`).
#[derive(Debug, Clone)]
pub struct StreamDelta {
    pub text: String,
    pub thinking: bool,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;
    fn model_name(&self) -> &str;

    /// Generate a reply, sending each chunk to `sink` as it arrives, and return
    /// the assembled response. The default implementation is non-streaming: it
    /// runs `generate` and emits the whole answer once, so providers without
    /// streaming still satisfy the contract.
    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        let resp = self.generate(req).await?;
        if !resp.text.is_empty() {
            let _ = sink
                .send(StreamDelta {
                    text: resp.text.clone(),
                    thinking: false,
                })
                .await;
        }
        Ok(resp)
    }
}
