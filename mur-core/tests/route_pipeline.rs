//! End-to-end test: model registry → router → escalation ledger.
//!
//! Simulates the complete Phase 1 flow: register tiered models, configure
//! a role with routing overrides, route several tasks, and verify the
//! escalation ledger captures decisions + cost savings correctly.

mod route_fixtures;
use mur_common::model::RoleEntry;
use mur_common::route::{RouteDecision, RoutePolicy, TaskType};
use mur_core::route::ledger::EscalationLedger;
use mur_core::route::Router;
use route_fixtures::test_registry;
use tempfile::TempDir;

fn setup_registry_with_roles() -> mur_common::model::ModelRegistry {
    let mut reg = test_registry();
    reg.roles.insert(
        "dev".into(),
        RoleEntry {
            primary: "anthropic_opus".into(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: Some(RoutePolicy::ForceFrontier {
                model_id: "anthropic_opus".into(),
            }),
        },
    );
    reg.roles.insert(
        "reflector".into(),
        RoleEntry {
            primary: "ollama_llama3".into(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: Some(RoutePolicy::ForceLocal),
        },
    );
    reg
}

#[test]
fn full_pipeline_routes_records_and_tracks_cost() {
    let reg = setup_registry_with_roles();
    let router = Router::new(reg).unwrap();
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();

    let tasks = vec![
        ("Run unit tests", TaskType::Execution, 200_u64, Some("dev")),
        ("Summarize chat history", TaskType::General, 1500, Some("reflector")),
        ("Add a docstring", TaskType::Documentation, 300, None),
        ("Refactor auth module", TaskType::Refactor, 8000, None),
        ("Fix typo in README", TaskType::Documentation, 100, Some("reflector")),
    ];

    let mut local_count = 0;
    let mut escalate_count = 0;

    for (summary, task_type, tokens, role) in &tasks {
        // Use audit() for proper cost-tracking from the start.
        let event = router.audit(summary, *task_type, *tokens, *role, "2026-06-01T12:00:00Z");
        match &event.decision {
            RouteDecision::Local { .. } => local_count += 1,
            RouteDecision::Escalate { .. } => escalate_count += 1,
        }
        // cost fields must be populated
        if let RouteDecision::Escalate { .. } = &event.decision {
            assert!(event.estimated_cost_usd > 0.0, "escalated task must have a cost");
        }
        assert!(event.counterfactual_cost_usd > 0.0, "all tasks have a counterfactual cost");
        ledger.append(&event).unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);

    // Routing: dev→escalate, reflector→local, no-role→depends-on-difficulty.
    assert_eq!(local_count, 3, "reflector tasks + easy doc task should be local");
    assert_eq!(escalate_count, 2, "dev task + hard refactor should escalate");

    // Ledger + cost summary.
    let events = EscalationLedger::replay_today(tmp.path());
    assert_eq!(events.len(), 5);

    let s = EscalationLedger::summary(tmp.path(), 1);
    assert_eq!(s.total, 5);
    assert_eq!(s.escalations, 2);
    assert!((s.rate - 0.4).abs() < 0.001, "rate={}", s.rate);
    // 2 escalations × 200+8000=8200 tokens × $0.015/1k ≈ $0.123
    assert!(s.spend_usd > 0.0, "spend should be > 0");
    // 3 local tasks avoided frontier cost → savings > 0
    assert!(s.savings_usd > 0.0, "savings should be > 0");
    assert!(
        s.total > s.escalations,
        "more local tasks than escalations"
    );

    // Verify specific decisions.
    let dev_event = events.iter().find(|e| e.role.as_deref() == Some("dev")).unwrap();
    assert!(matches!(dev_event.decision, RouteDecision::Escalate { .. }));
    assert_eq!(dev_event.task_type, TaskType::Execution);

    let reflector_event = events
        .iter()
        .find(|e| e.role.as_deref() == Some("reflector"))
        .unwrap();
    assert!(matches!(reflector_event.decision, RouteDecision::Local { .. }));
}

#[test]
fn empty_registry_is_rejected() {
    let reg = mur_common::model::ModelRegistry::default();
    let err = Router::new(reg).unwrap_err();
    assert!(
        err.to_string().contains("empty"),
        "empty registry should error: {err}"
    );
}

#[test]
fn escalation_rate_decreases_with_more_local_tasks() {
    let reg = setup_registry_with_roles();
    let router = Router::new(reg).unwrap();
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();

    let easy_tasks = [
        ("run cargo fmt", TaskType::Execution, 100_u64),
        ("echo hello", TaskType::Execution, 50),
        ("list files", TaskType::Execution, 75),
        ("check git status", TaskType::Execution, 80),
        ("print working dir", TaskType::Execution, 60),
    ];

    let hard_tasks = [
        ("refactor auth", TaskType::Refactor, 9000_u64),
        ("rewrite database layer", TaskType::CodeGen, 12000),
        ("fix race condition in scheduler", TaskType::Debugging, 7000),
    ];

    for (summary, tt, tokens) in &hard_tasks {
        let event = router.audit(summary, *tt, *tokens, None, "2026-06-01T12:00:00Z");
        ledger.append(&event).unwrap();
    }
    for (summary, tt, tokens) in &easy_tasks {
        let event = router.audit(summary, *tt, *tokens, None, "2026-06-01T12:01:00Z");
        ledger.append(&event).unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);

    let s = EscalationLedger::summary(tmp.path(), 1);
    assert_eq!(s.total, 8);
    // Hard tasks (3) + easy tasks (5) → 3 escalations → rate 3/8 = 0.375
    assert!(s.rate > 0.3 && s.rate < 0.45, "rate={}, expected ~0.375", s.rate);
    assert!(s.savings_usd > 0.0, "savings should be > 0");
}
