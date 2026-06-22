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

/// Legacy flat message type used by adapter internals (anthropic/openai/ollama).
/// Kept for backward compatibility while adapters are migrated to `RichMessage`.
#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResult {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResultEntry {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RichMessage {
    Text {
        role: String,
        content: String,
    },
    ToolUse {
        text: Option<String>,
        calls: Vec<ToolCallResult>,
    },
    ToolResults {
        results: Vec<ToolResultEntry>,
    },
    /// A user turn carrying an inline image (base64) plus its text caption —
    /// e.g. a screenshot pasted into `mur agent cli`. Only the Anthropic
    /// adapter renders the image today; other adapters drop it to text.
    ImageText {
        role: String,
        /// e.g. "image/png" — passed straight through to the provider.
        media_type: String,
        /// Base64-encoded image bytes (no data: prefix).
        data: String,
        text: String,
    },
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub messages: Vec<RichMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub tool_calls: Vec<ToolCallResult>,
    pub stop_reason: StopReason,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_message_text_roundtrip() {
        let m = RichMessage::Text {
            role: "user".into(),
            content: "hello".into(),
        };
        match m {
            RichMessage::Text { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content, "hello");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn llm_request_tools_defaults_empty() {
        let req = LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            temperature: None,
            max_tokens: None,
            tools: vec![],
        };
        assert!(req.tools.is_empty());
    }

    #[test]
    fn llm_response_defaults() {
        let r = LlmResponse {
            text: "hello".into(),
            input_tokens: 5,
            output_tokens: 2,
            model: "claude-3".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        };
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }
}
