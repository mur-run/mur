//! Daemon-side sleep cycle — fires after `idle_threshold_minutes` of no events,
//! drains the Signal inbox into the pattern store.

use anyhow::Result;
use mur_core::store::config::load_config;
use mur_core::store::yaml::YamlStore;
use mur_core::sync::inbox::Inbox;

/// Run one sleep-cycle pass:
/// 1. Drain `~/.mur/inbox/` → apply all pending Signals to patterns.
///
/// Best-effort — errors are logged but never propagate to the event loop.
pub fn run_sleep_cycle() {
    if let Err(e) = try_run_sleep_cycle() {
        eprintln!("murmurd sleep-cycle error: {e:#}");
    }
}

fn try_run_sleep_cycle() -> Result<()> {
    let store = YamlStore::default_store()?;
    let inbox = Inbox::default_location()?;
    let report = inbox.apply_all(&store)?;
    if report.applied > 0 || report.skipped > 0 {
        eprintln!(
            "murmurd sleep-cycle: inbox drained — applied={} skipped={} errors={}",
            report.applied,
            report.skipped,
            report.errors.len()
        );
    }
    Ok(())
}

/// Returns `true` if the sleep cycle is enabled in `~/.mur/config.yaml`.
/// Defaults to `false` — opt-in only.
pub fn is_enabled() -> bool {
    load_config()
        .map(|c| c.sleep_cycle.enabled)
        .unwrap_or(false)
}

/// Idle threshold in minutes from config (default 15).
pub fn idle_threshold_minutes() -> u64 {
    load_config()
        .map(|c| c.sleep_cycle.idle_threshold_minutes)
        .unwrap_or(15)
}
