//! #2083: a one-shot fleet run (`mur fleet run` without `--loop`) enforces
//! `cost_usd` on ACTUAL spend.
//!
//! The loop guards each iteration on a projection; a one-shot run has no
//! iterations to project, so it prices every billable turn the run makes —
//! the router's planning turn and each member's delegated turn — and trips
//! the run's stop signal once the total reaches the cap. The DAG executor
//! already honours that signal at every step boundary and cancels in-flight
//! member turns (#2036), so the run ends `Stopped` — the same terminal path
//! as `mur fleet stop`.
//!
//! **Pricing.** A turn is priced at the input and output rates of the model
//! that answered it (`usage.model_ref`, else the agent's profile model), each
//! side at its own rate. Only a model with no rate in `models.yaml` falls back
//! to the guard's conservative flat rate on its whole token count — and the
//! run says so the first time it happens, because spend priced that way is an
//! upper estimate, not a bill.
//!
//! **Billability.** Only billable work counts. A fleet that cannot spend (all
//! local or subscription) gets no cap at all ([`super::loop_run::budget_for`]).
//! A turn answered by a local or subscription model adds nothing: those tokens
//! are not on anyone's bill. A turn whose model and agent cannot be resolved
//! counts as metered, as everywhere else.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mur_common::fleet::Fleet;
use mur_common::model::ModelEntry;

use crate::executor::dag::{StepEvent, StepEventKind, StopSignal, TokenUsage};

/// What the run tells the job record and the operator when the cap stopped it.
pub const BUDGET_STOP_REASON: &str = "stopped: cost_usd reached";

/// What the guard knows about one agent of the fleet before any turn runs.
#[derive(Debug, Clone, Default)]
pub struct AgentBilling {
    /// Does its work cost money? Used only when the turn's own model cannot
    /// be resolved.
    pub bills: bool,
    /// The profile's `model_ref`: the rate to use when a turn does not say
    /// which model answered it.
    pub model_ref: Option<String>,
}

#[derive(Default)]
struct Spend {
    usd: f64,
    /// Models already warned about as priced at the flat fallback rate.
    guessed: HashSet<String>,
}

/// The live cost guard of one one-shot run.
pub struct OneShotBudget {
    cap: f64,
    /// Flat per-1k rate for a model `models.yaml` gives no rate for.
    fallback_per_1k: f64,
    agents: HashMap<String, AgentBilling>,
    models: BTreeMap<String, ModelEntry>,
    router: String,
    spend: Mutex<Spend>,
    tripped: AtomicBool,
}

impl OneShotBudget {
    /// The guard for `fleet`, or `None` when nothing caps this run: no
    /// `cost_usd` resolved, or the fleet cannot spend.
    pub fn for_fleet(mur_home: &Path, fleet: &Fleet, cost_usd: Option<f64>) -> Option<Arc<Self>> {
        let billing = super::billing::fleet_billing(mur_home, fleet);
        let cap = super::loop_run::budget_for(cost_usd.filter(|&c| c > 0.0), &billing)?;
        let lookup = super::billing::billing_lookup(mur_home);
        let router = fleet.router_or_concierge().to_string();
        let agents = std::iter::once(&router)
            .chain(fleet.members.iter())
            .map(|a| {
                let model_ref = mur_common::agent::AgentProfile::load(mur_home, a)
                    .ok()
                    .and_then(|p| p.model_ref);
                let info = AgentBilling {
                    bills: super::billing::agent_bills(lookup(a)),
                    model_ref,
                };
                (a.clone(), info)
            })
            .collect();
        let models = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml"))
            .map(|r| r.models)
            .unwrap_or_default();
        let (fallback_per_1k, _) = super::loop_run::fleet_price_per_1k(mur_home);
        println!(
            "  bounds: cost_usd ${cap:.2} — enforced on actual spend, each billable turn \
             at its model's input/output rates"
        );
        Some(Arc::new(Self::new(
            cap,
            fallback_per_1k,
            router,
            agents,
            models,
        )))
    }

    pub fn new(
        cap: f64,
        fallback_per_1k: f64,
        router: String,
        agents: HashMap<String, AgentBilling>,
        models: BTreeMap<String, ModelEntry>,
    ) -> Self {
        Self {
            cap,
            fallback_per_1k,
            agents,
            models,
            router,
            spend: Mutex::new(Spend::default()),
            tripped: AtomicBool::new(false),
        }
    }

    fn spend(&self) -> std::sync::MutexGuard<'_, Spend> {
        self.spend.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Billable spend so far, in USD.
    pub fn spent_usd(&self) -> f64 {
        self.spend().usd
    }

    /// True once spend has reached the cap. Latches: spend only grows.
    pub fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    /// Account one step event. Only terminal events carry usage.
    pub fn record(&self, e: &StepEvent) {
        if matches!(
            e.kind,
            StepEventKind::Done | StepEventKind::Failed | StepEventKind::Stopped
        ) {
            self.record_turn(e.agent.as_deref(), &e.usage);
        }
    }

    /// Account the router's planning turn, which runs before any step and so
    /// never reaches the DAG observer.
    pub fn record_router(&self, usage: &TokenUsage) {
        let router = self.router.clone();
        self.record_turn(Some(&router), usage);
    }

    /// Price one agent turn and add it to the run's spend.
    pub fn record_turn(&self, agent: Option<&str>, usage: &TokenUsage) {
        if usage.is_zero() {
            return;
        }
        let info = agent.and_then(|a| self.agents.get(a));
        // The model that actually answered beats the profile's: a fallback
        // chain may have routed the turn elsewhere.
        let model_ref = usage
            .model_ref
            .as_deref()
            .filter(|m| self.models.contains_key(*m))
            .or_else(|| info.and_then(|i| i.model_ref.as_deref()));
        let entry = model_ref.and_then(|m| self.models.get(m));
        let billable = match entry {
            Some(e) => super::billing::agent_bills(Some(e.billing_or_inferred())),
            None => info.is_none_or(|i| i.bills),
        };
        if !billable {
            return;
        }
        let priced = entry.and_then(|e| e.projected_cost(usage.input, usage.output));
        let cost = priced.unwrap_or_else(|| {
            super::loop_run::iteration_cost_usd(usage.total(), self.fallback_per_1k)
        });
        let mut s = self.spend();
        if priced.is_none() {
            let label = model_ref
                .or(usage.model_ref.as_deref())
                .unwrap_or("an unresolved model")
                .to_string();
            if s.guessed.insert(label.clone()) {
                eprintln!(
                    "  ⚠ cost_usd: {label} has no rate in models.yaml — its tokens are counted at \
                     the guard's conservative ${}/1k, so spend is an upper estimate. \
                     `mur model add` records a real rate.",
                    self.fallback_per_1k
                );
            }
        }
        s.usd += cost;
        if s.usd >= self.cap && !self.tripped.swap(true, Ordering::SeqCst) {
            eprintln!(
                "\n⏹ cost_usd reached: ${:.2} spent of ${:.2} — no further steps start.",
                s.usd, self.cap
            );
        }
    }

    /// The step observer to hand the DAG executor.
    pub fn observer(self: &Arc<Self>) -> Arc<dyn Fn(StepEvent) + Send + Sync> {
        let me = Arc::clone(self);
        Arc::new(move |e: StepEvent| me.record(&e))
    }

    /// `kill_switch` OR this budget: the run halts on whichever trips first.
    pub fn stop_signal(self: &Arc<Self>, kill_switch: StopSignal) -> StopSignal {
        let me = Arc::clone(self);
        Arc::new(move || me.tripped() || kill_switch())
    }

    /// The job's error text when the budget, not the operator, stopped it.
    pub fn stop_reason(&self) -> String {
        format!(
            "{BUDGET_STOP_REASON} (${:.2} spent of ${:.2})",
            self.spent_usd(),
            self.cap
        )
    }
}

#[cfg(test)]
#[path = "oneshot_budget_tests.rs"]
mod tests;
