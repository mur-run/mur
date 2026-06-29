use anyhow::{Context, Result};
use serde::Deserialize;
use mur_common::parallel::JudgeConfig;
use crate::conversations::backend::{ChatRequest, ChatBackend};
use super::{JudgeTask, TrackScore, rubric::build_judge_prompt};

pub struct CyclicJudge {
    pub config: JudgeConfig,
    backend: std::sync::Arc<dyn ChatBackend>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ScoreEntry {
    label: String,
    score: f32,
    reasoning: String,
}

#[derive(Deserialize)]
struct JudgeResponse {
    scores: Vec<ScoreEntry>,
}

impl CyclicJudge {
    pub fn new(config: JudgeConfig, backend: std::sync::Arc<dyn ChatBackend>) -> Self {
        Self { config, backend }
    }

    pub async fn score(&self, task: &JudgeTask) -> Result<Vec<TrackScore>> {
        let n = task.implementations.len();
        if n == 0 {
            return Ok(vec![]);
        }

        // Round 1: original order
        let order1: Vec<usize> = (0..n).collect();
        let scores1 = self.one_round(task, &order1).await?;

        // Round 2: rotate right by 1 (last becomes first)
        let mut order2: Vec<usize> = (0..n).collect();
        order2.rotate_right(1);
        let scores2 = self.one_round(task, &order2).await?;

        // Average and compute low-confidence flag
        let mut results = Vec::with_capacity(n);
        for i in 0..n {
            let track_name = task.implementations[i].track_config.name.clone();
            let s1 = scores1[i];
            let s2 = scores2[i];
            let avg = (s1 + s2) / 2.0;
            let low_confidence = (s1 - s2).abs() > 2.0; // on 0-10 scale
            results.push(TrackScore {
                track_name,
                score: avg,
                reasoning: String::new(), // filled by caller from round 1
                low_confidence,
            });
        }
        Ok(results)
    }

    async fn one_round(&self, task: &JudgeTask, order: &[usize]) -> Result<Vec<f32>> {
        let ordered_impls: Vec<(String, &str)> = order
            .iter()
            .map(|&i| {
                let imp = &task.implementations[i];
                let code = std::str::from_utf8(&imp.source).unwrap_or("");
                (imp.track_config.name.clone(), code)
            })
            .collect();

        let prompt = build_judge_prompt(&task.unit_name, &ordered_impls, &self.config.rubric);

        let response_text = self
            .backend
            .generate(ChatRequest {
                model: &self.config.model,
                system: Some("You are an expert Rust code reviewer. Score implementations on correctness, design, maintainability, and security."),
                user: &prompt,
                max_tokens: 2000,
                temperature: None,
                stop: vec![],
                cache_system: false,
                cache_user_prefix: None,
            })
            .await
            .context("LLM judge call failed")?
            .text;

        let parsed: JudgeResponse = serde_json::from_str(response_text.trim())
            .context("failed to parse judge JSON")?;

        // Map label letters back to position
        let mut scores_by_position = vec![5.0f32; order.len()]; // default mid-range
        for entry in &parsed.scores {
            if entry.label.len() == 1 {
                let pos = (entry.label.as_bytes()[0] - b'A') as usize;
                if pos < order.len() {
                    scores_by_position[pos] = entry.score;
                }
            }
        }

        // Reorder back to original track indices
        let mut out = vec![5.0f32; order.len()];
        for (pos, &original_i) in order.iter().enumerate() {
            out[original_i] = scores_by_position[pos];
        }
        Ok(out)
    }
}
