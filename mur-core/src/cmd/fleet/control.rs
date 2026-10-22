//! Phase 3 kill-switch. A `~/.mur/fleets/<name>/.stopped` sentinel disables a
//! fleet: a running run starts no further steps and cancels the member turns in
//! flight (#2036, via [`stop_signal`]), the daemon won't auto-run
//! it, and a manual `run`/`--loop` refuses. Cheap, cooperative, no commander
//! dependency. `mur fleet {stop,start} <name>`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::store;

/// Sentinel file marking a fleet as stopped/disabled.
pub fn stopped_path(mur_home: &Path, name: &str) -> PathBuf {
    store::fleet_dir(mur_home, name).join(".stopped")
}

/// File name of the pause sentinel, beside [`stopped_path`]'s `.stopped`.
pub const PAUSED_FILE: &str = ".paused";

/// Sentinel naming the run an operator asked to pause (pause/resume design
/// §5.1). Operator-written only: `fleets/` is not writable by a sandboxed run,
/// and a run must not be able to un-pause itself any more than un-stop itself.
pub fn paused_path(mur_home: &Path, name: &str) -> PathBuf {
    store::fleet_dir(mur_home, name).join(PAUSED_FILE)
}

/// The `run_id` the pause sentinel targets, if the sentinel exists and parses.
pub fn paused_run_id(mur_home: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(paused_path(mur_home, name)).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("run_id:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Is `run_id` the run the fleet's pause sentinel names? A sentinel for another
/// run (a stale one) never pauses this one.
pub fn is_paused(mur_home: &Path, name: &str, run_id: &str) -> bool {
    paused_run_id(mur_home, name).is_some_and(|id| id == run_id)
}

/// Does the fleet carry any pause sentinel, whichever run it names? The daemon
/// uses this to leave a paused fleet alone.
// Called by `mur-daemon` (a separate crate), so the `mur` bin target sees no use.
#[allow(dead_code)]
pub fn is_paused_any(mur_home: &Path, name: &str) -> bool {
    paused_path(mur_home, name).exists()
}

/// Is the fleet's kill-switch engaged?
pub fn is_stopped(mur_home: &Path, name: &str) -> bool {
    stopped_path(mur_home, name).exists()
}

/// The kill-switch as a [`StopSignal`](crate::executor::dag::StopSignal) a
/// DAG run polls at step boundaries and during in-flight member turns
/// (#2036). One `stat` per poll.
pub fn stop_signal(mur_home: &Path, name: &str) -> crate::executor::dag::StopSignal {
    let (home, name) = (mur_home.to_path_buf(), name.to_string());
    std::sync::Arc::new(move || is_stopped(&home, &name))
}

/// Stop or pause, as one signal a DAG run polls. Stop is checked first, so a
/// run with both sentinels set halts as `Stop` (design §6).
pub fn halt_signal(mur_home: &Path, name: &str, run_id: &str) -> crate::executor::dag::HaltSignal {
    use crate::executor::dag::HaltReason;
    let (home, name, run_id) = (mur_home.to_path_buf(), name.to_string(), run_id.to_string());
    std::sync::Arc::new(move || {
        if is_stopped(&home, &name) {
            Some(HaltReason::Stop)
        } else if is_paused(&home, &name, &run_id) {
            Some(HaltReason::Pause)
        } else {
            None
        }
    })
}

/// `mur fleet stop <name>` — engage the kill-switch (idempotent). `load_fleet`
/// validates the name (slug) and that the fleet exists.
pub fn cmd_fleet_stop(mur_home: &Path, name: &str) -> Result<()> {
    let _ = store::load_fleet(mur_home, name)?;
    std::fs::write(stopped_path(mur_home, name), "stopped\n")?;
    println!(
        "Fleet '{name}' stopped: auto-run disabled; a running run starts no further steps and cancels member turns in flight. Re-enable with `mur fleet start {name}`."
    );
    Ok(())
}

/// `mur fleet start <name>` — clear the kill-switch (idempotent).
pub fn cmd_fleet_start(mur_home: &Path, name: &str) -> Result<()> {
    let _ = store::load_fleet(mur_home, name)?;
    let p = stopped_path(mur_home, name);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    println!("Fleet '{name}' started: kill-switch cleared.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;

    fn save_dev(home: &Path) {
        store::save_fleet(
            home,
            &Fleet {
                name: "dev".into(),
                display_name: String::new(),
                goal: "g".into(),
                router: None,
                members: vec!["pm".into()],
                channel_id: "fleet-dev".into(),
                procedure: vec![],
                team_id: None,
                rules: vec![],
                skills: vec![],
                loop_cfg: None,
                parallel: None,
                hitl: None,
                requires_programs: vec![],
                limits: None,
                needs: vec![],
            },
        )
        .unwrap();
    }

    #[test]
    fn stop_start_toggles_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        save_dev(home);
        assert!(!is_stopped(home, "dev"));
        cmd_fleet_stop(home, "dev").unwrap();
        assert!(is_stopped(home, "dev"));
        cmd_fleet_stop(home, "dev").unwrap(); // idempotent
        assert!(is_stopped(home, "dev"));
        cmd_fleet_start(home, "dev").unwrap();
        assert!(!is_stopped(home, "dev"));
        cmd_fleet_start(home, "dev").unwrap(); // idempotent on absent
        assert!(!is_stopped(home, "dev"));
    }

    fn write_pause(home: &Path, run_id: &str) {
        std::fs::write(
            paused_path(home, "dev"),
            format!("run_id: {run_id}\nrequested_at: t\n"),
        )
        .unwrap();
    }

    #[test]
    fn is_paused_matches_only_the_run_it_names() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        save_dev(home);
        assert!(!is_paused(home, "dev", "r1"));
        assert!(!is_paused_any(home, "dev"));
        write_pause(home, "r1");
        assert!(is_paused(home, "dev", "r1"));
        // A stale sentinel for another run never pauses this one.
        assert!(!is_paused(home, "dev", "r2"));
        assert!(is_paused_any(home, "dev"));
        assert_eq!(paused_run_id(home, "dev").as_deref(), Some("r1"));
    }

    #[test]
    fn halt_signal_fires_on_either_sentinel_and_reports_stop_over_pause() {
        use crate::executor::dag::HaltReason;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        save_dev(home);
        let sig = halt_signal(home, "dev", "r1");
        assert_eq!(sig(), None);
        write_pause(home, "r1");
        assert_eq!(sig(), Some(HaltReason::Pause));
        // Both set: stop is reported, never pause.
        cmd_fleet_stop(home, "dev").unwrap();
        assert_eq!(sig(), Some(HaltReason::Stop));
        // Negative control: a pause for another run does not fire.
        cmd_fleet_start(home, "dev").unwrap();
        assert_eq!(halt_signal(home, "dev", "r2")(), None);
    }

    #[test]
    fn stop_unknown_fleet_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(cmd_fleet_stop(tmp.path(), "nope").is_err());
    }
}
