//! Route data types for the cost-router orchestrator.
//!
//! These types are shared between `mur-core` (router logic) and
//! `mur-agent-runtime` (future Phase 2 spawn decisions).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Whether a model is cheap/local or frontier/expensive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTier {
    /// Free or near-free local model (Ollama, llama.cpp, mlx).
    Local,
    /// Paid cloud frontier model (Claude, GPT, Gemini).
    Frontier,
}

impl Serialize for RouteTier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            RouteTier::Local => "local",
            RouteTier::Frontier => "frontier",
        })
    }
}

impl<'de> Deserialize<'de> for RouteTier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "local" => Ok(RouteTier::Local),
            "frontier" => Ok(RouteTier::Frontier),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["local", "frontier"],
            )),
        }
    }
}

/// Which model and tier to use for a sub-task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteDecision {
    /// Route to a cheap/local model.
    Local {
        model_id: String,
        reason: String,
    },
    /// Escalate to a frontier model.
    Escalate {
        model_id: String,
        reason: String,
    },
}

impl fmt::Display for RouteDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteDecision::Local { model_id, reason } => {
                write!(f, "local → {model_id} ({reason})")
            }
            RouteDecision::Escalate { model_id, reason } => {
                write!(f, "escalate → {model_id} ({reason})")
            }
        }
    }
}

/// Categories of sub-tasks for difficulty scoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Writing or modifying code.
    CodeGen,
    /// Reviewing / auditing existing code.
    CodeReview,
    /// Searching / retrieving information.
    Retrieval,
    /// Refactoring across multiple files.
    Refactor,
    /// Writing or updating documentation.
    Documentation,
    /// Debugging / investigating issues.
    Debugging,
    /// Running tests or commands.
    Execution,
    /// General chat / Q&A.
    General,
}

impl TaskType {
    /// Base difficulty score 0.0–1.0 before other factors.
    pub fn base_difficulty(&self) -> f64 {
        match self {
            TaskType::CodeGen => 0.65,
            TaskType::Refactor => 0.70,
            TaskType::Debugging => 0.60,
            TaskType::CodeReview => 0.55,
            TaskType::Retrieval => 0.30,
            TaskType::Documentation => 0.35,
            TaskType::Execution => 0.25,
            TaskType::General => 0.40,
        }
    }
}

/// One routing decision recorded in the escalation audit ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationEvent {
    /// ISO-8601 timestamp.
    pub timestamp: String,
    /// Human-readable summary of what was asked.
    pub task_summary: String,
    /// 0.0–1.0 difficulty score from the heuristic.
    pub difficulty_score: f64,
    /// Classified task type.
    pub task_type: TaskType,
    /// Estimated context window tokens needed.
    pub estimated_context_tokens: u64,
    /// The routing decision made.
    pub decision: RouteDecision,
    /// Role that originated this task (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Which local model would have been used if not escalated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_from: Option<String>,
    /// Estimated USD cost of the route actually taken (0.0 for local).
    #[serde(default)]
    pub estimated_cost_usd: f64,
    /// Estimated USD this task would have cost on the frontier model —
    /// i.e. the cost avoided when routed local. Equals `estimated_cost_usd`
    /// for escalations.
    #[serde(default)]
    pub counterfactual_cost_usd: f64,
}

/// Per-role routing override, stored in `RoleEntry.route_policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutePolicy {
    /// Let the heuristic decide (default).
    Auto,
    /// Bias toward local models; only escalate above a higher threshold.
    PreferLocal,
    /// Always use local models for this role.
    ForceLocal,
    /// Always use a specific frontier model for this role.
    ForceFrontier {
        /// Model registry key (e.g. "anthropic_opus_4_7").
        model_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_tier_serializes_as_lowercase() {
        let local = RouteTier::Local;
        let json = serde_json::to_string(&local).unwrap();
        assert_eq!(json, r#""local""#);

        let frontier = RouteTier::Frontier;
        let json = serde_json::to_string(&frontier).unwrap();
        assert_eq!(json, r#""frontier""#);
    }

    #[test]
    fn route_tier_deserializes_case_insensitive() {
        let tier: RouteTier = serde_json::from_str(r#""local""#).unwrap();
        assert_eq!(tier, RouteTier::Local);

        let tier: RouteTier = serde_json::from_str(r#""LOCAL""#).unwrap();
        assert_eq!(tier, RouteTier::Local);

        let tier: RouteTier = serde_json::from_str(r#""Frontier""#).unwrap();
        assert_eq!(tier, RouteTier::Frontier);
    }

    #[test]
    fn route_decision_display() {
        let d = RouteDecision::Local {
            model_id: "ollama_llama3".into(),
            reason: "low difficulty (score 0.15)".into(),
        };
        let s = d.to_string();
        assert!(s.contains("local"));
        assert!(s.contains("ollama_llama3"));

        let d = RouteDecision::Escalate {
            model_id: "anthropic_opus_4_7".into(),
            reason: "high complexity (score 0.82)".into(),
        };
        let s = d.to_string();
        assert!(s.contains("escalate"));
        assert!(s.contains("anthropic_opus_4_7"));
    }

    #[test]
    fn escalation_event_roundtrip() {
        let event = EscalationEvent {
            timestamp: "2026-06-01T12:00:00Z".into(),
            task_summary: "Refactor auth module".into(),
            difficulty_score: 0.82,
            task_type: TaskType::CodeGen,
            estimated_context_tokens: 3500,
            decision: RouteDecision::Escalate {
                model_id: "anthropic_opus_4_7".into(),
                reason: "high complexity".into(),
            },
            role: Some("dev".into()),
            escalation_from: Some("ollama_llama3".into()),
            estimated_cost_usd: 0.0525,
            counterfactual_cost_usd: 0.0525,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EscalationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_summary, "Refactor auth module");
        assert_eq!(parsed.difficulty_score, 0.82);
        assert_eq!(parsed.task_type, TaskType::CodeGen);
        assert_eq!(parsed.counterfactual_cost_usd, 0.0525);
        assert!(matches!(
            parsed.decision,
            RouteDecision::Escalate { .. }
        ));
    }

    #[test]
    fn route_policy_serde_roundtrip() {
        let policy = RoutePolicy::Auto;
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, RoutePolicy::Auto);

        let policy = RoutePolicy::PreferLocal;
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, RoutePolicy::PreferLocal);

        let policy = RoutePolicy::ForceFrontier {
            model_id: "claude".into(),
        };
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            parsed,
            RoutePolicy::ForceFrontier {
                model_id: "claude".into()
            }
        );
    }

    #[test]
    fn task_type_base_difficulty_ordering() {
        // Execution and retrieval should be easiest.
        assert!(TaskType::Execution.base_difficulty() < TaskType::CodeGen.base_difficulty());
        assert!(TaskType::Retrieval.base_difficulty() < TaskType::CodeGen.base_difficulty());
        // Refactor should be hardest.
        assert!(TaskType::Refactor.base_difficulty() > TaskType::Documentation.base_difficulty());
        // All scores in [0.0, 1.0].
        let all = [
            TaskType::CodeGen,
            TaskType::CodeReview,
            TaskType::Retrieval,
            TaskType::Refactor,
            TaskType::Documentation,
            TaskType::Debugging,
            TaskType::Execution,
            TaskType::General,
        ];
        for tt in &all {
            let s = tt.base_difficulty();
            assert!((0.0..=1.0).contains(&s), "{tt:?} base_difficulty {s} out of range");
        }
    }
}
