//! Adapter behaviour kind. Phase 1 only implements `PullIndex`; the
//! `FederatedQuery` variant exists so MCP adapters (P2+) compile without
//! changing the trait signature.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Adapter pulls documents into mur's vector index and answers via local search.
    PullIndex,
    /// Adapter does not hand over documents; queries are forwarded to the
    /// adapter at search time (e.g., NotebookLM via MCP).
    FederatedQuery,
}

impl SourceKind {
    /// Used by the P1.3+ orchestrator to dispatch between local search and
    /// federated query paths.
    #[allow(dead_code)]
    pub fn is_pull_index(self) -> bool {
        matches!(self, SourceKind::PullIndex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_index_is_pull_index() {
        assert!(SourceKind::PullIndex.is_pull_index());
        assert!(!SourceKind::FederatedQuery.is_pull_index());
    }

    #[test]
    fn serde_roundtrip_snake_case() {
        let s = serde_yaml::to_string(&SourceKind::PullIndex).unwrap();
        assert!(s.contains("pull_index"));
        let back: SourceKind = serde_yaml::from_str(&s).unwrap();
        assert_eq!(back, SourceKind::PullIndex);
    }
}
