//! What a loop iteration really cost: every turn priced at its answering
//! model's own rates and billing, by the same pricer a one-shot run's
//! `cost_usd` guard uses ([`OneShotBudget`]). A subscription or local turn
//! adds nothing, so a fleet is no longer billed for every token at the
//! registry's dearest metered rate.
//!
//! The meter has no cap of its own. The loop's guard still decides between
//! iterations; this only tells it what each one spent.

use std::path::Path;
use std::sync::Arc;

use mur_common::fleet::Fleet;

use super::super::oneshot_budget::OneShotBudget;
use super::{GuardRate, fleet_price_per_1k};

/// The meter for one loop run. `MUR_FLEET_COST_PER_1K` still pins one rate
/// for every metered token, as it did when the whole iteration was priced
/// flat. `capped` turns on the guessed-rate warnings: without a cap the
/// figure binds nothing, so a warning would only be noise.
pub(super) fn loop_meter(mur_home: &Path, fleet: &Fleet, capped: bool) -> Arc<OneShotBudget> {
    let pinned = fleet_price_per_1k(mur_home).1 == GuardRate::Env;
    let meter = OneShotBudget::meter(mur_home, fleet)
        .with_pinned_rate(pinned)
        .with_warnings(capped);
    if capped {
        meter.warn_unpriced();
    }
    Arc::new(meter)
}

/// What the meter counted since the last call: this iteration's spend. Always
/// moves `metered` forward, so a turn is never billed to two iterations, even
/// when the caller charges the projection instead.
pub(super) fn iteration_cost(meter: &OneShotBudget, metered: &mut f64) -> f64 {
    let now = meter.spent_usd();
    let cost = (now - *metered).max(0.0);
    *metered = now;
    cost
}
