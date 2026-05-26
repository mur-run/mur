//! Skill trace loading and clustering (M6c).
//! Shared by api-drift and coverage-gap doctor checks.

pub mod cluster;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct SkillTrace {
    pub skill_name: String,
    pub skill_version: String,
    pub outcome: TraceOutcome,
    pub timestamp: DateTime<Utc>,
    pub tools_used: Vec<String>,
    pub error: Option<String>,
    pub trace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceOutcome {
    Success,
    Failure,
    Cancelled,
}
