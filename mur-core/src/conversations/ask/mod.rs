//! Mode C — Ask: local-only RAG with inline citations. See spec §5.
#![allow(dead_code)] // filled progressively across Tasks 19-25

use mur_common::Source;
use std::time::Duration;

pub mod prompt;
pub mod retrieve;
// Later tasks add: pub mod generate; pub mod cite; pub mod format;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    Plain,
    Json,
}

#[derive(Debug, Clone)]
pub struct Filters {
    pub source: Vec<Source>,
    pub since: Option<chrono::NaiveDate>,
    pub until: Option<chrono::NaiveDate>,
    pub min_score: f64,
}

#[derive(Debug, Clone)]
pub struct AskRequest {
    pub question: String,
    pub filters: Filters,
    pub k_summary: usize,
    pub k_raw: usize,
    pub escalation_threshold: f64,
    pub mmr_threshold: f64,
    pub model: String,
    pub format: Format,
    pub max_context_tokens: usize,
    pub response_tokens: usize,
    pub timeout: Duration,
    pub no_escalate: bool,
    pub debug_prompt: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Citation {
    pub id: u32,
    pub date: chrono::NaiveDate,
    pub source: String, // file_prefix
    pub conv_id: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HitInfo {
    pub layer: i8,
    pub source: String,
    pub conv_id: String,
    pub date: chrono::NaiveDate,
    pub score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AskResponse {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub hits_used: Vec<HitInfo>,
    pub degraded_to_mode_b: bool,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub enum AskEvent {
    Token(String),
    Citation(Citation),
    HitInfo(HitInfo),
    Done {
        tokens_in: usize,
        tokens_out: usize,
        degraded: bool,
        duration_ms: u64,
    },
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_default_shape() {
        let f = Filters {
            source: vec![],
            since: None,
            until: None,
            min_score: 0.35,
        };
        assert_eq!(f.min_score, 0.35);
    }
}
