//! Candidate source backed by the W2 harvest-proposal inbox — replaces the
//! emergence/fingerprint miner (workflow-engine v2 P1a; ambient-capture spec §3.2).

use anyhow::Result;
use std::path::PathBuf;

use super::candidate::{CandidateSource, WorkflowCandidate};
use crate::harvest::proposal::{self, Proposal};

pub struct HarvestProposalSource {
    inbox_dir: PathBuf,
}

impl HarvestProposalSource {
    pub fn new(inbox_dir: PathBuf) -> Self {
        Self { inbox_dir }
    }

    pub fn default_source() -> Self {
        Self::new(proposal::inbox_dir())
    }
}

fn to_candidate(p: &Proposal) -> WorkflowCandidate {
    WorkflowCandidate {
        id: format!("harvest:{}", p.id),
        title: p.title.clone(),
        suggested_name: p.suggested_name.clone(),
        steps_preview: p.steps.iter().take(5).cloned().collect(),
        session_count: 1,
        evidence_session_ids: vec![p.id.clone()],
    }
}

impl CandidateSource for HarvestProposalSource {
    fn candidates(&self, _threshold: usize) -> Result<Vec<WorkflowCandidate>> {
        Ok(proposal::pending_in_dir(&self.inbox_dir)?
            .iter()
            .map(to_candidate)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::proposal::{Proposal, ProposalStatus, save_in_dir};

    #[test]
    fn maps_pending_proposals_to_candidates() {
        let tmp = tempfile::TempDir::new().unwrap();
        save_in_dir(
            tmp.path(),
            &Proposal {
                id: "s1".into(),
                project: None,
                title: "Deploy api".into(),
                suggested_name: "deploy-api".into(),
                steps: vec!["cargo build".into()],
                skeleton: vec!["cargo build".into()],
                event_count: 5,
                duration_secs: 60,
                occurrences: 2,
                created_at: "2026-06-11T00:00:00Z".into(),
                status: ProposalStatus::Pending,
                similar_to: None,
            },
        )
        .unwrap();
        let src = HarvestProposalSource::new(tmp.path().to_path_buf());
        let cands = src.candidates(3).unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].id, "harvest:s1");
        assert_eq!(cands[0].suggested_name, "deploy-api");
    }

    #[test]
    fn empty_inbox_yields_no_candidates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = HarvestProposalSource::new(tmp.path().to_path_buf());
        assert!(src.candidates(3).unwrap().is_empty());
    }
}
