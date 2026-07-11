//! Phase 2b: daemon fleet auto-run. Decides which fleets are due (an
//! `interval:<dur>` trigger in `fleet.yaml` + a `.last_run` stamp) and runs each
//! due fleet's guarded loop on an isolated OS thread + fresh runtime, so the
//! loop's blocking router dial never stalls the daemon runtime. The "due"
//! decision is pure + unit-tested here; the guards + loop live in mur-core.
//! Live runs need the member agents up (operator-observable).
//!
//! Trigger grammar: `manual` (default — never auto-runs) | `interval:<dur>`
//! e.g. `interval:30m` | `cron:<expr>` where `<expr>` is a 5-field POSIX cron
//! expression (min hour dom month dow), interpreted in local time — same
//! grammar as agent lifecycle schedules. A cron fleet fires on the NEXT
//! schedule boundary after it is first seen (a baseline is stamped on first
//! sight), never spuriously on enable.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{Local, TimeZone, Utc};
use mur_core::cmd::fleet::loop_run::parse_duration;
use mur_core::cmd::fleet::store;

/// Cron catch-up grace window (seconds). The daemon polls [`tick`] on its ~30s
/// action-tick cycle (see `main.rs`); a `.last_run` older than a few of those
/// cycles is stale. Clamping the cron lower bound to this window upholds the
/// "fires on the next boundary, never spuriously on enable" invariant when a
/// `.last_run` predates a switch to `cron:` (or daemon downtime), while still
/// honoring a boundary that fell within the last poll window. ~3 tick cycles.
const CRON_CATCHUP_GRACE_SECS: u64 = 90;

/// Is a fleet with this `trigger` due, given its last run and now (unix secs)?
/// `interval:<dur>` and `cron:<expr>` can return true; `manual`/unknown → false.
/// A never-run (`None`) cron fleet is NOT due — it gets a baseline stamp first
/// (see [`establish_cron_baselines`]) so it fires on the next boundary, not now.
pub fn is_due(trigger: &str, last_run_unix: Option<u64>, now_unix: u64) -> bool {
    if let Some(dur) = trigger.strip_prefix("interval:").and_then(parse_duration) {
        return match last_run_unix {
            None => true, // never run → due
            Some(last) => now_unix.saturating_sub(last) >= dur.as_secs(),
        };
    }
    if let Some(expr) = trigger.strip_prefix("cron:") {
        let Some(last) = last_run_unix else {
            return false; // baseline established separately; not due until next boundary
        };
        // Clamp the lower bound to the catch-up grace window: a `.last_run` far
        // older than now (a stale stamp left by a prior `interval:`/`manual` run
        // before the operator switched to `cron:`, or daemon downtime) must not
        // replay long-past boundaries. Standard-cron semantics — missed runs are
        // skipped, not flooded. In steady state last_run is within one tick, so
        // this is a no-op; it only bites a genuinely stale stamp.
        let last = last.max(now_unix.saturating_sub(CRON_CATCHUP_GRACE_SECS));
        let Some(last_dt) = Local.timestamp_opt(last as i64, 0).single() else {
            return false;
        };
        // Due iff a schedule boundary falls in (last, now].
        return mur_agent_runtime::scheduler::next_fire_after(expr.trim(), last_dt)
            .map(|t| t.timestamp().max(0) as u64 <= now_unix)
            .unwrap_or(false);
    }
    false
}

fn last_run_path(mur_home: &Path, name: &str) -> PathBuf {
    store::fleet_dir(mur_home, name).join(".last_run")
}

fn read_last_run(mur_home: &Path, name: &str) -> Option<u64> {
    std::fs::read_to_string(last_run_path(mur_home, name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_last_run(mur_home: &Path, name: &str, now_unix: u64) -> Result<()> {
    std::fs::write(last_run_path(mur_home, name), now_unix.to_string())?;
    Ok(())
}

/// Stamp a `.last_run` baseline for cron fleets seen for the first time, so a
/// cron fleet fires on its NEXT schedule boundary rather than immediately (no
/// lower bound = nothing for [`is_due`] to compute "next boundary after" from).
/// Idempotent: fleets that already have a `.last_run` are left untouched.
fn establish_cron_baselines(mur_home: &Path, now_unix: u64) {
    let Ok(names) = store::list_fleets(mur_home) else {
        return;
    };
    for name in names {
        let Ok(fleet) = store::load_fleet(mur_home, &name) else {
            continue;
        };
        let is_cron = fleet
            .loop_cfg
            .as_ref()
            .map(|l| l.trigger.starts_with("cron:"))
            .unwrap_or(false);
        if is_cron
            && read_last_run(mur_home, &name).is_none()
            && let Err(e) = write_last_run(mur_home, &name, now_unix)
        {
            tracing::error!(error = %e, fleet = %name, "fleet_tick: cron baseline stamp failed");
        }
    }
}

/// Fleets due to auto-run at `now_unix`. Reads each `fleet.yaml` trigger +
/// `.last_run`; does not touch channels.
pub fn due_fleets(mur_home: &Path, now_unix: u64) -> Result<Vec<String>> {
    let mut due = vec![];
    for name in store::list_fleets(mur_home)? {
        let fleet = store::load_fleet(mur_home, &name)?;
        let lc = fleet.loop_cfg.as_ref();
        let trigger = lc.map(|l| l.trigger.as_str()).unwrap_or("manual");
        // Auto-run requires a positive budget (no unattended run without a $ ceiling)
        // and is skipped when the kill-switch is engaged.
        let has_budget = lc.map(|l| l.budget_usd > 0.0).unwrap_or(false);
        // Fail-closed: if governance_state errors (e.g. I/O, bad signature),
        // treat the fleet as halted and skip it. A zero budget ceiling is a hard
        // halt too (the loop breaks LoopStop::Budget on it), so skip those as
        // well — otherwise the daemon would re-spawn a halted fleet every tick.
        let gov = mur_core::cmd::commander::governance_state(mur_home, &name);
        let commander_halted = match gov {
            Ok(g) => g.killed || matches!(g.budget_ceiling, Some(c) if c == 0.0),
            Err(_) => true, // fail-closed
        };
        if has_budget
            && is_due(trigger, read_last_run(mur_home, &name), now_unix)
            && !mur_core::cmd::fleet::control::is_stopped(mur_home, &name)
            && !commander_halted
        {
            due.push(name);
        }
    }
    Ok(due)
}

/// One daemon cycle: find due fleets and detach each one's guarded loop onto its
/// own thread. Non-blocking. Stamps `.last_run` BEFORE launching so the next
/// tick won't re-trigger a fleet whose loop is still running.
/// Is unattended fleet auto-run enabled? OFF unless `MUR_FLEET_AUTORUN` is set
/// truthy, so the shipped daemon never runs fleets on its own without an
/// operator explicitly opting in. (Per-fleet autonomy + an enforced budget +
/// a kill-switch are the Phase-3 prerequisites for flipping this on safely.)
fn autorun_flag(v: Option<&str>) -> bool {
    matches!(v, Some(s) if s == "1" || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("yes"))
}

fn auto_run_enabled(mur_home: &Path) -> bool {
    autorun_flag(std::env::var("MUR_FLEET_AUTORUN").ok().as_deref())
        || mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
            .fleet
            .autorun
}

pub fn tick(mur_home: &Path) {
    // Safety gate: unattended auto-run is OFF by default (best-practice audit;
    // OWASP Agentic ASI06 excessive agency). Opt in with `MUR_FLEET_AUTORUN=1`.
    if !auto_run_enabled(mur_home) {
        return;
    }
    let now = Utc::now().timestamp().max(0) as u64;
    // Give never-run cron fleets a baseline so they fire on the next boundary,
    // not on this tick. (interval/manual unaffected.)
    establish_cron_baselines(mur_home, now);
    let due = match due_fleets(mur_home, now) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "fleet_tick: due_fleets failed");
            return;
        }
    };
    for name in due {
        if let Err(e) = write_last_run(mur_home, &name, now) {
            tracing::error!(error = %e, fleet = %name, "fleet_tick: stamp last_run failed; skipping");
            continue;
        }
        let home = mur_home.to_path_buf();
        let fleet = name.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, fleet = %fleet, "fleet_tick: runtime build failed");
                    return;
                }
            };
            tracing::info!(fleet = %fleet, "fleet_tick: auto-running loop");
            // None args → use the fleet.yaml loop config (max_iterations/deadline/budget).
            if let Err(e) = rt.block_on(mur_core::cmd::fleet::loop_run::cmd_fleet_run_loop(
                &home, &fleet, None, None, None,
            )) {
                tracing::error!(error = %e, fleet = %fleet, "fleet_tick: loop failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::{Fleet, FleetLoop};

    fn loop_fleet(name: &str, trigger: &str) -> Fleet {
        Fleet {
            name: name.into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: format!("fleet-{name}"),
            rules: vec![],
            skills: vec![],
            loop_cfg: Some(FleetLoop {
                trigger: trigger.into(),
                max_iterations: 3,
                budget_usd: 1.0, // positive so auto-run eligibility holds in tests
                deadline: String::new(),
                done_when: String::new(),
            }),
            team_id: None,
            parallel: None,
            requires_programs: vec![],
        }
    }

    #[test]
    fn autorun_flag_off_by_default() {
        assert!(!autorun_flag(None)); // unset → OFF (shipped default)
        assert!(!autorun_flag(Some("")));
        assert!(!autorun_flag(Some("0")));
        assert!(!autorun_flag(Some("false")));
        assert!(autorun_flag(Some("1")));
        assert!(autorun_flag(Some("true")));
        assert!(autorun_flag(Some("YES")));
    }

    #[test]
    fn auto_run_enabled_checks_env_var_or_config_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe { std::env::remove_var("MUR_FLEET_AUTORUN") };

        // neither env nor config → false
        assert!(!auto_run_enabled(home));

        // config flag alone → true
        std::fs::write(home.join("config.yaml"), "fleet:\n  autorun: true\n").unwrap();
        assert!(auto_run_enabled(home));

        // config flag false + env var set → true (env var still satisfies the gate)
        std::fs::write(home.join("config.yaml"), "fleet:\n  autorun: false\n").unwrap();
        unsafe { std::env::set_var("MUR_FLEET_AUTORUN", "1") };
        assert!(auto_run_enabled(home));

        unsafe { std::env::remove_var("MUR_FLEET_AUTORUN") };
    }

    #[test]
    fn is_due_manual_and_unknown_never_run() {
        assert!(!is_due("manual", None, 1000));
        assert!(!is_due("", None, 1000));
        assert!(!is_due("cron:* * * * *", None, 1000));
        assert!(!is_due("interval:bogus", None, 1000));
    }

    #[test]
    fn is_due_interval_respects_elapsed() {
        assert!(is_due("interval:60s", None, 1000)); // never run → due
        assert!(is_due("interval:60s", Some(1000), 1060)); // exactly elapsed
        assert!(is_due("interval:1m", Some(1000), 2000)); // past
        assert!(!is_due("interval:1m", Some(1000), 1030)); // within
        assert!(!is_due("interval:1m", Some(2000), 1000)); // clock skew → not due
    }

    #[test]
    fn is_due_cron_fires_on_boundary() {
        // `* * * * *` → every minute at :00. Minute boundaries are unix%60==0 in
        // every real timezone (whole-minute offsets), so this is TZ-independent.
        // last_run=1000 (..:40s); next boundary strictly after is 1020.
        assert!(!is_due("cron:* * * * *", Some(1000), 1010)); // before boundary
        assert!(is_due("cron:* * * * *", Some(1000), 1020)); // boundary reached
        assert!(is_due("cron:* * * * *", Some(1000), 1080)); // well past
        // never-run → not due (baseline stamped separately, then next boundary)
        assert!(!is_due("cron:* * * * *", None, 1000));
        // clock skew (last_run in the future) → next boundary > now → not due
        assert!(!is_due("cron:* * * * *", Some(2000), 1000));
        // invalid expression → not due (fail-safe)
        assert!(!is_due("cron:not a cron", Some(1000), 999999));
    }

    #[test]
    fn cron_stale_last_run_clamped_to_grace_window() {
        // A `.last_run` far older than now (e.g. left by a prior interval/manual
        // run before switching to cron, or daemon downtime) must be clamped to
        // (now - grace), so it can't replay long-past boundaries on the first
        // cron tick. Proof: a decades-stale last_run yields the SAME verdict as
        // a last_run of exactly (now - grace). TZ-independent (both sides use the
        // same Local-based boundary computation).
        let now = 1_700_000_000u64;
        let expr = "cron:0 0 * * *"; // sparse (daily) so the clamp is observable
        let clamped = is_due(expr, Some(now - CRON_CATCHUP_GRACE_SECS), now);
        assert_eq!(is_due(expr, Some(0), now), clamped); // epoch-stale ≡ clamped
        assert_eq!(is_due(expr, Some(now - 10 * 86400), now), clamped); // 10d-stale ≡ clamped
        // And it must NOT be perpetually due: a daily cron with a clamped bound
        // is not due unless midnight genuinely fell in the last grace window.
        // (We don't assert the exact bool — it's TZ-dependent — only stability.)
    }

    #[test]
    fn establish_cron_baselines_stamps_only_new_cron() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        store::save_fleet(home, &loop_fleet("cronf", "cron:* * * * *")).unwrap();
        store::save_fleet(home, &loop_fleet("intf", "interval:1m")).unwrap();
        store::save_fleet(home, &loop_fleet("manf", "manual")).unwrap();

        establish_cron_baselines(home, 5000);
        // cron fleet got a baseline; interval/manual did not
        assert_eq!(read_last_run(home, "cronf"), Some(5000));
        assert_eq!(read_last_run(home, "intf"), None);
        assert_eq!(read_last_run(home, "manf"), None);

        // idempotent: an existing baseline is not overwritten
        establish_cron_baselines(home, 9000);
        assert_eq!(read_last_run(home, "cronf"), Some(5000));
    }

    #[test]
    fn cron_fleet_not_due_immediately_after_baseline() {
        // The baseline+is_due composition: a freshly-baselined cron fleet is not
        // due on the same tick (next boundary is strictly in the future).
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        store::save_fleet(home, &loop_fleet("cronf", "cron:* * * * *")).unwrap();
        // 1000 is ..:40s; baseline at 1000, due-check at 1000 → next boundary 1020 > 1000.
        establish_cron_baselines(home, 1000);
        assert!(due_fleets(home, 1000).unwrap().is_empty());
        // at the boundary it becomes due
        assert_eq!(due_fleets(home, 1020).unwrap(), vec!["cronf".to_string()]);
    }

    #[test]
    fn due_fleets_filters_by_trigger_and_last_run() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        store::save_fleet(home, &loop_fleet("auto", "interval:1m")).unwrap();
        store::save_fleet(home, &loop_fleet("manual_one", "manual")).unwrap();

        assert_eq!(due_fleets(home, 5000).unwrap(), vec!["auto".to_string()]);

        write_last_run(home, "auto", 5000).unwrap();
        assert_eq!(read_last_run(home, "auto"), Some(5000));
        assert!(due_fleets(home, 5030).unwrap().is_empty());

        assert_eq!(due_fleets(home, 5070).unwrap(), vec!["auto".to_string()]);
    }

    #[test]
    fn due_fleets_requires_positive_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut f = loop_fleet("nobudget", "interval:1m");
        if let Some(l) = f.loop_cfg.as_mut() {
            l.budget_usd = 0.0;
        }
        store::save_fleet(home, &f).unwrap();
        // interval + not stopped, but no budget → NOT auto-run-eligible
        assert!(due_fleets(home, 5000).unwrap().is_empty());
    }

    #[test]
    fn due_fleets_skips_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        store::save_fleet(home, &loop_fleet("auto", "interval:1m")).unwrap();
        assert_eq!(due_fleets(home, 5000).unwrap(), vec!["auto".to_string()]);
        // kill-switch engaged → no longer auto-run-eligible
        mur_core::cmd::fleet::control::cmd_fleet_stop(home, "auto").unwrap();
        assert!(due_fleets(home, 5000).unwrap().is_empty());
    }

    #[test]
    fn due_fleets_skips_commander_killed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // commander identity + a due interval fleet with a budget + channel
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&cdir)
            .unwrap();
        let mut f = loop_fleet("killed", "interval:1m");
        f.channel_id = "fleet-killed".into();
        store::save_fleet(home, &f).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("killed", "mur", &[])
            .unwrap();
        mur_core::cmd::commander::cmd_commander_directive(home, "killed", "kill", None, 1000)
            .unwrap();
        // even though due + budgeted, a commander kill excludes it
        assert!(
            !due_fleets(home, 5000)
                .unwrap()
                .contains(&"killed".to_string())
        );
    }

    #[test]
    fn due_fleets_skips_zero_budget_ceiling() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&cdir)
            .unwrap();
        let f = loop_fleet("capped", "interval:1m");
        store::save_fleet(home, &f).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("capped", "mur", &[])
            .unwrap();
        mur_core::cmd::commander::cmd_commander_directive(
            home,
            "capped",
            "budget_ceiling",
            Some(0.0),
            1000,
        )
        .unwrap();
        // a zero budget ceiling is a hard halt → excluded from auto-run
        assert!(
            !due_fleets(home, 5000)
                .unwrap()
                .contains(&"capped".to_string())
        );
    }
    // Note: the `governance_state` Err → fail-closed skip arm is defensive against
    // genuine channel-store corruption. A *missing* channel reads as Ok(empty)
    // (inert, no directives possible), so it is intentionally not unit-tested
    // here — exercising the Err arm would require corrupting the on-disk log.
}
