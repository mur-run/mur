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
//! **Pricing.** A turn is many LLM calls, and routing or fallback can answer
//! each with a different model, so the runtime reports the turn split by
//! answering model (`usage.by_model`). Each share is priced at its own
//! model's input and output rates and billed by its own model's billing mode.
//! The cached part of a share's prompt is priced at the model's cache-read
//! and cache-write rates; a model with no cache rate recorded prices it at
//! the input rate, which over- rather than under-states spend.
//!
//! A share names its model by `models.yaml` key, which the runtime stamps
//! wherever it knows it. Without a key (an older runtime) the provider's model
//! name is matched against the registry, and accepted only when exactly one
//! entry serves that name — an alias is not a provider name, and two aliases
//! can serve one name at different prices. A share whose model still cannot
//! be resolved is never given some other model's classification: it counts as
//! metered at the guard's conservative fallback rates, with a warning. So does
//! a resolved metered model with no rate. The fallback is the registry's
//! dearest input rate for the prompt and its dearest output rate for the
//! reply, and the cached part of the prompt is split out just as for a priced
//! model (#2130): pricing a heavily cached agent loop's whole prompt at an
//! output rate overstated spend by two orders of magnitude. Spend priced that
//! way is still an upper estimate, not a bill, and the run says so the first
//! time it happens; a run whose metered member has no rate is warned about
//! before it starts. Only a reply that names no model at all is priced as the
//! agent's profile model.
//!
//! **Billability.** Only billable work counts: a share answered by a local or
//! subscription model adds nothing, because those tokens are not on anyone's
//! bill. The guard is armed whenever a positive cap is set, even on a fleet
//! whose primaries are all free. Its billability cannot be judged in advance,
//! because a fallback chain or routing can hand any turn to a metered model.
//! Unlike the loop's projection ([`super::loop_run::budget_for`]), counting
//! actual shares cannot stop a run on money nobody is paying.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mur_common::fleet::Fleet;
use mur_common::model::ModelEntry;

use crate::executor::dag::{
    HaltReason, HaltSignal, ModelShare, StepEvent, StepEventKind, TokenUsage,
};

/// What the run tells the job record and the operator when the cap stopped it.
pub const BUDGET_STOP_REASON: &str = "stopped: cost_usd reached";

/// What the guard knows about one agent of the fleet before any turn runs.
#[derive(Debug, Clone, Default)]
pub struct AgentBilling {
    /// Does its work cost money? Used only for a reply that names no model
    /// and an agent whose profile model is not in the registry.
    pub bills: bool,
    /// The profile's `model_ref`: the model assumed when a reply names none.
    pub model_ref: Option<String>,
}

#[derive(Default)]
struct Spend {
    usd: f64,
    /// Models already warned about as priced at the flat fallback rate.
    guessed: HashSet<String>,
}

enum Resolved<'a> {
    Entry(&'a ModelEntry),
    /// The reply named a model the registry cannot pin down.
    Unresolved(String),
    /// The reply named no model, and the profile's is not in the registry.
    ProfileUnknown,
}

/// The live cost guard of one one-shot run.
pub struct OneShotBudget {
    cap: f64,
    /// The rates a model `models.yaml` gives no rate for is priced at.
    fallback: ModelEntry,
    agents: HashMap<String, AgentBilling>,
    models: BTreeMap<String, ModelEntry>,
    router: String,
    spend: Mutex<Spend>,
    tripped: AtomicBool,
}

impl OneShotBudget {
    /// The guard for `fleet`, or `None` when no positive `cost_usd` caps this
    /// run. Never `None` because the primaries are free: a fallback or routed
    /// model can be metered, and per-share billing already leaves free usage
    /// uncounted (#2083 re-review).
    pub fn for_fleet(mur_home: &Path, fleet: &Fleet, cost_usd: Option<f64>) -> Option<Arc<Self>> {
        let cap = cost_usd.filter(|&c| c > 0.0)?;
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
        let (flat_per_1k, source) = super::loop_run::fleet_price_per_1k(mur_home);
        let fallback = match source {
            super::loop_run::GuardRate::Registry => dearest_rates(&models),
            _ => None,
        }
        .unwrap_or_else(|| flat_rate(flat_per_1k));
        println!(
            "  bounds: cost_usd ${cap:.2} — enforced on actual spend, each billable turn \
             at its model's input/output rates"
        );
        let budget = Self::new(cap, 0.0, router, agents, models).with_fallback(fallback);
        budget.warn_unpriced();
        Some(Arc::new(budget))
    }

    /// A guard pricing an unpriced model at one flat `fallback_per_1k` rate
    /// for every token; [`Self::with_fallback`] sets separate rates.
    pub fn new(
        cap: f64,
        fallback_per_1k: f64,
        router: String,
        agents: HashMap<String, AgentBilling>,
        models: BTreeMap<String, ModelEntry>,
    ) -> Self {
        Self {
            cap,
            fallback: flat_rate(fallback_per_1k),
            agents,
            models,
            router,
            spend: Mutex::new(Spend::default()),
            tripped: AtomicBool::new(false),
        }
    }

    /// Price an unpriced model at `fallback`'s rates instead.
    pub fn with_fallback(mut self, fallback: ModelEntry) -> Self {
        self.fallback = fallback;
        self
    }

    /// Say before the run starts which metered agents' models have no rate,
    /// rather than only once their first turn is priced as a guess (#2130).
    fn warn_unpriced(&self) {
        let mut unpriced: Vec<(&str, &str)> = self
            .agents
            .iter()
            .filter(|(_, info)| info.bills)
            .filter_map(|(agent, info)| {
                let key = info.model_ref.as_deref()?;
                let entry = self.models.get(key)?;
                let (i, o) = entry.effective_costs();
                (i.is_none() && o.is_none()).then_some((agent.as_str(), key))
            })
            .collect();
        if unpriced.is_empty() {
            return;
        }
        unpriced.sort_unstable();
        let list: Vec<String> = unpriced.iter().map(|(a, m)| format!("{a} ({m})")).collect();
        eprintln!(
            "  ⚠ cost_usd: no rate in models.yaml for {} — their spend is an upper estimate \
             at {}. `mur model add <key> … --input-cost --output-cost` records a real rate.",
            list.join(", "),
            self.fallback_text()
        );
    }

    /// The fallback rates, as the warnings quote them.
    fn fallback_text(&self) -> String {
        let (i, o) = self.fallback.effective_costs();
        let (i, o) = (i.unwrap_or_default(), o.unwrap_or_default());
        match self.fallback.cache_read_cost_per_1k {
            Some(r) => format!("${i}/1k in, ${r}/1k cached, ${o}/1k out"),
            None => format!("${i}/1k in, ${o}/1k out"),
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
        let cost: f64 = usage
            .shares()
            .iter()
            .map(|share| self.price_share(info, share))
            .sum();
        let mut s = self.spend();
        s.usd += cost;
        if s.usd >= self.cap && !self.tripped.swap(true, Ordering::SeqCst) {
            eprintln!(
                "\n⏹ cost_usd reached: ${:.2} spent of ${:.2} — no further steps start.",
                s.usd, self.cap
            );
        }
    }

    /// What one answering model's share of a turn cost.
    fn price_share(&self, info: Option<&AgentBilling>, share: &ModelShare) -> f64 {
        if share.input == 0 && share.output == 0 {
            return 0.0;
        }
        let entry = match self.resolve(info, share) {
            Resolved::Entry(e) => e,
            Resolved::Unresolved(label) => return self.flat(&label, share, "is not in"),
            Resolved::ProfileUnknown => {
                // No model reported and none configured that the registry
                // knows: the agent's own billing class is all there is.
                return if info.is_none_or(|i| i.bills) {
                    self.flat("an unnamed model", share, "is not in")
                } else {
                    0.0
                };
            }
        };
        if !super::billing::agent_bills(Some(entry.billing_or_inferred())) {
            return 0.0;
        }
        match entry.cost_with_cache(
            share.input,
            share.cache_read,
            share.cache_write,
            share.output,
        ) {
            Some(c) => c,
            None => {
                let label = share
                    .registry_key
                    .clone()
                    .or_else(|| share.model.clone())
                    .unwrap_or_else(|| "the profile model".into());
                self.flat(&label, share, "has no rate in")
            }
        }
    }

    /// Which registry entry answered `share`.
    fn resolve(&self, info: Option<&AgentBilling>, share: &ModelShare) -> Resolved<'_> {
        if let Some(key) = &share.registry_key {
            return match self.models.get(key) {
                Some(e) => Resolved::Entry(e),
                None => Resolved::Unresolved(key.clone()),
            };
        }
        if let Some(name) = &share.model {
            // Exactly one entry may serve the name; otherwise the guard cannot
            // tell which alias — and which price — answered.
            let mut serving = self.models.values().filter(|e| &e.model == name);
            return match (serving.next(), serving.next()) {
                (Some(e), None) => Resolved::Entry(e),
                _ => Resolved::Unresolved(name.clone()),
            };
        }
        match info
            .and_then(|i| i.model_ref.as_deref())
            .and_then(|m| self.models.get(m))
        {
            Some(e) => Resolved::Entry(e),
            None => Resolved::ProfileUnknown,
        }
    }

    /// Conservative pricing: metered, at the fallback rates, said once per
    /// model. The cached part of the prompt is split out exactly as for a
    /// priced model, so a cache read is never billed at an output rate.
    fn flat(&self, label: &str, share: &ModelShare, why: &str) -> f64 {
        if self.spend().guessed.insert(label.to_string()) {
            eprintln!(
                "  ⚠ cost_usd: {label} {why} models.yaml — its tokens are counted as metered at \
                 the guard's conservative {}, so spend is an upper estimate. \
                 `mur model add` records a real rate.",
                self.fallback_text()
            );
        }
        self.fallback
            .cost_with_cache(
                share.input,
                share.cache_read,
                share.cache_write,
                share.output,
            )
            .unwrap_or_default()
    }

    /// The step observer to hand the DAG executor.
    pub fn observer(self: &Arc<Self>) -> Arc<dyn Fn(StepEvent) + Send + Sync> {
        let me = Arc::clone(self);
        Arc::new(move |e: StepEvent| me.record(&e))
    }

    /// This budget as a halt the DAG polls. It halts with the budget's own
    /// reason, so a halted step says the cap stopped it, not `mur fleet stop`
    /// (#2130). The kill-switch stays on the run's `stop` signal, which the
    /// executor reads first.
    pub fn halt_signal(self: &Arc<Self>) -> HaltSignal {
        let me = Arc::clone(self);
        Arc::new(move || me.tripped().then(|| HaltReason::Limit(me.stop_reason())))
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

/// One rate for every token: the env override and the built-in default.
fn flat_rate(per_1k: f64) -> ModelEntry {
    ModelEntry {
        input_cost_per_1k: Some(per_1k),
        output_cost_per_1k: Some(per_1k),
        ..Default::default()
    }
}

/// The registry's dearest rate on each side, so an unpriced model is never
/// cheaper than anything it could stand in for — without charging its prompt
/// at an output rate. A side no entry prices takes the other side's rate; a
/// cache read with no recorded rate anywhere is priced as input.
fn dearest_rates(models: &BTreeMap<String, ModelEntry>) -> Option<ModelEntry> {
    let dearest = |side: fn(&ModelEntry) -> Option<f64>| {
        models
            .values()
            .filter_map(side)
            .filter(|r| *r > 0.0)
            .reduce(f64::max)
    };
    let input = dearest(|m| m.effective_costs().0);
    let output = dearest(|m| m.effective_costs().1);
    let (input, output) = match (input, output) {
        (None, None) => return None,
        (i, o) => (i.or(o), o.or(i)),
    };
    Some(ModelEntry {
        input_cost_per_1k: input,
        output_cost_per_1k: output,
        cache_read_cost_per_1k: dearest(|m| m.cache_read_cost_per_1k),
        cache_write_cost_per_1k: dearest(|m| m.cache_write_cost_per_1k),
        ..Default::default()
    })
}

#[cfg(test)]
#[path = "oneshot_budget_tests.rs"]
mod tests;
