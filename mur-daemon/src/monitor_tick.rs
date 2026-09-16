//! Durable-monitor worker: recovery once at start, then one scheduler pass
//! every `TICK_INTERVAL` on a plain OS thread — rusqlite is synchronous and
//! the adapters block (a 30 s GitHub timeout at worst), so this never runs
//! on the tokio runtime. Everything it does is in `mur_core::monitor::service`.
//!
//! `handle` is the tokio runtime's own `Handle`, captured by `main` via
//! `Handle::current()` while still on the runtime and passed in here so
//! `drain_actions`'s HITL gate can `Handle::block_on` back onto it — correct
//! from a plain OS thread, and never to be moved onto the runtime itself
//! (the mirror-image bug: `block_on` called FROM a tokio worker).

use std::path::{Path, PathBuf};

use chrono::Utc;
use mur_core::monitor::drain_actions;
use mur_core::monitor::service::{self, TICK_INTERVAL};

pub fn spawn(mur_home: PathBuf, handle: tokio::runtime::Handle) {
    std::thread::Builder::new()
        .name("mur-monitor".into())
        .spawn(move || run_loop(&mur_home, &handle))
        .expect("spawn mur-monitor thread");
}

fn run_loop(mur_home: &Path, handle: &tokio::runtime::Handle) {
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
        // Space held for Task 8's future `drain_outbox` call, before
        // `tick_once` — outbox delivery is independent of this tick's own
        // observation/action work and belongs ahead of it in the sequence.

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
        // Runs right after `tick_once` and before `drain_notifications` so
        // an action that appends a notifiable event (e.g. `exhausted`,
        // `approval_required`) gets it delivered this same tick, rather
        // than waiting up to `TICK_INTERVAL` for the next one.
        match drain_actions::drain_actions(mur_home, handle, Utc::now()) {
            Ok(a) if a.executed + a.blocked + a.failed + a.exhausted > 0 => tracing::info!(
                executed = a.executed,
                blocked = a.blocked,
                failed = a.failed,
                exhausted = a.exhausted,
                "monitor actions"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "monitor action drain failed"),
        }
        match service::drain_notifications(mur_home, Utc::now()) {
            Ok(d) if d.delivered + d.failed + d.parked > 0 => tracing::info!(
                delivered = d.delivered,
                failed = d.failed,
                parked = d.parked,
                "monitor notifications"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "monitor notification drain failed"),
        }
        std::thread::sleep(TICK_INTERVAL);
    }
}
