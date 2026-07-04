//! Daily daemon auto-upgrade of origin-stamped (registry-installed) skills.
//! Mirrors `fleet_tick.rs`'s structure: a pure `is_due` check driven by a
//! `.last_run` stamp file, gated by config, with the actual fetch+upgrade
//! work spawned on its own OS thread + fresh runtime so the blocking git
//! shell-out never stalls the daemon's async runtime (same reasoning as
//! fleet loops).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

/// How often the daemon re-runs the upgrade pass.
const UPGRADE_INTERVAL_SECS: u64 = 24 * 60 * 60;

fn stamp_path(mur_home: &Path) -> PathBuf {
    mur_home.join("cache").join(".skill-upgrade-last-run")
}

fn read_last_run(mur_home: &Path) -> Option<u64> {
    std::fs::read_to_string(stamp_path(mur_home))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_last_run(mur_home: &Path, now_unix: u64) -> Result<()> {
    let path = stamp_path(mur_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, now_unix.to_string())?;
    Ok(())
}

/// Is a `.skill-upgrade-last-run` stamp missing, or older than the upgrade
/// interval? Pure function — no I/O, no config lookup — so it's trivially
/// unit-testable.
pub fn is_due(last_run_unix: Option<u64>, now_unix: u64) -> bool {
    match last_run_unix {
        None => true,
        Some(last) => now_unix.saturating_sub(last) >= UPGRADE_INTERVAL_SECS,
    }
}

fn auto_upgrade_enabled(mur_home: &Path) -> bool {
    mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
        .skills
        .auto_upgrade
}

/// Called on the daemon's ~30s action-tick cycle alongside `fleet_tick::tick`.
pub fn tick(mur_home: &Path) {
    if !auto_upgrade_enabled(mur_home) {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if !is_due(read_last_run(mur_home), now) {
        return;
    }
    if let Err(e) = write_last_run(mur_home, now) {
        tracing::error!(error = %e, "skill_upgrade_tick: stamp last_run failed; skipping");
        return;
    }

    let home = mur_home.to_path_buf();
    std::thread::spawn(move || {
        let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
            .unwrap_or_else(|_| mur_core::cmd::skill_registry::DEFAULT_REGISTRY.to_string());
        let registry_dir = match mur_core::cmd::skill_registry::fetch_registry(&home, &registry_url)
        {
            Ok(dir) => dir,
            Err(e) => {
                tracing::error!(error = %e, "skill_upgrade_tick: fetch_registry failed");
                return;
            }
        };
        let report = mur_core::cmd::skill_upgrade::upgrade_all(&home, &registry_dir, true);
        let upgraded = report
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i.status,
                    mur_core::cmd::skill_upgrade::UpgradeStatus::Upgraded { .. }
                )
            })
            .count();
        tracing::info!(
            upgraded,
            total = report.items.len(),
            "skill_upgrade_tick: auto-upgrade pass complete"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_stamp_is_due() {
        assert!(is_due(None, 1_000_000));
    }

    #[test]
    fn just_stamped_is_not_due() {
        assert!(!is_due(Some(1_000_000), 1_000_000));
        assert!(!is_due(Some(1_000_000), 1_000_000 + 60));
    }

    #[test]
    fn stamp_older_than_24h_is_due() {
        let last = 1_000_000;
        let twenty_five_hours = 25 * 60 * 60;
        assert!(is_due(Some(last), last + twenty_five_hours));
    }
}
