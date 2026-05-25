//! Shared coordination types (§3, §7, §8 of the coordination protocol spec).
//!
//! These are pure data types with serde support. No I/O, no validation
//! logic — that lives in [`super::plan`].

use serde::{Deserialize, Serialize};

/// SDLC phase taxonomy (§3.2).
///
/// Each microstep declares a phase. The `verify` phase is special:
/// the agent cannot self-declare success on it — the host runs the
/// Verify Gateway as a subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Gather requirements, decompose the task.
    Plan,
    /// Choose approach, sketch types/contract.
    Design,
    /// Write code / produce artifacts.
    Implement,
    /// Run tests / validate.
    Test,
    /// Verify Gateway check (host-run, not agent-run).
    Verify,
}

impl Phase {
    /// Position in the SDLC order (0 = Plan, 4 = Verify).
    /// Used to validate that phases within a step are declared in order.
    pub fn sdlc_index(self) -> u8 {
        match self {
            Phase::Plan => 0,
            Phase::Design => 1,
            Phase::Implement => 2,
            Phase::Test => 3,
            Phase::Verify => 4,
        }
    }
}

/// Determinism mode for a plan or step (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeterminismMode {
    /// Fail the step immediately on budget/turn cap violation.
    Strict,
    /// Continue past caps with a warning trace.
    #[default]
    BestEffort,
}

/// Failure category taxonomy (§8.1).
///
/// From Trace2Skill (arXiv 2603.25158), shared with mur skill spec §8.2
/// and commander P1 journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// Agent lacked domain information.
    Knowledge,
    /// Wrong tool or tool parameters.
    Tool,
    /// Instructions were ambiguous.
    Clarification,
    /// Output format mismatch (content was correct).
    Style,
    /// Transient infrastructure failure (network, rate limit, timeout).
    Transient,
    /// Verify Gateway command exited non-zero.
    VerifyFailed,
}

/// Recovery action for a failed microstep (§8.2).
///
/// Serialized as a tagged enum: `{"kind": "retry"}` or
/// `{"kind": "reroute", "reason": "tool"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Retry same agent, same step, same parameters.
    Retry,
    /// Re-route to a different capable agent.
    Reroute { reason: FailureCategory },
    /// Bubble to planner LLM for full re-planning.
    Escalate { reason: FailureCategory },
    /// Give up; emit workflow_failed.
    Abort,
}

/// Host conformance level (§2.3).
///
/// Ordered: Minimal < Standard < Full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceLevel {
    /// Plan schema + microstep journal emission.
    Minimal,
    /// Minimal + Verify Gateway + Determinism + Recovery.
    Standard,
    /// Standard + Replay + Idempotency enforcement.
    Full,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_phase_deser() {
        let phase: super::Phase = serde_json::from_str(r#""implement""#).unwrap();
        assert_eq!(phase, super::Phase::Implement);
    }

    #[test]
    fn test_phase_ordering() {
        use super::Phase;
        let ordered = [
            Phase::Plan,
            Phase::Design,
            Phase::Implement,
            Phase::Test,
            Phase::Verify,
        ];
        for (i, phase) in ordered.iter().enumerate() {
            assert_eq!(phase.sdlc_index(), i as u8);
        }
    }

    #[test]
    fn test_determinism_mode_deser() {
        let strict: super::DeterminismMode = serde_json::from_str(r#""strict""#).unwrap();
        assert_eq!(strict, super::DeterminismMode::Strict);
        let be: super::DeterminismMode = serde_json::from_str(r#""best-effort""#).unwrap();
        assert_eq!(be, super::DeterminismMode::BestEffort);
    }

    #[test]
    fn test_determinism_mode_default() {
        assert_eq!(
            super::DeterminismMode::default(),
            super::DeterminismMode::BestEffort
        );
    }

    #[test]
    fn test_failure_category_deser() {
        let cat: super::FailureCategory = serde_json::from_str(r#""knowledge""#).unwrap();
        assert_eq!(cat, super::FailureCategory::Knowledge);
        let cat: super::FailureCategory = serde_json::from_str(r#""tool""#).unwrap();
        assert_eq!(cat, super::FailureCategory::Tool);
        let cat: super::FailureCategory = serde_json::from_str(r#""verify_failed""#).unwrap();
        assert_eq!(cat, super::FailureCategory::VerifyFailed);
    }

    #[test]
    fn test_recovery_action_serde_roundtrip() {
        let action = super::RecoveryAction::Reroute {
            reason: super::FailureCategory::Knowledge,
        };
        let json = serde_json::to_string(&action).unwrap();
        let roundtripped: super::RecoveryAction = serde_json::from_str(&json).unwrap();
        match roundtripped {
            super::RecoveryAction::Reroute { reason } => {
                assert_eq!(reason, super::FailureCategory::Knowledge);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_retry_variant_no_reason() {
        let action = super::RecoveryAction::Retry;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"kind":"retry"}"#);
        let parsed: super::RecoveryAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, super::RecoveryAction::Retry));
    }

    #[test]
    fn test_conformance_level_ordering() {
        use super::ConformanceLevel;
        assert!(ConformanceLevel::Standard > ConformanceLevel::Minimal);
        assert!(ConformanceLevel::Full > ConformanceLevel::Standard);
    }
}
