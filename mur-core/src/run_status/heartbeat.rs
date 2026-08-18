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
///
/// Goes through `store::update` rather than its own load+save pair so the
/// terminal check and the write happen under the same exclusive lock as
/// every other writer of this run (in particular a separate `mur job stop
/// <run_id>` process). A load-then-save outside a lock can always act on a
/// view that's gone stale by the time it saves; the check has to be made
/// fresh, inside the locked section, every time — checking before calling
/// `update` would just move the stale-view window one line earlier.
pub fn beat_once(mur_home: &Path, run_id: &str, now: DateTime<Utc>) -> Result<()> {
    store::update(mur_home, run_id, |run| {
        if !run.state.is_terminal() {
            run.last_heartbeat_at = Some(now);
        }
    })?;
    Ok(())
}

/// Handle to a background ticker. Dropping it also stops the ticker, so a
/// panicking executor cannot leave a run beating forever.
pub struct Heartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Heartbeat {
    /// Start beating `run_id` every `interval` until `stop` (or drop).
    pub fn spawn(mur_home: PathBuf, run_id: String, interval: std::time::Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = tokio::spawn(async move {
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
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop beating and wait for any in-flight beat to land before
    /// returning. This is what makes it safe to call a terminal
    /// `store::save` right after `stop().await`: `beat_once` is synchronous,
    /// so `abort()` cannot interrupt it mid-write — cancellation only takes
    /// effect at the task's next `.await` point (the next `ticker.tick()`).
    /// A beat already in flight therefore always finishes before the task
    /// ends, so it can never land *after* — and clobber — the caller's
    /// terminal write. In practice this returns immediately: the ticker is
    /// parked on `tick()` almost all the time.
    pub async fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.abort();
            // `abort()` makes the task resolve to `Err(JoinError)` unless it
            // had already returned on its own; either way there is nothing
            // actionable to do with the result here.
            let _ = handle.await;
        }
    }
}

impl Drop for Heartbeat {
    /// Best-effort only: flips the flag but — unlike `stop()` — cannot await
    /// the task, since `drop` isn't async. An in-flight beat may still land
    /// after this returns. That's fine: `Drop` firing without a prior
    /// `stop().await` means the executor panicked or returned early, i.e.
    /// the process is on its way down. A dead pid classifies as
    /// `Liveness::Dead` regardless of how fresh the heartbeat looks, so
    /// there's no lying-alive risk on this path — only the awaited `stop()`
    /// path needs to close the race against a deliberate terminal write.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::Liveness;
    use crate::run_status::store;
    use crate::run_status::{RUN_SCHEMA, RunKind, RunState, State, classify};

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

    /// The cross-process version of the race Phase 3 closed in-process.
    /// `Heartbeat::stop().await` only orders things within one Tokio
    /// runtime; it says nothing about a *separate* `mur job stop <run_id>`
    /// process racing this one's ticker on the same `run.json`. Since
    /// `beat_once` now goes through `store::update`, every call — including
    /// one whose in-memory view was taken before the stopping writer ran —
    /// re-reads under the same exclusive lock the stopping writer used, so
    /// its terminal check is always evaluated against current-at-lock-time
    /// data, never a stale pre-lock snapshot.
    ///
    /// That serialization is what makes this deterministic rather than
    /// probabilistic: `update` performs the full load-check-mutate-save as
    /// one atomic section, so there is no window in which a `beat_once` call
    /// can observe pre-`Stopped` state yet still land *after* the `Stopped`
    /// write — every `update` call on this run has a definite place in one
    /// total order, and once `Stopped` lands, every later slot's fresh read
    /// sees it. What is proven here empirically (real OS threads, so
    /// genuinely preemptible, unlike a single-threaded tokio test) is that
    /// hammering `beat_once` concurrently with the `Stopped` write cannot
    /// observably clobber it — not merely "usually doesn't". What is *not*
    /// proven: an exact, reproduced instant-for-instant interleaving of one
    /// specific stale read racing one specific write — with a real OS
    /// scheduler that is not constructible deterministically, only
    /// statistically pressured for. This test applies that pressure for the
    /// duration of the run rather than for a single fixed instant.
    #[test]
    fn a_beat_once_with_a_stale_view_cannot_undo_a_concurrent_stop() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");
        let mur_home = tmp.path().to_path_buf();

        let stop_beating = Arc::new(AtomicBool::new(false));
        let beater_flag = stop_beating.clone();
        let beater_home = mur_home.clone();
        let beater = thread::spawn(move || {
            // Hammer beat_once from a separate OS thread — standing in for a
            // separate `mur job stop` process's writer, which likewise has
            // no in-process handle to synchronize against, only the file
            // lock. Each of these calls independently loads a fresh view
            // under its own `update` lock acquisition, so some of them are
            // guaranteed to have their in-memory RunState computed before
            // the main thread's Stopped write is visible on disk — the
            // "stale view" the test name refers to.
            while !beater_flag.load(Ordering::Relaxed) {
                let _ = beat_once(&beater_home, "r", chrono::Utc::now());
            }
        });

        // Let the beater accumulate a healthy number of stale-view attempts
        // before the terminal write lands.
        thread::sleep(std::time::Duration::from_millis(30));

        let applied = store::update(&mur_home, "r", |run| {
            run.state = State::Stopped;
        })
        .unwrap();
        assert!(applied, "the seeded record must still be there to update");

        // Give the beater every chance to land a clobbering write after the
        // Stopped write before we tell it to stop.
        thread::sleep(std::time::Duration::from_millis(30));
        stop_beating.store(true, Ordering::Relaxed);
        beater.join().unwrap();

        let after = store::load(&mur_home, "r").unwrap().unwrap();
        assert_eq!(
            after.state,
            State::Stopped,
            "a beat_once with a stale view landed after the Stopped write \
             and reverted it — store::update did not exclude"
        );
        let status = classify(after, chrono::Utc::now(), chrono::Duration::seconds(30));
        assert_ne!(
            status.liveness,
            Liveness::Alive,
            "a stopped run must never classify as alive, no matter how \
             fresh a racing heartbeat looked"
        );
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

        hb.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let after_stop = store::load(tmp.path(), "r").unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let later = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(
            after_stop.last_heartbeat_at, later.last_heartbeat_at,
            "ticker kept beating after stop"
        );
    }

    /// `Drop` without a prior `stop().await` is the panic / early-return path.
    /// It is documented as best-effort — it flips the flag but cannot await
    /// the task — and the safety argument for that rests on the ticker
    /// actually stopping. Nothing pinned it, so a later change that dropped
    /// the flag write would leave an orphan task beating a record forever
    /// while its process went down.
    #[tokio::test]
    async fn dropping_without_stop_still_ends_the_ticker() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");

        {
            let _hb = Heartbeat::spawn(
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
        } // dropped here — no `stop().await`

        // `Drop` cannot await, so a beat already in flight may still land.
        // Let it, then take the reading everything after must match.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let after_drop = store::load(tmp.path(), "r").unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let later = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(
            after_drop.last_heartbeat_at, later.last_heartbeat_at,
            "ticker kept beating after being dropped without stop()"
        );
    }

    /// Reproduces the exact failure this module exists to prevent: if
    /// `stop()` only flipped the flag and returned immediately (the old
    /// fire-and-forget version), a beat already in flight when the caller
    /// writes the terminal state can land *after* that write — reverting
    /// `state` back to `Running` and stamping a fresh heartbeat on top.
    /// `classify` checks `is_terminal()` before it ever looks at the
    /// heartbeat, so a clobbered record would then report `Liveness::Alive`:
    /// a finished run reported alive, permanently.
    // Multi-threaded: the race this test proves closed needs a beat that is
    // genuinely running on another OS thread while this task writes the
    // terminal state. Under the default current-thread flavor, `beat_once`
    // (fully synchronous, no internal `.await`) can never be preempted by
    // this task, so the flag store in even a fire-and-forget `stop()` is
    // unconditionally visible before the ticker can run again — the race
    // could not occur no matter how many times this test ran. That matches
    // mur-core's production runtime, which also enables `rt-multi-thread`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_awaits_in_flight_beat_so_terminal_write_survives() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r");

        let interval = std::time::Duration::from_millis(20);
        let hb = Heartbeat::spawn(tmp.path().to_path_buf(), "r".into(), interval);

        // Let it beat at least once before stopping.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let beat_at_least_once = store::load(tmp.path(), "r").unwrap().unwrap();
        assert!(
            beat_at_least_once.last_heartbeat_at.is_some(),
            "ticker never beat — this test can't prove anything about the race"
        );

        hb.stop().await;

        // The terminal write happens immediately after stop() returns —
        // exactly the ordering Task 4's executor uses.
        let mut done = store::load(tmp.path(), "r").unwrap().unwrap();
        done.state = State::Done;
        store::save(tmp.path(), &done).unwrap();

        // Give a fire-and-forget stop() every chance to clobber it: several
        // ticker intervals, not just one.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        let after = store::load(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(
            after.state,
            State::Done,
            "a beat landed after stop().await returned and clobbered the terminal write"
        );

        let status = classify(after, chrono::Utc::now(), chrono::Duration::seconds(30));
        assert_ne!(
            status.liveness,
            Liveness::Alive,
            "a finished run must never classify as alive, no matter how fresh its heartbeat looks"
        );
    }
}
