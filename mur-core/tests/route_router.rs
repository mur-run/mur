mod route_fixtures;
use mur_common::model::RoleEntry;
use mur_common::route::{RouteDecision, RoutePolicy, TaskType};
use mur_core::route::Router;
use route_fixtures::test_registry;

#[test]
fn easy_task_routes_to_local() {
    let router = Router::new(test_registry()).unwrap();
    let decision = router.decide("run cargo fmt", TaskType::Execution, 200, None);
    assert!(matches!(decision, RouteDecision::Local { .. }), "got {decision:?}");
}

#[test]
fn hard_task_routes_to_frontier() {
    let router = Router::new(test_registry()).unwrap();
    let decision = router.decide(
        "refactor the entire auth system across 12 modules",
        TaskType::Refactor,
        8000,
        None,
    );
    assert!(matches!(decision, RouteDecision::Escalate { .. }), "got {decision:?}");
}

#[test]
fn force_local_override_wins() {
    let mut reg = test_registry();
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
    let router = Router::new(reg).unwrap();
    let decision =
        router.decide("refactor everything", TaskType::Refactor, 10_000, Some("reflector"));
    assert!(matches!(decision, RouteDecision::Local { .. }), "got {decision:?}");
}

#[test]
fn force_frontier_trumps_easy_task() {
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
    let router = Router::new(reg).unwrap();
    let decision = router.decide("echo hello", TaskType::Execution, 50, Some("dev"));
    assert!(matches!(decision, RouteDecision::Escalate { .. }), "got {decision:?}");
}

#[test]
fn decide_with_score_exposes_score() {
    let router = Router::new(test_registry()).unwrap();
    let (_decision, score) =
        router.decide_with_score("medium task", TaskType::CodeGen, 500, None);
    assert!((0.0..=1.0).contains(&score));
}
