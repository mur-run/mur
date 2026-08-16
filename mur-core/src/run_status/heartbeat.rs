//! The heartbeat ticker.
//!
//! `last_heartbeat_at` is the one field `rebuild` cannot recover, which is
//! precisely why it is worth writing: it is the only evidence that separates
//! "this process is up" from "this run is moving".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::store;

/// Stamp one beat. Silently does nothing when the record is missing (the run
/// is gone — do not resurrect it) or terminal (a finished run must stop
/// looking fresh).
pub fn beat_once(mur_home: &Path, run_id: &str, now: DateTime<Utc>) -> Result<()> {
    let Some(mut run) = store::load(mur_home, run_id)? else {
        return Ok(());
    };
    if run.state.is_terminal() {
        return Ok(());
    }
    run.last_heartbeat_at = Some(now);
    store::save(mur_home, &run)
}

/// Handle to a background ticker. Dropping it also stops the ticker, so a
/// panicking executor cannot leave a run beating forever.
pub struct Heartbeat {
    stop: Arc<AtomicBool>,
}

impl Heartbeat {
    /// Start beating `run_id` every `interval` until `stop` (or drop).
    pub fn spawn(mur_home: PathBuf, run_id: String, interval: std::time::Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick fires immediately; that first beat is wanted, so
            // a run is never briefly indistinguishable from a rebuilt one.
            loop {
                ticker.tick().await;
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                // A failed beat is not fatal to the run it is observing.
                let _ = beat_once(&mur_home, &run_id, Utc::now());
            }
        });
        Self { stop }
    }

    /// Stop beating. Idempotent.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::store;
    use crate::run_status::{RUN_SCHEMA, RunKind, RunState, State};

    fn seed(mur_home: &std::path::Path, run_id: &str) {
        store::save(
            mur_home,
            &RunState {
                schema: RUN_SCHEMA,
                run_id: run_id.into(),
                channel_id: None,
                kind: RunKind::Job,
                label: "l".into(),
                pid: std::process::id(),
                started_at: chrono::Utc::now(),
                last_heartbeat_at: None,
                state: State::Running,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".into(),
                build_sha: "deadbee".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn beat_once_stamps_the_heartbeat_and_touches_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");
        let before = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(before.last_heartbeat_at.is_none());

        let now = chrono::Utc::now();
        beat_once(tmp.path(), "r", now).unwrap();

        let after = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(after.last_heartbeat_at, Some(now));
        assert_eq!(after.state, before.state, "heartbeat must not change state");
        assert_eq!(after.label, before.label);
    }

    /// A run whose record is gone must not resurrect it. Writing a fresh
    /// record here would manufacture a run that no longer exists.
    #[test]
    fn beat_once_on_a_missing_run_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        beat_once(tmp.path(), "ghost", chrono::Utc::now()).unwrap();
        assert!(store::load(tmp.path(), "ghost").unwrap().is_none());
    }

    /// A terminal run's heartbeat must stop moving — otherwise a finished run
    /// would look perpetually fresh.
    #[test]
    fn beat_once_skips_terminal_runs() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");
        let mut run = store::load(tmp.path(), "r").unwrap().unwrap();
        run.state = State::Done;
        store::save(tmp.path(), &run).unwrap();

        beat_once(tmp.path(), "r", chrono::Utc::now()).unwrap();

        let after = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(after.last_heartbeat_at.is_none(), "terminal run got a beat");
    }

    #[tokio::test]
    async fn spawned_ticker_beats_then_stops() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");

        let hb = Heartbeat::spawn(
            tmp.path().to_path_buf(),
            "r".into(),
            std::time::Duration::from_millis(20),
        );
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let while_running = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(
            while_running.last_heartbeat_at.is_some(),
            "ticker never beat"
        );

        hb.stop();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let after_stop = store::load(tmp.path(), "r").unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let later = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(
            after_stop.last_heartbeat_at, later.last_heartbeat_at,
            "ticker kept beating after stop"
        );
    }
}
