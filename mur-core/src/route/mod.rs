//! Cost-router orchestrator — difficulty heuristic, routing decisions,
//! and escalation audit ledger.
//!
//! Phase 1 (this module): route decisions + audit ledger.
//! Phase 2 (deferred): governed spawn via `CodingAgentAdapter`.

pub mod heuristic;
pub mod ledger;

use anyhow::Result;
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::{EscalationEvent, RouteDecision, RoutePolicy, RouteTier, TaskType};

use crate::route::heuristic::{DefaultHeuristic, DifficultyHeuristic};

/// Default difficulty threshold above which a task escalates to frontier.
const DEFAULT_ESCALATION_THRESHOLD: f64 = 0.55;

/// Higher escalation threshold applied when a role's policy is `PreferLocal`.
/// Reachable because the heuristic's realistic max is ≈ 0.85 (see
/// `DefaultHeuristic`).
const PREFER_LOCAL_THRESHOLD: f64 = 0.75;

/// Providers treated as local/cheap when a model carries no explicit `tier`.
const LOCAL_PROVIDERS: &[&str] =
    &["ollama", "llamacpp", "llama_cpp", "mlx", "lmstudio", "local"];

/// Enforced by `Router::new` (which rejects an empty registry); justifies the
/// `expect` on cross-tier degradation, where at least one model always exists.
const REGISTRY_NONEMPTY_INVARIANT: &str =
    "Router::new guarantees a non-empty registry, so a model always exists";

/// Combines the difficulty heuristic with the model registry and per-role
/// overrides to decide local vs. frontier routing for a sub-task.
pub struct Router {
    registry: ModelRegistry,
    heuristic: DefaultHeuristic,
    escalation_threshold: f64,
}

impl Router {
    /// Create a new Router from a model registry.
    ///
    /// Errors if the registry has no models — routing cannot pick a target
    /// otherwise. A single-tier registry is fine: tasks degrade gracefully to
    /// whatever tier exists.
    pub fn new(registry: ModelRegistry) -> Result<Self> {
        if registry.models.is_empty() {
            anyhow::bail!(
                "cannot build a Router: the model registry is empty — add at \
                 least one model with `mur model add`"
            );
        }
        Ok(Self {
            registry,
            heuristic: DefaultHeuristic::default(),
            escalation_threshold: DEFAULT_ESCALATION_THRESHOLD,
        })
    }

    /// Decide where to route a sub-task.
    ///
    /// Thin wrapper over [`Router::decide_with_score`] that discards the score.
    /// There is exactly **one** routing code path (`decide_with_score`), so the
    /// two entry points can never disagree.
    pub fn decide(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
        role: Option<&str>,
    ) -> RouteDecision {
        self.decide_with_score(task_summary, task_type, estimated_tokens, role)
            .0
    }

    /// Decide where to route a sub-task, returning the decision **and** the
    /// difficulty score (recorded in the escalation ledger).
    ///
    /// Single source of truth for routing — [`Router::decide`] and
    /// [`Router::audit`] both delegate here. Order of precedence:
    /// 1. Per-role [`RoutePolicy`] override: `ForceLocal` / `ForceFrontier` /
    ///    `PreferLocal` (higher threshold); `Auto` falls through to the heuristic.
    /// 2. Difficulty score vs. the escalation threshold.
    pub fn decide_with_score(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
        role: Option<&str>,
    ) -> (RouteDecision, f64) {
        let score = self.heuristic.score(task_summary, task_type, estimated_tokens);

        // 1. Per-role policy override (Auto falls through to the heuristic).
        if let Some(role_name) = role
            && let Some(policy) = self.role_policy(role_name)
        {
            match policy {
                RoutePolicy::Auto => {}
                RoutePolicy::PreferLocal => {
                    return (
                        self.by_threshold(score, PREFER_LOCAL_THRESHOLD, "prefer-local"),
                        score,
                    );
                }
                RoutePolicy::ForceLocal => {
                    return (
                        RouteDecision::Local {
                            model_id: self.local_or_frontier(),
                            reason: "role policy: force_local".into(),
                        },
                        score,
                    );
                }
                RoutePolicy::ForceFrontier { model_id } => {
                    let id = if self.registry.models.contains_key(model_id) {
                        model_id.clone()
                    } else {
                        self.frontier_or_local()
                    };
                    return (
                        RouteDecision::Escalate {
                            model_id: id,
                            reason: format!("role policy: force_frontier → {model_id}"),
                        },
                        score,
                    );
                }
            }
        }

        // 2. No (or Auto) override — route by the default threshold.
        (
            self.by_threshold(score, self.escalation_threshold, "threshold"),
            score,
        )
    }

    /// Build a fully-populated audit event for a routing decision — including
    /// the counterfactual local model and cost estimates. `timestamp` is
    /// supplied by the caller (RFC-3339) so the Router stays deterministic and
    /// testable.
    pub fn audit(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
        role: Option<&str>,
        timestamp: &str,
    ) -> EscalationEvent {
        let (decision, score) =
            self.decide_with_score(task_summary, task_type, estimated_tokens, role);

        // Cost the frontier alternative: tokens × best-frontier price per 1k.
        let frontier_cost_per_1k = self.frontier_cost_per_1k().unwrap_or(0.0);
        let counterfactual = estimated_tokens as f64 / 1000.0 * frontier_cost_per_1k;

        let (estimated_cost, escalation_from) = match &decision {
            RouteDecision::Escalate { .. } => (counterfactual, self.pick_best_local()),
            RouteDecision::Local { .. } => (0.0, None),
        };

        EscalationEvent {
            timestamp: timestamp.to_string(),
            task_summary: task_summary.to_string(),
            difficulty_score: score,
            task_type,
            estimated_context_tokens: estimated_tokens,
            decision,
            role: role.map(str::to_string),
            escalation_from,
            estimated_cost_usd: estimated_cost,
            counterfactual_cost_usd: counterfactual,
        }
    }

    /// Route by a difficulty threshold, degrading across tiers when one tier is
    /// unregistered. `label` names the threshold in the reason string.
    fn by_threshold(&self, score: f64, threshold: f64, label: &str) -> RouteDecision {
        if score >= threshold {
            match self.pick_best_frontier() {
                Some(model_id) => RouteDecision::Escalate {
                    model_id,
                    reason: format!("difficulty {score:.2} ≥ {label} {threshold:.2}"),
                },
                None => RouteDecision::Local {
                    model_id: self.pick_best_local().expect(REGISTRY_NONEMPTY_INVARIANT),
                    reason: format!(
                        "difficulty {score:.2} ≥ {label}, but no frontier model — using local"
                    ),
                },
            }
        } else {
            match self.pick_best_local() {
                Some(model_id) => RouteDecision::Local {
                    model_id,
                    reason: format!("difficulty {score:.2} < {label} {threshold:.2}"),
                },
                None => RouteDecision::Escalate {
                    model_id: self.pick_best_frontier().expect(REGISTRY_NONEMPTY_INVARIANT),
                    reason: format!(
                        "difficulty {score:.2} < {label}, but no local model — using frontier"
                    ),
                },
            }
        }
    }

    /// Return the role's routing policy, if any.
    fn role_policy(&self, role_name: &str) -> Option<&RoutePolicy> {
        self.registry.roles.get(role_name)?.route_policy.as_ref()
    }

    /// Effective tier for a model: its explicit `tier`, or inferred from the
    /// provider when unset (honors the `ModelEntry.tier` doc contract).
    fn effective_tier(entry: &ModelEntry) -> RouteTier {
        if let Some(tier) = entry.tier {
            return tier;
        }
        if LOCAL_PROVIDERS.contains(&entry.provider.to_lowercase().as_str()) {
            RouteTier::Local
        } else {
            RouteTier::Frontier
        }
    }

    /// Pick the best model in `tier` by capability count. Ties resolve
    /// deterministically by the `BTreeMap`'s key order.
    fn pick_best(&self, tier: RouteTier) -> Option<String> {
        self.registry
            .models
            .iter()
            .filter(|(_, e)| Self::effective_tier(e) == tier)
            .max_by_key(|(_, e)| e.capabilities.len())
            .map(|(k, _)| k.clone())
    }

    fn pick_best_local(&self) -> Option<String> {
        self.pick_best(RouteTier::Local)
    }

    fn pick_best_frontier(&self) -> Option<String> {
        self.pick_best(RouteTier::Frontier)
    }

    /// Best local model, degrading to frontier if no local model exists.
    fn local_or_frontier(&self) -> String {
        self.pick_best_local()
            .or_else(|| self.pick_best_frontier())
            .expect(REGISTRY_NONEMPTY_INVARIANT)
    }

    /// Best frontier model, degrading to local if no frontier model exists.
    fn frontier_or_local(&self) -> String {
        self.pick_best_frontier()
            .or_else(|| self.pick_best_local())
            .expect(REGISTRY_NONEMPTY_INVARIANT)
    }

    /// USD-per-1k of the best frontier model, if known.
    fn frontier_cost_per_1k(&self) -> Option<f64> {
        let id = self.pick_best_frontier()?;
        self.registry.models.get(&id)?.cost_per_1k_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::RoleEntry;

    fn test_registry() -> ModelRegistry {
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

    #[test]
    fn easy_task_routes_to_local() {
        let router = Router::new(test_registry()).unwrap();
        let decision = router.decide("run cargo fmt", TaskType::Execution, 200, None);
        assert!(
            matches!(decision, RouteDecision::Local { .. }),
            "expected Local, got {decision:?}"
        );
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
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "expected Escalate, got {decision:?}"
        );
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
        let decision = router.decide(
            "refactor everything",
            TaskType::Refactor,
            10_000,
            Some("reflector"),
        );
        assert!(
            matches!(decision, RouteDecision::Local { .. }),
            "force_local should win, got {decision:?}"
        );
    }

    #[test]
    fn force_frontier_override_wins() {
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
        let decision = router.decide(
            "run echo hello",
            TaskType::Execution,
            50,
            Some("dev"),
        );
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "force_frontier should win even on trivial tasks, got {decision:?}"
        );
    }

    #[test]
    fn no_local_model_escalates() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "anthropic_opus".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                base_url: None,
                secret: None,
                capabilities: vec!["chat".into()],
                params: serde_json::Value::Null,
                tier: Some(RouteTier::Frontier),
                cost_per_1k_tokens: Some(0.015),
            },
        );
        let router = Router::new(reg).unwrap();
        let decision = router.decide("echo hello", TaskType::Execution, 50, None);
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "no local model → must escalate, got {decision:?}"
        );
    }

    #[test]
    fn decide_with_score_returns_score() {
        let router = Router::new(test_registry()).unwrap();
        let (decision, score) = router.decide_with_score(
            "medium task",
            TaskType::CodeGen,
            500,
            None,
        );
        assert!((0.0..=1.0).contains(&score));
        // Medium code-gen task around the threshold — don't assert the
        // decision, just that we got a valid one.
        match &decision {
            RouteDecision::Local { model_id, .. } => {
                assert_eq!(model_id, "ollama_llama3");
            }
            RouteDecision::Escalate { model_id, .. } => {
                assert_eq!(model_id, "anthropic_opus");
            }
        }
    }

    #[test]
    fn prefer_local_raises_threshold() {
        let mut reg = test_registry();
        reg.roles.insert(
            "chat".into(),
            RoleEntry {
                primary: "ollama_llama3".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::PreferLocal),
            },
        );
        let router = Router::new(reg).unwrap();
        // A moderately-hard task that would normally escalate should stay local
        // under PreferLocal.
        let (decision, _score) = router.decide_with_score(
            "refactor a function",
            TaskType::Refactor,
            3000,
            Some("chat"),
        );
        // PreferLocal threshold is 0.75. Refactor base 0.70, ctx(3000) ≈ 0.49,
        // keywords (1 hit) ≈ 0.33 → 0.50·0.70 + 0.35·0.49 + 0.15·0.33 ≈ 0.57
        // < 0.75 → stays local.
        assert!(
            matches!(decision, RouteDecision::Local { .. }),
            "prefer_local should keep moderate tasks local, got {decision:?}"
        );
    }

    #[test]
    fn prefer_local_still_escalates_extreme_tasks() {
        // Reachability guard: the heuristic max is ≈ 0.85, so the 0.75
        // prefer-local threshold IS reachable — an extreme task must escalate.
        let mut reg = test_registry();
        reg.roles.insert(
            "chat".into(),
            RoleEntry {
                primary: "ollama_llama3".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::PreferLocal),
            },
        );
        let router = Router::new(reg).unwrap();
        // base 0.70 + ctx(200k)=1.0 + keywords (redesign/rewrite/migrate → 1.0)
        // → 0.50·0.70 + 0.35 + 0.15 = 0.85 ≥ 0.75 → escalate.
        let (decision, score) = router.decide_with_score(
            "redesign rewrite migrate the storage engine",
            TaskType::Refactor,
            200_000,
            Some("chat"),
        );
        assert!(score >= 0.75, "extreme task should exceed prefer-local threshold, got {score}");
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "extreme task must escalate even under prefer_local, got {decision:?}"
        );
    }

    #[test]
    fn decide_matches_decide_with_score_for_auto_role() {
        // Regression: `decide()` must delegate to `decide_with_score()`. Previously
        // `decide()` handled an explicit `Auto` role by recursing with
        // `TaskType::General` and 0 tokens — discarding the real task type and
        // size — so a hard task routed Local via `decide()` but Escalate via
        // `decide_with_score()`. No test caught the divergence.
        let mut reg = test_registry();
        reg.roles.insert(
            "dev".into(),
            RoleEntry {
                primary: "anthropic_opus".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::Auto),
            },
        );
        let router = Router::new(reg).unwrap();
        // Hard task (Refactor base 0.70 + ctx(8000) → ~0.58 ≥ 0.55 threshold)
        // must escalate whether or not the role is explicitly `Auto`.
        let hard = "refactor the entire auth system across 12 modules";
        let via_decide = router.decide(hard, TaskType::Refactor, 8000, Some("dev"));
        let (via_score, _) =
            router.decide_with_score(hard, TaskType::Refactor, 8000, Some("dev"));
        assert_eq!(
            std::mem::discriminant(&via_decide),
            std::mem::discriminant(&via_score),
            "decide() and decide_with_score() must agree: {via_decide:?} vs {via_score:?}"
        );
        assert!(
            matches!(via_decide, RouteDecision::Escalate { .. }),
            "an explicit Auto role must still escalate a hard task, got {via_decide:?}"
        );
    }
}
