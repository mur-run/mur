//! Durable-monitor worker: recovery once at start, then one scheduler pass
//! every `TICK_INTERVAL` on a plain OS thread — rusqlite is synchronous and
//! the adapters block (a 30 s GitHub timeout at worst), so this never runs
//! on the tokio runtime. Everything it does is in `mur_core::monitor::service`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use mur_core::monitor::service::{self, TICK_INTERVAL};

pub fn spawn(mur_home: PathBuf) {
    std::thread::Builder::new()
        .name("mur-monitor".into())
        .spawn(move || run_loop(&mur_home))
        .expect("spawn mur-monitor thread");
}

fn run_loop(mur_home: &Path) {
    let owner = format!("daemon-{}", std::process::id());
    match service::recover(mur_home, Utc::now()) {
        Ok(r) => tracing::info!(
            recovered_leases = r.recovered_leases.len(),
            overdue = r.overdue,
            "monitor: recovered"
        ),
        Err(e) => tracing::error!(error = %e, "monitor: recovery failed; ticking anyway"),
    }
    loop {
        match service::tick_once(mur_home, Utc::now(), &owner) {
            Ok(r) if r.claimed > 0 => tracing::info!(
                claimed = r.claimed,
                observed = r.observed,
                unknown = r.unknown,
                completed = r.completed,
                action_pending = r.action_pending,
                exhausted = r.exhausted,
                stale_fence = r.stale_fence,
                "monitor tick"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "monitor tick failed"),
        }
        std::thread::sleep(TICK_INTERVAL);
    }
}
