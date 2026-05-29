use crate::capture::emergence::{EmergentCandidate, detect_emergent};
use mur_common::event::BehaviorFingerprint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowCandidate {
    pub id: String,
    pub title: String,
    pub suggested_name: String,
    pub steps_preview: Vec<String>,
    pub session_count: usize,
    pub evidence_session_ids: Vec<String>,
}

impl WorkflowCandidate {
    pub fn from_emergent(e: &EmergentCandidate) -> Self {
        let mut kw = e.keywords.clone();
        kw.sort();
        let mut h = Sha256::new();
        h.update(e.behavior.as_bytes());
        h.update([0]);
        h.update(kw.join(",").as_bytes());
        let id = format!("{:x}", h.finalize());
        Self {
            id,
            title: e.behavior.clone(),
            suggested_name: e.suggested_name.clone(),
            steps_preview: e.evidence.clone(),
            session_count: e.session_count,
            evidence_session_ids: e.session_ids.clone(),
        }
    }
}

/// A source of workflow candidates. v1 has one impl (emergence); co-occurrence
/// is added post-migration without changing consumers.
pub trait CandidateSource {
    fn candidates(&self, threshold: usize) -> anyhow::Result<Vec<WorkflowCandidate>>;
}

pub struct EmergenceSource {
    fingerprints: Vec<BehaviorFingerprint>,
}

impl EmergenceSource {
    pub fn from_fingerprints(fingerprints: Vec<BehaviorFingerprint>) -> Self {
        Self { fingerprints }
    }
    /// Load all persisted fingerprints (~/.mur/fingerprints.jsonl).
    pub fn from_disk() -> anyhow::Result<Self> {
        Ok(Self {
            fingerprints: crate::capture::emergence::load_fingerprints()?,
        })
    }
}

impl CandidateSource for EmergenceSource {
    fn candidates(&self, threshold: usize) -> anyhow::Result<Vec<WorkflowCandidate>> {
        Ok(detect_emergent(&self.fingerprints, threshold)
            .iter()
            .map(WorkflowCandidate::from_emergent)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::emergence::EmergentCandidate;

    fn ec(behavior: &str, kw: &[&str]) -> EmergentCandidate {
        EmergentCandidate {
            behavior: behavior.into(),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            session_count: 3,
            session_ids: vec!["s1".into(), "s2".into(), "s3".into()],
            evidence: vec!["ran tests".into(), "committed".into()],
            suggested_name: "test-then-commit".into(),
            suggested_content: "\u{2026}".into(),
        }
    }

    #[test]
    fn id_is_stable_and_order_independent() {
        let a = WorkflowCandidate::from_emergent(&ec("b", &["test", "commit"]));
        let b = WorkflowCandidate::from_emergent(&ec("b", &["commit", "test"]));
        assert_eq!(a.id, b.id); // keyword order must not change the id
        assert_eq!(a.session_count, 3);
        assert_eq!(a.suggested_name, "test-then-commit");
    }

    #[test]
    fn emergence_source_maps_candidates() {
        let src = EmergenceSource::from_fingerprints(vec![]); // empty -> no candidates
        assert!(src.candidates(3).unwrap().is_empty());
    }
}
