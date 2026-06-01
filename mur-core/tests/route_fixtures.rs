#![allow(dead_code)]

use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::{EscalationEvent, RouteDecision, RouteTier, TaskType};

/// A registry with one local and one (priced) frontier model.
pub fn test_registry() -> ModelRegistry {
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "ollama_llama3".into(),
        ModelEntry {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
            capabilities: vec!["chat".into()],
            params: serde_json::Value::Null,
            tier: Some(RouteTier::Local),
            cost_per_1k_tokens: None,
        },
    );
    reg.models.insert(
        "anthropic_opus".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            base_url: None,
            secret: None,
            capabilities: vec!["chat".into(), "tools".into()],
            params: serde_json::Value::Null,
            tier: Some(RouteTier::Frontier),
            cost_per_1k_tokens: Some(0.015),
        },
    );
    reg
}

/// A canned local/escalation audit event for ledger tests.
pub fn make_event(escalate: bool) -> EscalationEvent {
    EscalationEvent {
        timestamp: "2026-06-01T12:00:00Z".into(),
        task_summary: "test task".into(),
        difficulty_score: if escalate { 0.82 } else { 0.15 },
        task_type: TaskType::General,
        estimated_context_tokens: 1000,
        decision: if escalate {
            RouteDecision::Escalate {
                model_id: "anthropic_opus".into(),
                reason: "high difficulty".into(),
            }
        } else {
            RouteDecision::Local {
                model_id: "ollama_llama3".into(),
                reason: "low difficulty".into(),
            }
        },
        role: None,
        escalation_from: if escalate {
            Some("ollama_llama3".into())
        } else {
            None
        },
        estimated_cost_usd: if escalate { 0.015 } else { 0.0 },
        counterfactual_cost_usd: 0.015,
    }
}
