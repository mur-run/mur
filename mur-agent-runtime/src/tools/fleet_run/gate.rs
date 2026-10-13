//! Who may run what: the config allowlist and the live-run slot.

/// Keep this in step with `mur_core::cmd::fleet::progress::STALE_AFTER_SECS`.
const PROGRESS_STALE_AFTER_SECS: u64 = 600;

/// Return the active run id when this fleet's single progress slot still
/// describes a fresh, unfinished run. Corrupt or old files are inert.
pub(super) fn live_run_id(mur_home: &std::path::Path, fleet: &str) -> Option<String> {
    let path = mur_common::paths::fleet_state_dir(mur_home, fleet)
        .join(mur_common::paths::FLEET_PROGRESS_FILE);
    let body = std::fs::read(&path).ok()?;
    let progress: serde_json::Value = serde_json::from_slice(&body).ok()?;
    if !progress.get("finished_at")?.is_null() {
        return None;
    }
    let age = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .unwrap_or_default()
        .as_secs();
    (age <= PROGRESS_STALE_AFTER_SECS)
        .then(|| progress.get("run_id")?.as_str().map(str::to_owned))
        .flatten()
}

/// Is `agent` allowed to run `fleet` per the global config? Deny-by-default:
/// missing section or empty lists deny everything.
pub fn allowed(cfg: &mur_common::config::FleetRunConfig, agent: &str, fleet: &str) -> bool {
    cfg.agents.iter().any(|a| a == agent) && cfg.fleets.iter().any(|f| f == fleet)
}

/// Does `cfg` allow `agent` to run ANY fleet? Used at tool registration (on
/// the pre-seal boot snapshot, #1796) so unauthorized agents never see it.
pub fn enabled_in(cfg: &mur_common::config::FleetRunConfig, agent: &str) -> bool {
    cfg.agents.iter().any(|a| a == agent) && !cfg.fleets.is_empty()
}
