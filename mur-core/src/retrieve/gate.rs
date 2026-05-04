//! Adaptive query gate — decides whether to trigger pattern retrieval.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// 0 tokens — pure ack / greeting / noise
    Skip,
    /// Capability index only (~150-300 tokens, SessionStart layer)
    L0,
    /// 1-3 pattern snippets (~500 tokens)
    L1,
    /// Full body + linked workflows (~1500-2000 tokens)
    L2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateOutcome {
    pub tier: Tier,
    pub score: f32,
    pub reasons: Vec<&'static str>,
}

/// Evaluate whether a query should trigger pattern retrieval.
/// Stub implementation — replaced in Task 7 with composite scoring.
pub fn evaluate_query(query: &str) -> GateOutcome {
    let _ = query;
    GateOutcome { tier: Tier::L1, score: 0.5, reasons: vec![] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_ordering() {
        // Tier should support ordering: Skip < L0 < L1 < L2
        assert!(Tier::Skip < Tier::L0);
        assert!(Tier::L0 < Tier::L1);
        assert!(Tier::L1 < Tier::L2);
    }

    #[test]
    fn test_outcome_construction() {
        let o = GateOutcome { tier: Tier::L1, score: 0.62, reasons: vec!["intent: action verb"] };
        assert_eq!(o.tier, Tier::L1);
        assert!((o.score - 0.62).abs() < 1e-6);
        assert_eq!(o.reasons.len(), 1);
    }
}
