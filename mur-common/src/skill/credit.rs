//! Credit ledger data types (M7c).
//!
//! `CreditEntry` is one append-only JSON line in
//! `~/.mur/agents/<agent>/credit/ledger.jsonl`. The ledger is per-agent,
//! never mutated in place, and read across peers by `cmd::skill_credit`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditKind {
    Author,
    Mutator,
    Recombiner,
    Propagator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreditEvidence {
    Author,
    Mutator {
        from_version: String,
        diff_summary: String,
    },
    Recombiner {
        role: String,
        child: String,
    },
    Propagator {
        from_agent: String,
        fitness_at_install: f64,
        samples_at_install: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditEntry {
    pub ts: DateTime<Utc>,
    pub skill: String,
    pub skill_version: String,
    pub kind: CreditKind,
    /// The crediting subject (the contributor's agent name).
    pub agent: String,
    /// Free-form evidence keyed by `kind`. `null` for `Author`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CreditEvidence>,
    /// Mirrors `EvolutionEvent.source` (e.g., `"human:alice"`, `"agent://bob"`).
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_author_entry() {
        let entry = CreditEntry {
            ts: DateTime::parse_from_rfc3339("2026-05-27T10:21:33Z")
                .unwrap()
                .with_timezone(&Utc),
            skill: "research-prices".into(),
            skill_version: "1.0.0".into(),
            kind: CreditKind::Author,
            agent: "alice".into(),
            evidence: None,
            source: "human:alice".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: CreditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn propagator_evidence_round_trips() {
        let entry = CreditEntry {
            ts: Utc::now(),
            skill: "x".into(),
            skill_version: "1.0.0".into(),
            kind: CreditKind::Propagator,
            agent: "bob".into(),
            evidence: Some(CreditEvidence::Propagator {
                from_agent: "alice".into(),
                fitness_at_install: 0.78,
                samples_at_install: 7,
            }),
            source: "agent://alice".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: CreditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn unknown_kind_returns_error() {
        let raw = r#"{"ts":"2026-05-27T10:21:33Z","skill":"x","skill_version":"1.0.0","kind":"future_kind","agent":"alice","source":"human:alice"}"#;
        let result: Result<CreditEntry, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown kind should fail to deserialize; reader code must filter at the line level");
    }
}
