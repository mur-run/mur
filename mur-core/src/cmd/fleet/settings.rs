//! `mur fleet set-loop` — mutate a fleet's `loop:` block (trigger, budget,
//! iteration cap, deadline, done-when policy) without touching anything else.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::fleet::FleetLoop;

use super::store;

/// Reject a loop setting whose value would be silently reinterpreted as
/// something else.
///
/// Five fields are fail-open at execution time: an unparseable deadline means
/// "no deadline", `max_iterations: 0` means the default, an unfirable cron
/// means "never scheduled", a negative budget means "no ceiling", and a
/// `done_when` without the `marker:` prefix means "ask the router". Each of
/// those is a value that looks configured and does nothing.
///
/// Only the fields passed in THIS call are judged. A fleet already holding a
/// bad value must not have an unrelated partial update (`--budget-usd 2`)
/// rejected because of it — that would match neither the merge semantics
/// documented on `cmd_fleet_set_loop` nor the lenient read side in
/// `done_policy`. Strictness belongs at the write boundary, and this is it:
/// both `mur fleet set-loop` and the Hub's `fleet_set_loop` command land here.
///
/// Validation reuses the parsers execution uses (`parse_duration`,
/// `scheduler::next_fire_after`) so it cannot disagree with what will run.
///
/// Expects `trigger`/`deadline`/`done_when` to already be trimmed by the
/// caller. Trimming a local copy in here for the check while the caller
/// stores the original would let a value pass validation and still persist
/// with stray whitespace attached — which then fails every `strip_prefix`
/// read downstream (`fleet_tick.rs`, `done_policy::done_marker`) exactly
/// like the fail-open cases this function exists to catch. `cmd_fleet_set_loop`
/// trims once and passes that same value both here and to the merge, so
/// what was validated is what gets stored.
fn validate_loop_fields(
    trigger: Option<&str>,
    max_iterations: Option<u32>,
    deadline: Option<&str>,
    budget_usd: Option<f64>,
    done_when: Option<&str>,
) -> Result<()> {
    if let Some(t) = trigger {
        if let Some(expr) = t.strip_prefix("cron:") {
            if mur_agent_runtime::scheduler::next_fire_after(expr, chrono::Local::now()).is_none() {
                bail!(
                    "cron expression {expr:?} must be a 5-field POSIX schedule like \
                     \"0 9 * * 1-5\" that can actually fire — a day/month combination \
                     naming no real date (e.g. Feb 31) will never fire"
                );
            }
        } else if let Some(dur) = t.strip_prefix("interval:") {
            if super::loop_run::parse_duration(dur).is_none() {
                bail!("interval {dur:?} must be a duration like 30s, 5m, 2h, 1d");
            }
        } else if t != "manual" {
            bail!("trigger must be manual, interval:<dur>, or cron:<5-field expression>");
        }
    }

    if let Some(n) = max_iterations
        && n == 0
    {
        bail!("max_iterations must be at least 1 (0 silently falls back to the default)");
    }

    if let Some(d) = deadline
        && !d.is_empty()
        && super::loop_run::parse_duration(d).is_none()
    {
        bail!(
            "deadline {d:?} must be a duration relative to the loop start \
             (30s, 5m, 2h, 1d) — not a calendar date"
        );
    }

    if let Some(b) = budget_usd
        && b < 0.0
    {
        bail!("budget_usd must be zero or positive");
    }

    if let Some(dw) = done_when {
        let recognised = dw.is_empty()
            || dw == super::done_policy::DONE_WHEN_QUEUE_EMPTY
            || super::done_policy::done_marker(dw).is_some();
        if !recognised {
            bail!(
                "done_when must be empty (router decides), {}, or marker:<TEXT>",
                super::done_policy::DONE_WHEN_QUEUE_EMPTY
            );
        }
    }

    Ok(())
}

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
    // Trim once and reuse the trimmed value for both validation and the
    // merge below, so "what was validated is what gets stored". Deadline is
    // included even though `parse_duration` already trims internally on
    // both write and read — the invariant should hold for all three string
    // fields rather than for two of three by coincidence.
    let trigger = trigger.map(|t| t.trim().to_string());
    let deadline = deadline.map(|d| d.trim().to_string());
    let done_when = done_when.map(|dw| dw.trim().to_string());

    validate_loop_fields(
        trigger.as_deref(),
        max_iterations,
        deadline.as_deref(),
        budget_usd,
        done_when.as_deref(),
    )?;

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

    #[test]
    fn validate_rejects_values_that_would_be_silently_reinterpreted() {
        // A calendar date parses as no duration at all, which means "no
        // deadline enforced" — the exact shape mur-common's fixture carries.
        assert!(validate_loop_fields(None, None, Some("2026-12-31"), None, None).is_err());
        // 0 is filtered out by effective_max_iterations and becomes the default.
        assert!(validate_loop_fields(None, Some(0), None, None, None).is_err());
        // Feb 31 never comes: valid syntax, no future firing, silent no-op.
        assert!(validate_loop_fields(Some("cron:0 9 31 2 *"), None, None, None, None).is_err());
        assert!(validate_loop_fields(Some("cron:not a cron"), None, None, None, None).is_err());
        assert!(validate_loop_fields(Some("interval:tuesday"), None, None, None, None).is_err());
        assert!(validate_loop_fields(Some("whenever"), None, None, None, None).is_err());
        // Missing the marker: prefix means router judgment, not marker matching.
        assert!(validate_loop_fields(None, None, None, None, Some("DONE")).is_err());
        // A negative budget is filtered out and becomes "no ceiling".
        assert!(validate_loop_fields(None, None, None, Some(-1.0), None).is_err());
    }

    #[test]
    fn validate_accepts_the_three_done_policies_and_real_schedules() {
        assert!(validate_loop_fields(None, None, None, None, Some("")).is_ok());
        assert!(validate_loop_fields(None, None, None, None, Some("queue-empty")).is_ok());
        assert!(validate_loop_fields(None, None, None, None, Some("marker:DONE")).is_ok());
        assert!(validate_loop_fields(Some("manual"), None, None, None, None).is_ok());
        assert!(validate_loop_fields(Some("interval:30m"), None, None, None, None).is_ok());
        assert!(validate_loop_fields(Some("cron:*/15 * * * *"), None, None, None, None).is_ok());
        assert!(validate_loop_fields(None, Some(1), Some("2h"), Some(0.0), None).is_ok());
        // An empty deadline is "no deadline", which is a legitimate value.
        assert!(validate_loop_fields(None, None, Some(""), None, None).is_ok());
    }

    #[test]
    fn set_loop_judges_only_the_fields_passed_in_this_call() {
        // A fleet already holding a bad deadline must still accept an unrelated
        // partial update. Validating the merged block instead of the arguments
        // would make one stale value block every future edit.
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

        let mut f = store::load_fleet(home, "dev").unwrap();
        f.loop_cfg = Some(FleetLoop {
            trigger: "manual".into(),
            max_iterations: 0,
            budget_usd: 0.0,
            deadline: "2026-12-31".into(),
            done_when: "all_tasks_done".into(),
        });
        store::save_fleet(home, &f).unwrap();

        cmd_fleet_set_loop(home, "dev", None, None, None, Some(2.0), None).unwrap();

        let l = store::load_fleet(home, "dev").unwrap().loop_cfg.unwrap();
        assert_eq!(l.budget_usd, 2.0, "the field we passed was written");
        assert_eq!(
            l.deadline, "2026-12-31",
            "a pre-existing bad value is left alone, not rejected"
        );
    }

    #[test]
    fn set_loop_stores_the_trimmed_value_it_validated() {
        // A value with stray whitespace must not pass validation against a
        // trimmed copy and then persist with the whitespace still attached:
        // that form fails every `strip_prefix` read downstream
        // (`fleet_tick.rs`, `done_policy::done_marker`) and silently falls
        // back exactly like the fail-open cases this validator exists to
        // catch. A leading/trailing space is an ordinary UX slip, not a
        // contrived input — neither the CLI nor the Hub trims before calling.
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

        cmd_fleet_set_loop(
            home,
            "dev",
            Some(" interval:30m ".to_string()),
            None,
            None,
            None,
            Some("  marker:DONE  ".to_string()),
        )
        .unwrap();

        let l = store::load_fleet(home, "dev").unwrap().loop_cfg.unwrap();
        assert_eq!(l.trigger, "interval:30m", "no leading/trailing whitespace");
        assert_eq!(l.done_when, "marker:DONE", "no leading/trailing whitespace");
    }
}
