use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowCandidate {
    pub id: String,
    pub title: String,
    pub suggested_name: String,
    pub steps_preview: Vec<String>,
    pub session_count: usize,
    pub evidence_session_ids: Vec<String>,
}

/// A source of workflow candidates. The harvest-proposal inbox is the v2
/// source (workflow-engine v2 P1a replaced the emergence/fingerprint miner);
/// co-occurrence can be added later without changing consumers.
pub trait CandidateSource {
    fn candidates(&self, threshold: usize) -> anyhow::Result<Vec<WorkflowCandidate>>;
}
