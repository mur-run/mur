//! Phase 3 kill-switch. A `~/.mur/fleets/<name>/.stopped` sentinel disables a
//! fleet: a running loop bails at the next iteration, the daemon won't auto-run
//! it, and a manual `run`/`--loop` refuses. Cheap, cooperative, no commander
//! dependency. `mur fleet {stop,start} <name>`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::store;

/// Sentinel file marking a fleet as stopped/disabled.
pub fn stopped_path(mur_home: &Path, name: &str) -> PathBuf {
    store::fleet_dir(mur_home, name).join(".stopped")
}

/// Is the fleet's kill-switch engaged?
pub fn is_stopped(mur_home: &Path, name: &str) -> bool {
    stopped_path(mur_home, name).exists()
}

/// `mur fleet stop <name>` — engage the kill-switch (idempotent). `load_fleet`
/// validates the name (slug) and that the fleet exists.
pub fn cmd_fleet_stop(mur_home: &Path, name: &str) -> Result<()> {
    let _ = store::load_fleet(mur_home, name)?;
    std::fs::write(stopped_path(mur_home, name), "stopped\n")?;
    println!(
        "Fleet '{name}' stopped: auto-run disabled and any running loop halts next iteration. Re-enable with `mur fleet start {name}`."
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
                team_id: None,
                rules: vec![],
                skills: vec![],
                loop_cfg: None,
                parallel: None,
                requires_programs: vec![],
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

    #[test]
    fn stop_unknown_fleet_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(cmd_fleet_stop(tmp.path(), "nope").is_err());
    }
}
