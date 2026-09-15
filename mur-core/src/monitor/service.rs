//! What the daemon calls. Opens the store, builds the registry, runs one
//! scheduler pass. Kept in `mur-core` (not the daemon) so the CLI tests and
//! the daemon exercise the identical assembly — one derivation, many
//! surfaces.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_monitor::scheduler::{self, RecoveryReport, TickReport};
use mur_monitor::store::MonitorStore;

/// How often the daemon thread wakes. Well inside `DEFAULT_LEASE` so a
/// slow tick never lets its own leases expire under it.
pub const TICK_INTERVAL: Duration = Duration::from_secs(15);
/// Per-tick claim bound — the startup throttle (spec §daemon 恢復).
pub const TICK_MAX_CLAIMS: usize = 8;

pub fn tick_once(mur_home: &Path, now: DateTime<Utc>, owner: &str) -> Result<TickReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        // No store yet — the common case for a daemon whose owner has never
        // run `mur monitor add`. Truthfully "claimed nothing, observed
        // nothing", not an error: an absent store is normal, not a fault,
        // and must not be the thing that creates itself just by being
        // asked. The next `mur monitor add` creates it and the very next
        // tick (<= TICK_INTERVAL later) picks it up.
        return Ok(TickReport::default());
    };
    let registry = super::registry(mur_home);
    scheduler::tick(&store, &registry, now, owner, TICK_MAX_CLAIMS)
}

pub fn recover(mur_home: &Path, now: DateTime<Utc>) -> Result<RecoveryReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        // Same reasoning as `tick_once`: no store means nothing to recover.
        return Ok(RecoveryReport::default());
    };
    scheduler::recover(&store, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{RunKind, RunState, State, store as run_store};
    use chrono::{Duration as CD, TimeZone, Utc};
    use mur_monitor::spec::MonitorSpec;
    use mur_monitor::state::{MonitorState, Outcome};
    use mur_monitor::store::MonitorStore;

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    fn run(state: State, beat: chrono::DateTime<Utc>) -> RunState {
        RunState {
            schema: 1,
            run_id: "run-1".into(),
            channel_id: None,
            kind: RunKind::Fleet,
            label: "x".into(),
            pid: std::process::id(),
            started_at: t0(),
            last_heartbeat_at: Some(beat),
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: String::new(),
            build_sha: String::new(),
        }
    }

    fn spec() -> MonitorSpec {
        MonitorSpec::from_yaml(
            "schema_version: 1\nname: e2e\nsource: { type: mur_run, reference: run-1 }\nidempotency_key: e2e\ncreated_by: { actor: user:test }\n",
        )
        .unwrap()
    }

    #[test]
    fn tick_interval_is_well_inside_the_lease() {
        assert!(TICK_INTERVAL * 4 < mur_monitor::scheduler::DEFAULT_LEASE);
    }

    /// Same bug class as the murmur footer (fix round 3): the daemon must
    /// not materialise `monitors.db` for a user who has never run `mur
    /// monitor add`, just by ticking. Asserts on the filesystem, not only
    /// the returned report — an empty `TickReport` would come back either
    /// way (an empty store's tick claims nothing too) and would pass
    /// without the fix.
    #[test]
    fn tick_once_against_a_home_with_no_store_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dir = mur_monitor::store::db_dir(d.path());
        assert!(!dir.exists(), "sanity: bare tempdir has no monitor dir yet");

        let r = tick_once(d.path(), t0(), "t").unwrap();

        assert!(
            !dir.exists(),
            "a tick against a store-less home must not create the monitor directory"
        );
        assert_eq!(r, mur_monitor::scheduler::TickReport::default());
    }

    #[test]
    fn recover_against_a_home_with_no_store_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dir = mur_monitor::store::db_dir(d.path());
        assert!(!dir.exists(), "sanity: bare tempdir has no monitor dir yet");

        let r = recover(d.path(), t0()).unwrap();

        assert!(
            !dir.exists(),
            "recovery against a store-less home must not create the monitor directory"
        );
        assert_eq!(r, mur_monitor::scheduler::RecoveryReport::default());
    }

    #[test]
    fn end_to_end_mur_run_pending_then_done_settles_once() {
        let d = tempfile::tempdir().unwrap();
        run_store::save(d.path(), &run(State::Running, t0())).unwrap();
        let id = MonitorStore::open(d.path())
            .unwrap()
            .create(&spec(), t0(), None)
            .unwrap()
            .id;

        let r1 = tick_once(d.path(), t0(), "t").unwrap();
        assert_eq!((r1.claimed, r1.observed), (1, 1));
        let row = MonitorStore::open(d.path())
            .unwrap()
            .get(&id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.state, row.outcome),
            (MonitorState::Sleeping, Outcome::Pending)
        );

        run_store::save(d.path(), &run(State::Done, t0())).unwrap();
        let r2 = tick_once(d.path(), row.next_check_at, "t").unwrap();
        assert_eq!(r2.completed, 1);
        let r3 = tick_once(d.path(), row.next_check_at + CD::hours(1), "t").unwrap();
        assert_eq!(r3.claimed, 0, "settled once, never re-observed");
        let s = MonitorStore::open(d.path()).unwrap();
        assert_eq!(
            s.events(&id)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "terminal")
                .count(),
            1
        );
    }

    #[test]
    fn daemon_offline_across_a_check_catches_up_on_restart() {
        let d = tempfile::tempdir().unwrap();
        run_store::save(d.path(), &run(State::Running, t0())).unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec(), t0(), None).unwrap().id;
        // a worker claimed it and the daemon died before writing back
        s.claim_due(t0(), "old-daemon", mur_monitor::scheduler::DEFAULT_LEASE, 8)
            .unwrap();
        drop(s);

        let restart = t0() + CD::hours(3);
        let rec = recover(d.path(), restart).unwrap();
        assert_eq!(rec.recovered_leases, vec![id.clone()]);
        assert_eq!(rec.overdue, 1);
        let r = tick_once(d.path(), restart, "new-daemon").unwrap();
        assert_eq!(r.observed, 1);
        let s = MonitorStore::open(d.path()).unwrap();
        let kinds: Vec<_> = s.events(&id).unwrap().into_iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&"lease_recovered".to_string()), "{kinds:?}");
        // Not asserted: "stalled". The correction in the task brief for this
        // test explains why: the first observation after recovery carries a
        // progress token, and `advance_progress` moves `last_progress_at` to
        // `now` when the token differs from the stored one (`None` on a
        // monitor that has never been observed), so the stall timer resets
        // on this very tick. Task 8's
        // `stalled_then_recovered_are_each_one_event` already proves the
        // stalled semantics directly.
        assert!(kinds.contains(&"soft_deadline".to_string()), "{kinds:?}");
    }
}
