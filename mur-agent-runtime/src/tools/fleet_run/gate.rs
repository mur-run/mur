//! Who may run what: the config allowlist and the live-run slot.

/// Keep this in step with `mur_core::cmd::fleet::progress::STALE_AFTER_SECS`.
const PROGRESS_STALE_AFTER_SECS: u64 = 600;

/// Return the active run id when this fleet's single progress slot still
/// describes a fresh, unfinished run. Corrupt or old files are inert.
pub fn live_run_id(mur_home: &std::path::Path, fleet: &str) -> Option<String> {
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

/// Can a child spawned by `requester`'s runtime sign everything this run
/// writes? That child holds one key, the requester's own. The fleet's router
/// signs the agent events and the control plane signs the `System` ones
/// (`run_paused`, the stop event, HITL requests), so one key covers the run
/// only when the requester is both. Every other run is launched by the
/// daemon, whose child reads keys from disk (#2116, #2081 items 5 and 6).
///
/// The runtime and the daemon both decide with this, so a run one side sends
/// is never a run the other side refuses.
pub fn runs_locally(requester: &str, router: &str) -> bool {
    requester.eq_ignore_ascii_case(mur_channel::verify::CONTROL_PLANE_AGENT)
        && router.eq_ignore_ascii_case(requester)
}
