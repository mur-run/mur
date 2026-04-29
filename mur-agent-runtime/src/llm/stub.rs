//! Deterministic LLM provider for tests. Selected by `MUR_LLM_PROVIDER=stub`.
//!
//! See spec §6.4 — used by integration + E2E tests so we don't depend on
//! real Ollama / Anthropic during PR-time CI.

use async_trait::async_trait;
use serde::Deserialize;

use super::{LlmClient, LlmError, LlmRequest, LlmResponse};

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
            .map(|m| m.content.as_str())
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
        })
    }
    fn model_name(&self) -> &str {
        "stub"
    }
}
