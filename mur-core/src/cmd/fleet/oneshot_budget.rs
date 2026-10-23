//! #2083: a one-shot fleet run (`mur fleet run` without `--loop`) enforces
//! `cost_usd` on ACTUAL spend.
//!
//! The loop guards each iteration on a projection; a one-shot run has no
//! iterations to project, so it sums the real tokens every finished step
//! reports and trips the run's stop signal once the total reaches the cap.
//! The DAG executor already honours that signal at every step boundary and
//! cancels in-flight member turns (#2036), so the run ends `Stopped` — the
//! same terminal path as `mur fleet stop`.
//!
//! Only billable work counts. A fleet that cannot spend (all local or
//! subscription) gets no cap at all ([`super::loop_run::budget_for`]), and in
//! a mixed fleet a step delegated to a local or subscription member adds
//! nothing: those tokens are not on anyone's bill. An agent whose billing
//! cannot be resolved counts as metered, as everywhere else.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mur_common::fleet::Fleet;

use crate::executor::dag::{StepEvent, StepEventKind, StopSignal};

/// What the run tells the job record and the operator when the cap stopped it.
pub const BUDGET_STOP_REASON: &str = "stopped: cost_usd reached";

/// The live cost guard of one one-shot run.
pub struct OneShotBudget {
    cap: f64,
    price_per_1k: f64,
    /// Per agent: does its work cost money? Agents missing here (no
    /// `delegate_to`, or not a member) count as billable.
    bills: HashMap<String, bool>,
    /// Billable tokens so far. Integer, so concurrent steps add without a lock.
    tokens: AtomicU64,
    tripped: AtomicBool,
    /// Kept so the trip line is printed once.
    reported: Mutex<bool>,
}

impl OneShotBudget {
    /// The guard for `fleet`, or `None` when nothing caps this run: no
    /// `cost_usd` resolved, or the fleet cannot spend.
    pub fn for_fleet(mur_home: &Path, fleet: &Fleet, cost_usd: Option<f64>) -> Option<Arc<Self>> {
        let billing = super::billing::fleet_billing(mur_home, fleet);
        let cap = super::loop_run::budget_for(cost_usd.filter(|&c| c > 0.0), &billing)?;
        let lookup = super::billing::billing_lookup(mur_home);
        let bills = fleet
            .members
            .iter()
            .map(|m| (m.clone(), super::billing::agent_bills(lookup(m))))
            .collect();
        let (price_per_1k, rate) = super::loop_run::fleet_price_per_1k(mur_home);
        println!("  bounds: cost_usd ${cap:.2} — enforced on actual spend of billable members");
        if rate == super::loop_run::GuardRate::Default {
            println!(
                "  ⚠ budget enforced at the default ${price_per_1k}/1k — no model in models.yaml \
                 carries a rate, so spend is a guess. `mur model add` records a real one."
            );
        }
        Some(Arc::new(Self::new(cap, price_per_1k, bills)))
    }

    pub fn new(cap: f64, price_per_1k: f64, bills: HashMap<String, bool>) -> Self {
        Self {
            cap,
            price_per_1k,
            bills,
            tokens: AtomicU64::new(0),
            tripped: AtomicBool::new(false),
            reported: Mutex::new(false),
        }
    }

    /// Billable spend so far, in USD.
    pub fn spent_usd(&self) -> f64 {
        super::loop_run::iteration_cost_usd(self.tokens.load(Ordering::SeqCst), self.price_per_1k)
    }

    /// True once spend has reached the cap. Latches: spend only grows.
    pub fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    /// Account one step event. Only terminal events carry usage; a step that
    /// ran on a local or subscription member adds nothing.
    pub fn record(&self, e: &StepEvent) {
        if !matches!(
            e.kind,
            StepEventKind::Done | StepEventKind::Failed | StepEventKind::Stopped
        ) || e.tokens_used == 0
        {
            return;
        }
        let billable = e
            .agent
            .as_ref()
            .and_then(|a| self.bills.get(a).copied())
            .unwrap_or(true);
        if !billable {
            return;
        }
        self.tokens.fetch_add(e.tokens_used, Ordering::SeqCst);
        if self.spent_usd() >= self.cap && !self.tripped.swap(true, Ordering::SeqCst) {
            let mut r = self.reported.lock().unwrap_or_else(|p| p.into_inner());
            if !*r {
                *r = true;
                eprintln!(
                    "\n⏹ cost_usd reached: ${:.2} spent of ${:.2} — no further steps start.",
                    self.spent_usd(),
                    self.cap
                );
            }
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
