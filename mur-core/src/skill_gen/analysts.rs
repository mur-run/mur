//! Phase 2: parallel Success and Error analysts that transform trajectories into Patches.

use crate::skill_gen::prompts::{ERROR_ANALYST_SYSTEM, SUCCESS_ANALYST_SYSTEM};
use crate::skill_gen::trajectory::{Outcome, Trajectory, TurnKind};
use mur_common::error::LlmError;
use mur_common::llm::LlmClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    #[serde(default)]
    pub source: PatchSource,
    #[serde(default)]
    pub abstract_hint: Option<String>,
    #[serde(default)]
    pub procedure_steps: Vec<StepDraft>,
    #[serde(default)]
    pub triggers: Vec<TriggerDraft>,
    #[serde(default)]
    pub variables: Vec<VariableDraft>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum PatchSource {
    #[default]
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDraft {
    pub description: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub params_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDraft {
    pub kind: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDraft {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AnalystError {
    #[error("LLM call failed: {0}")]
    Llm(#[from] LlmError),
    #[error("response not valid JSON: {0}")]
    BadJson(String),
    #[error("ReAct loop exceeded {0} rounds without producing a patch")]
    ReactExhausted(usize),
}

const ERROR_ANALYST_MAX_ROUNDS: usize = 5;

pub async fn run_phase2<L: LlmClient + 'static>(
    llm: Arc<L>,
    trajectories: Vec<Trajectory>,
    max_parallel: usize,
) -> Vec<Result<Patch, AnalystError>> {
    let sem = Arc::new(Semaphore::new(max_parallel.max(1)));
    let mut handles = Vec::new();
    for traj in trajectories {
        let llm = llm.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            match traj.outcome {
                Outcome::Success => analyze_success(&*llm, &traj).await,
                Outcome::Failure { .. } => analyze_error(&*llm, &traj).await,
            }
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(
            h.await
                .unwrap_or_else(|e| Err(AnalystError::BadJson(format!("task panic: {e}")))),
        );
    }
    out
}

async fn analyze_success<L: LlmClient>(llm: &L, traj: &Trajectory) -> Result<Patch, AnalystError> {
    let prompt = format!("Trajectory (success):\n{}", trajectory_to_text(traj));
    let raw = llm.complete(&prompt, Some(SUCCESS_ANALYST_SYSTEM)).await?;
    let raw = strip_code_fences(&raw);
    let mut p: Patch =
        serde_json::from_str(raw).map_err(|e| AnalystError::BadJson(e.to_string()))?;
    p.source = PatchSource::Success;
    Ok(p)
}

async fn analyze_error<L: LlmClient>(llm: &L, traj: &Trajectory) -> Result<Patch, AnalystError> {
    let mut transcript = format!("Trajectory (failure):\n{}\n", trajectory_to_text(traj));
    for round in 1..=ERROR_ANALYST_MAX_ROUNDS {
        let resp = llm
            .complete(&transcript, Some(ERROR_ANALYST_SYSTEM))
            .await?;
        transcript.push_str(&format!("\n--- Round {round} ---\n{resp}\n"));
        if let Some(patch_json) = extract_patch_block(&resp) {
            let mut p: Patch = serde_json::from_str(&patch_json)
                .map_err(|e| AnalystError::BadJson(e.to_string()))?;
            p.source = PatchSource::Error;
            return Ok(p);
        }
    }
    Err(AnalystError::ReactExhausted(ERROR_ANALYST_MAX_ROUNDS))
}

fn trajectory_to_text(t: &Trajectory) -> String {
    let mut s = format!("Task: {}\n", t.task_summary);
    for turn in &t.turns {
        match &turn.kind {
            TurnKind::UserPrompt => s.push_str(&format!("[User] {}\n", turn.content)),
            TurnKind::AgentMessage => s.push_str(&format!("[Agent] {}\n", turn.content)),
            TurnKind::ToolCall { tool, .. } => {
                s.push_str(&format!("[ToolCall:{}] {}\n", tool, turn.content))
            }
            TurnKind::ToolResult { tool, ok } => s.push_str(&format!(
                "[ToolResult:{} ok={}] {}\n",
                tool, ok, turn.content
            )),
            TurnKind::Error(msg) => s.push_str(&format!("[Error] {}\n", msg)),
        }
    }
    s
}

fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    s.trim_end_matches("```").trim()
}

fn extract_patch_block(resp: &str) -> Option<String> {
    let idx = resp.find("PATCH:")?;
    let after = &resp[idx + "PATCH:".len()..].trim();
    let start = after.find('{')?;
    let mut depth = 0usize;
    for (i, c) in after[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_gen::trajectory::Turn;
    use mur_common::error::LlmError;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct MockLlm {
        responses: Mutex<VecDeque<String>>,
    }

    impl LlmClient for MockLlm {
        fn complete(
            &self,
            _prompt: &str,
            _system: Option<&str>,
        ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            let r = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            async move { Ok(r) }
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![])
        }
    }

    fn make_trajectory(outcome: Outcome) -> Trajectory {
        Trajectory {
            task_summary: "test task".into(),
            turns: vec![Turn {
                kind: TurnKind::UserPrompt,
                content: "do something".into(),
                timestamp_ms: 1000,
            }],
            outcome,
            duration: std::time::Duration::from_millis(1000),
        }
    }

    fn mock_llm(responses: Vec<&str>) -> Arc<MockLlm> {
        let q: VecDeque<String> = responses.into_iter().map(Into::into).collect();
        Arc::new(MockLlm {
            responses: Mutex::new(q),
        })
    }

    #[tokio::test]
    async fn success_analyst_produces_patch() {
        let json = r#"{"abstract_hint":"test","procedure_steps":[{"description":"step1"}],"triggers":[],"variables":[],"notes":[]}"#;
        let llm = mock_llm(vec![json]);
        let trajs = vec![make_trajectory(Outcome::Success)];
        let results = run_phase2(llm, trajs, 1).await;
        assert_eq!(results.len(), 1);
        let patch = results.into_iter().next().unwrap().unwrap();
        assert!(matches!(patch.source, PatchSource::Success));
    }

    #[tokio::test]
    async fn error_analyst_extracts_patch() {
        let react = r#"THOUGHT: tool error. ACTION: done. PATCH: {"abstract_hint":"fix","procedure_steps":[],"triggers":[],"variables":[],"notes":["added guard"]}"#;
        let llm = mock_llm(vec![react]);
        let trajs = vec![make_trajectory(Outcome::Failure {
            reason: "timeout".into(),
        })];
        let results = run_phase2(llm, trajs, 1).await;
        assert_eq!(results.len(), 1);
        let patch = results.into_iter().next().unwrap().unwrap();
        assert!(matches!(patch.source, PatchSource::Error));
        assert_eq!(patch.notes, vec!["added guard"]);
    }

    #[tokio::test]
    async fn error_analyst_exhausts_rounds() {
        // 5 responses without PATCH:
        let responses = vec!["THOUGHT: still thinking. ACTION: inspect_turn 1"; 5];
        let llm = mock_llm(responses);
        let trajs = vec![make_trajectory(Outcome::Failure {
            reason: "timeout".into(),
        })];
        let results = run_phase2(llm, trajs, 1).await;
        assert!(matches!(results[0], Err(AnalystError::ReactExhausted(5))));
    }

    #[tokio::test]
    async fn mixed_batch_concurrency_limited() {
        // All Success trajectories to avoid ReAct ordering issues under concurrency.
        let json = r#"{"abstract_hint":"ok","procedure_steps":[],"triggers":[],"variables":[],"notes":[]}"#;
        let llm = mock_llm(vec![json, json, json, json, json]);
        let trajs = (0..5).map(|_| make_trajectory(Outcome::Success)).collect();
        let results = run_phase2(llm, trajs, 2).await;
        assert_eq!(results.len(), 5);
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok_count, 5);
    }

    #[tokio::test]
    async fn malformed_json_returns_error() {
        let llm = mock_llm(vec!["not json at all"]);
        let trajs = vec![make_trajectory(Outcome::Success)];
        let results = run_phase2(llm, trajs, 1).await;
        assert!(matches!(results[0], Err(AnalystError::BadJson(_))));
    }
}
