//! Whether a fleet costs money, and what "bounded" means for it (spec
//! 2026-09-12 execution-limits, §2 D4 and §5).
//!
//! A fleet is billable when ANY agent it runs — router or member — resolves to
//! a metered model, or to one whose billing cannot be resolved at all. That is
//! the conservative reading: the cost gate exists to stop unbounded spend, so
//! it errs toward "this might cost something". A local-only fleet is bounded by
//! a deadline; a billable one by a deadline or a `budget_usd`.

// Wired into the loop and the daemon by the next two commits; until then
// nothing calls it. Removed there.
#![allow(dead_code)]

use std::path::Path;

use mur_common::fleet::{Fleet, FleetLoop};
use mur_common::model::BillingMode;

/// The fold of every agent's billing into one answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetBilling {
    /// True when at least one agent is `UsageBilled`, or unresolvable.
    pub billable: bool,
    /// `(agent, model_ref)` pairs whose billing could not be resolved and were
    /// therefore counted as metered — so the user can be told which line in
    /// `models.yaml` to fix.
    pub unknown: Vec<(String, String)>,
}

/// The pure core: `lookup(agent)` answers `Some(mode)` for a resolvable agent
/// and `None` for one whose profile or model cannot be read.
pub fn fleet_billing_with(
    fleet: &Fleet,
    lookup: impl Fn(&str) -> Option<BillingMode>,
) -> FleetBilling {
    let mut billable = false;
    let mut unknown = Vec::new();
    let router = fleet.router_or_concierge().to_string();
    for agent in std::iter::once(router.as_str()).chain(fleet.members.iter().map(String::as_str)) {
        match lookup(agent) {
            Some(BillingMode::UsageBilled) => billable = true,
            Some(BillingMode::Local) | Some(BillingMode::Subscription) => {}
            None => {
                billable = true;
                unknown.push((agent.to_string(), String::from("?")));
            }
        }
    }
    FleetBilling { billable, unknown }
}

/// `fleet_billing_with` against the real `~/.mur`: each agent's
/// `profile.yaml` `model_ref` → `models.yaml` entry → `billing_or_inferred`.
pub fn fleet_billing(mur_home: &Path, fleet: &Fleet) -> FleetBilling {
    let registry = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml")).ok();
    let mut out = fleet_billing_with(fleet, |agent| {
        let profile = mur_common::agent::AgentProfile::load(mur_home, agent).ok()?;
        let model_ref = profile.model_ref?;
        let entry = registry.as_ref()?.models.get(&model_ref)?;
        Some(entry.billing_or_inferred())
    });
    // Fill in the model_ref for the unknowns so the note can name it.
    for (agent, model_ref) in out.unknown.iter_mut() {
        if let Ok(p) = mur_common::agent::AgentProfile::load(mur_home, agent)
            && let Some(m) = p.model_ref
        {
            *model_ref = m;
        }
    }
    out
}

/// §5: unattended work must be bounded. A deadline bounds any fleet; a
/// positive `budget_usd` bounds a billable one and means nothing for a fleet
/// that cannot spend.
pub fn is_bounded(lc: Option<&FleetLoop>, billing: &FleetBilling) -> bool {
    let Some(l) = lc else { return false };
    let has_deadline = !l.deadline.trim().is_empty();
    let has_budget = billing.billable && l.budget_usd > 0.0;
    has_deadline || has_budget
}

/// Why `is_bounded` said no, in the words of the fix. Only meaningful when it
/// did say no.
pub fn unbounded_reason(lc: Option<&FleetLoop>, billing: &FleetBilling, fleet: &str) -> String {
    let _ = lc;
    if billing.billable {
        format!(
            "fleet '{fleet}' has no bound — set a deadline (mur fleet settings {fleet} --deadline 2h) \
             or a budget (--budget-usd <USD>) before it may run unattended"
        )
    } else {
        format!(
            "fleet '{fleet}' has no bound — it runs on local/subscription models, so a budget does not \
             apply; set a deadline: mur fleet settings {fleet} --deadline 2h"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fleet(router: Option<&str>, members: &[&str], lc: Option<FleetLoop>) -> Fleet {
        Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: router.map(str::to_string),
            members: members.iter().map(|m| m.to_string()).collect(),
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: lc,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        }
    }

    fn lc(budget_usd: f64, deadline: &str) -> FleetLoop {
        FleetLoop {
            trigger: "interval:1h".into(),
            max_iterations: 0,
            budget_usd,
            deadline: deadline.into(),
            done_when: String::new(),
        }
    }

    #[test]
    fn a_fleet_is_billable_when_any_agent_is_metered_or_unknown() {
        let f = fleet(Some("mur"), &["a", "b"], None);
        let all_local = fleet_billing_with(&f, |_| Some(BillingMode::Local));
        assert!(!all_local.billable);
        assert!(all_local.unknown.is_empty());

        let one_metered = fleet_billing_with(&f, |a| {
            Some(if a == "b" {
                BillingMode::UsageBilled
            } else {
                BillingMode::Local
            })
        });
        assert!(one_metered.billable);

        let subscription = fleet_billing_with(&f, |_| Some(BillingMode::Subscription));
        assert!(!subscription.billable, "a subscription fleet cannot spend");

        // The router counts: a local member fleet with a metered router bills.
        let metered_router = fleet_billing_with(&f, |a| {
            Some(if a == "mur" {
                BillingMode::UsageBilled
            } else {
                BillingMode::Local
            })
        });
        assert!(metered_router.billable);

        // Unresolvable → metered, and named.
        let unknown = fleet_billing_with(&f, |a| (a != "a").then_some(BillingMode::Local));
        assert!(unknown.billable);
        assert_eq!(unknown.unknown, vec![("a".to_string(), "?".to_string())]);
    }

    #[test]
    fn bounded_means_a_deadline_or_a_budget_that_can_apply() {
        let local = FleetBilling {
            billable: false,
            unknown: vec![],
        };
        let billed = FleetBilling {
            billable: true,
            unknown: vec![],
        };

        // No loop config at all: never bounded.
        assert!(!is_bounded(None, &local));
        assert!(!is_bounded(None, &billed));

        // A deadline bounds anyone.
        assert!(is_bounded(Some(&lc(0.0, "2h")), &local));
        assert!(is_bounded(Some(&lc(0.0, "2h")), &billed));

        // A budget bounds only a fleet that can spend.
        assert!(is_bounded(Some(&lc(5.0, "")), &billed));
        assert!(
            !is_bounded(Some(&lc(5.0, "")), &local),
            "a dollar figure means nothing here"
        );

        // Neither → unbounded, whatever the billing.
        assert!(!is_bounded(Some(&lc(0.0, "")), &billed));
        assert!(
            !is_bounded(Some(&lc(0.0, "   ")), &local),
            "whitespace is not a deadline"
        );
    }

    #[test]
    fn the_unbounded_reason_names_the_knob_that_applies() {
        let local = FleetBilling {
            billable: false,
            unknown: vec![],
        };
        let billed = FleetBilling {
            billable: true,
            unknown: vec![],
        };
        let r = unbounded_reason(Some(&lc(0.0, "")), &local, "dev");
        assert!(r.contains("--deadline"), "{r}");
        assert!(
            !r.contains("--budget-usd"),
            "a local fleet is not offered a budget: {r}"
        );
        let r = unbounded_reason(Some(&lc(0.0, "")), &billed, "dev");
        assert!(
            r.contains("--deadline") && r.contains("--budget-usd"),
            "{r}"
        );
    }
}
