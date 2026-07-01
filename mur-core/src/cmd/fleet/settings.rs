//! `mur fleet set-loop` — mutate a fleet's `loop:` block (trigger, budget,
//! iteration cap, deadline, done-when marker) without touching anything else.

use std::path::Path;

use anyhow::Result;
use mur_common::fleet::FleetLoop;

use super::store;

/// Mutate a fleet's `loop:` block. Only the fields passed as `Some(..)` are
/// changed — everything else in the existing `loop_cfg` (or `FleetLoop`'s own
/// per-field defaults, if the fleet has no `loop_cfg` yet) is preserved. This
/// merge semantics matters: a partial update (e.g. "just bump the budget")
/// must never silently reset the trigger/deadline/done_when someone already set.
pub fn cmd_fleet_set_loop(
    mur_home: &Path,
    name: &str,
    trigger: Option<String>,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    done_when: Option<String>,
) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;

    // Get existing loop config or create a new one with defaults
    let mut lc = fleet.loop_cfg.unwrap_or_else(|| FleetLoop {
        trigger: "manual".to_string(),
        max_iterations: 0,
        budget_usd: 0.0,
        deadline: String::new(),
        done_when: String::new(),
    });

    // Merge: only update fields that were explicitly passed
    if let Some(t) = trigger {
        lc.trigger = t;
    }
    if let Some(mi) = max_iterations {
        lc.max_iterations = mi;
    }
    if let Some(d) = deadline {
        lc.deadline = d;
    }
    if let Some(b) = budget_usd {
        lc.budget_usd = b;
    }
    if let Some(dw) = done_when {
        lc.done_when = dw;
    }

    fleet.loop_cfg = Some(lc);
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_loop_merges_onto_existing_config_without_clobbering_other_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();

        // First update: set trigger + budget
        cmd_fleet_set_loop(
            home,
            "dev",
            Some("interval:30m".to_string()),
            None,
            None,
            Some(5.0),
            None,
        )
        .unwrap();

        let f = store::load_fleet(home, "dev").unwrap();
        let l = f.loop_cfg.as_ref().unwrap();
        assert_eq!(l.trigger, "interval:30m");
        assert_eq!(l.budget_usd, 5.0);
        // max_iterations must use its own default (not None), proving merge semantics
        assert_eq!(l.max_iterations, u32::default());

        // Second update: only touch max_iterations
        // This must NOT reset trigger/budget to None
        cmd_fleet_set_loop(home, "dev", None, Some(10), None, None, None).unwrap();

        let f = store::load_fleet(home, "dev").unwrap();
        let l = f.loop_cfg.as_ref().unwrap();
        assert_eq!(l.trigger, "interval:30m", "untouched field from first call");
        assert_eq!(l.budget_usd, 5.0, "untouched field from first call");
        assert_eq!(l.max_iterations, 10, "newly set field");
        assert_eq!(
            l.deadline, "",
            "untouched field uses FleetLoop's own default"
        );
        assert_eq!(
            l.done_when, "",
            "untouched field uses FleetLoop's own default"
        );
    }

    #[test]
    fn set_loop_on_fleet_with_no_prior_loop_cfg_uses_field_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();

        cmd_fleet_set_loop(home, "dev", None, None, None, Some(2.0), None).unwrap();
        let f = store::load_fleet(home, "dev").unwrap();
        let l = f.loop_cfg.as_ref().unwrap();
        assert_eq!(
            l.trigger, "manual",
            "untouched field uses FleetLoop's own default"
        );
        assert_eq!(l.budget_usd, 2.0);
    }
}
