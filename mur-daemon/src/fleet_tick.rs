//! Phase 2b: daemon fleet auto-run. Decides which fleets are due (an
//! `interval:<dur>` trigger in `fleet.yaml` + a `.last_run` stamp) and runs each
//! due fleet's guarded loop on an isolated OS thread + fresh runtime, so the
//! loop's blocking router dial never stalls the daemon runtime. The "due"
//! decision is pure + unit-tested here; the guards + loop live in mur-core.
//! Live runs need the member agents up (operator-observable).
//!
//! Trigger grammar (lazy; no cron dependency): `manual` (default — never
//! auto-runs) | `interval:<dur>` e.g. `interval:30m`. `cron:<expr>` is a
//! future addition (the `cron` crate is already vendored elsewhere).

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use mur_core::cmd::fleet::loop_run::parse_duration;
use mur_core::cmd::fleet::store;

/// Is a fleet with this `trigger` due, given its last run and now (unix secs)?
/// Only `interval:<dur>` ever returns true; `manual`/unknown → false.
pub fn is_due(trigger: &str, last_run_unix: Option<u64>, now_unix: u64) -> bool {
    let Some(dur) = trigger.strip_prefix("interval:").and_then(parse_duration) else {
        return false;
    };
    match last_run_unix {
        None => true, // never run → due
        Some(last) => now_unix.saturating_sub(last) >= dur.as_secs(),
    }
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

/// Fleets due to auto-run at `now_unix`. Reads each `fleet.yaml` trigger +
/// `.last_run`; does not touch channels.
pub fn due_fleets(mur_home: &Path, now_unix: u64) -> Result<Vec<String>> {
    let mut due = vec![];
    for name in store::list_fleets(mur_home)? {
        let fleet = store::load_fleet(mur_home, &name)?;
        let trigger = fleet
            .loop_cfg
            .as_ref()
            .map(|l| l.trigger.as_str())
            .unwrap_or("manual");
        if is_due(trigger, read_last_run(mur_home, &name), now_unix) {
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

fn auto_run_enabled() -> bool {
    autorun_flag(std::env::var("MUR_FLEET_AUTORUN").ok().as_deref())
}

pub fn tick(mur_home: &Path) {
    // Safety gate: unattended auto-run is OFF by default (best-practice audit;
    // OWASP Agentic ASI06 excessive agency). Opt in with `MUR_FLEET_AUTORUN=1`.
    if !auto_run_enabled() {
        return;
    }
    let now = Utc::now().timestamp().max(0) as u64;
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
            // None/None → use the fleet.yaml loop config (max_iterations/deadline).
            if let Err(e) = rt.block_on(mur_core::cmd::fleet::loop_run::cmd_fleet_run_loop(
                &home, &fleet, None, None,
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
                budget_usd: 0.0,
                deadline: String::new(),
                done_when: String::new(),
            }),
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
}
