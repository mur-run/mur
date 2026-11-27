//! What a fleet run tells the operator about its bounds and spend before the
//! first iteration, and the spend figures the guards then enforce.

use std::path::Path;

use mur_common::fleet::Fleet;

use super::super::billing::FleetBilling;
use super::{
    FleetBounds, GuardRate, budget_for, estimate_iteration_cost_usd, fleet_price_per_1k,
    no_cap_notice,
};

/// The spend side of a run's bounds, resolved once at its start.
pub(super) struct SpendPlan {
    pub billing: FleetBilling,
    /// The cap the budget guard enforces (`None` = the fleet cannot spend, or
    /// has no cap).
    pub budget: Option<f64>,
    pub price_per_1k: f64,
    /// Forward estimate of one iteration's cost before any real data, and the
    /// fail-safe fallback when an iteration reports no token usage, so spend
    /// can never silently under-count.
    pub projection: f64,
}

/// Print the run's bounds and every spend caveat, and resolve the spend plan.
pub(super) fn spend_plan(
    mur_home: &Path,
    fleet: &Fleet,
    name: &str,
    bounds: &FleetBounds,
) -> SpendPlan {
    let deadline = Some(bounds.deadline);
    println!(
        "  bounds: deadline {} ← {}; stuck {}",
        crate::cmd::limits::fmt_dur(bounds.deadline),
        bounds.deadline_source.label(),
        match bounds.stuck {
            mur_common::limits::Stuck::Off => "off".to_string(),
            mur_common::limits::Stuck::After(d) => crate::cmd::limits::fmt_dur(d),
        }
    );
    let billing = super::super::billing::fleet_billing(mur_home, fleet);
    let configured_budget = bounds.cost_usd.filter(|&b| b > 0.0);
    let budget = budget_for(configured_budget, &billing);
    if configured_budget.is_some_and(|b| b > 0.0) && budget.is_none() {
        println!(
            "  ℹ budget_usd ignored — fleet '{name}' runs on local/subscription models and cannot spend; \
             its bound is the deadline"
        );
    }
    if !billing.unknown.is_empty() {
        let who: Vec<String> = billing
            .unknown
            .iter()
            .map(|(a, m)| format!("{a} ({m})"))
            .collect();
        println!(
            "  ⚠ billing unknown for {} — treated as metered. Mark a local model with `billing: local` in models.yaml",
            who.join(", ")
        );
    }
    let price_per_1k = fleet_price_per_1k(mur_home);
    if let (rate, GuardRate::Default) = price_per_1k
        && budget.is_some()
    {
        // Enforcing on a guess is better than not enforcing, but silently
        // enforcing on one turns "stopped on budget" into a number the user
        // has no way to reconcile against a real bill.
        println!(
            "  ⚠ budget enforced at the default ${rate}/1k — no model in models.yaml \
             carries a rate, so spend is a guess. `mur model add` records a real one."
        );
    }
    let price_per_1k = price_per_1k.0;
    if let Some(line) = no_cap_notice(&billing, budget, deadline, price_per_1k) {
        println!("  {line}");
    }
    let projection = member_projection(mur_home, fleet, price_per_1k);
    SpendPlan {
        billing,
        budget,
        price_per_1k,
        projection,
    }
}

/// Iteration-1 forward estimate, member by member. A subscription or local
/// member cannot spend, so it adds nothing; a metered member is priced at its
/// own model's output rate, falling back to the fleet-wide `fallback_per_1k`
/// when that model carries no rate or cannot be resolved (never cheaper than
/// the conservative reading for an unknown).
pub(super) fn member_projection(mur_home: &Path, fleet: &Fleet, fallback_per_1k: f64) -> f64 {
    let lookup = super::super::billing::billing_lookup(mur_home);
    let registry = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml")).ok();
    let own_rate = |agent: &str| -> Option<f64> {
        let model_ref = mur_common::agent::AgentProfile::load(mur_home, agent)
            .ok()?
            .model_ref?;
        registry
            .as_ref()?
            .models
            .get(&model_ref)?
            .effective_costs()
            .1
    };
    // An explicit override pins one rate for everyone, as it does elsewhere.
    let pinned = std::env::var("MUR_FLEET_COST_PER_1K").is_ok();
    fleet
        .members
        .iter()
        .filter(|m| super::super::billing::agent_bills(lookup(m)))
        .map(|m| {
            let rate = if pinned { None } else { own_rate(m) };
            estimate_iteration_cost_usd(1, rate.unwrap_or(fallback_per_1k))
        })
        .sum()
}
