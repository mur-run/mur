//! Deterministic LLM provider for tests. Selected by `MUR_LLM_PROVIDER=stub`.
//!
//! See spec §6.4 — used by integration + E2E tests so we don't depend on
//! real Ollama / Anthropic during PR-time CI.

use async_trait::async_trait;
use serde::Deserialize;

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, RichMessage, StopReason};

#[derive(Debug, Deserialize)]
struct Scenario {
    r#match: ScenarioMatch,
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    fault: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScenarioMatch {
    contains: String,
}

const DEFAULT_SCENARIOS_YAML: &str = include_str!("stub_scenarios.yaml");

/// Deterministic LLM client driven by a small substring-match rule list.
pub struct StubLlm {
    scenarios: Vec<Scenario>,
}

impl StubLlm {
    pub fn with_default_scenarios() -> Self {
        let scenarios: Vec<Scenario> = serde_yaml_ng::from_str(DEFAULT_SCENARIOS_YAML)
            .expect("default stub scenarios YAML must be valid");
        Self { scenarios }
    }

    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml_ng::Error> {
        Ok(Self {
            scenarios: serde_yaml_ng::from_str(yaml)?,
        })
    }

    fn pick(&self, content: &str) -> Option<&Scenario> {
        self.scenarios
            .iter()
            .find(|s| content.contains(&s.r#match.contains))
    }
}

#[async_trait]
impl LlmClient for StubLlm {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let joined: String = req
            .messages
            .iter()
            .filter_map(|m| match m {
                RichMessage::Text { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = match self.pick(&joined) {
            Some(s) => {
                if s.fault.as_deref() == Some("rate_limit") {
                    return Err(LlmError::RateLimit);
                }
                s.response.clone().unwrap_or_default()
            }
            None => "[stub: no scenario matched]".to_string(),
        };
        Ok(LlmResponse {
            input_tokens: (joined.len() / 4) as u64,
            output_tokens: (text.len() / 4) as u64,
            text,
            model: "stub".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
    }
    fn model_name(&self) -> &str {
        "stub"
    }
}

/// Test stub that returns a pre-configured sequence of LlmResponse values.
/// Wraps around when exhausted.
pub struct SequenceLlm {
    responses: Vec<LlmResponse>,
    index: std::sync::atomic::AtomicUsize,
}

impl SequenceLlm {
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses,
            index: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmClient for SequenceLlm {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let idx = self.index.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.responses[idx % self.responses.len()].clone())
    }
    fn model_name(&self) -> &str {
        "sequence-stub"
    }
}
