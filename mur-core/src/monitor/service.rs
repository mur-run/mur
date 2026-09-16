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

// `drain_actions` and its supporting types live in their own module —
// moved out of this file as pure code movement to stay under CLAUDE.md's
// 800-line-per-file rule (see `drain_actions.rs`'s module doc). Callers
// reach them at `mur_core::monitor::drain_actions` rather than through a
// re-export here: `mur-core` builds as both a lib and a bin from the same
// sources, and a `pub use` with no consumer inside the bin target trips
// `unused_imports` under `-D warnings` even though the lib's external
// consumers do use it.

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

/// Notifications drained per channel per tick. Bounded so a backlog after
/// downtime spreads across ticks instead of firing a hundred banners at
/// once. Per channel is the unit that matters: this caps what a user can
/// actually be interrupted by, and the same events queued for `log` cost
/// nobody an interruption.
pub const DRAIN_MAX_PER_TICK: usize = 20;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub delivered: usize,
    pub failed: usize,
    pub parked: usize,
}

/// Deliver what the tick recorded. Never creates a store (a user who has
/// never run `mur monitor` acquires nothing) and never fails the caller: a
/// delivery problem is recorded on the queue and retried, per spec §錯誤處理
/// ("notification failure: 不回滾已完成 action").
pub fn drain_notifications(mur_home: &Path, now: DateTime<Utc>) -> Result<DrainReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        return Ok(DrainReport::default());
    };
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let registry = super::notify::registry_from_config(&cfg.notifications);
    drain_with(&store, &registry, now)
}

/// The drain loop itself, seamed out from `drain_notifications` so a test
/// can inject a registry holding a channel that fails on purpose — there is
/// no other way to reach the `Err(reason)` arm below, since
/// `drain_notifications` always builds its registry from `config.yaml`.
/// Keep `drain_notifications`'s signature and behavior unchanged by this
/// split: it still opens the store and reads config; this function only
/// runs the loop.
pub(crate) fn drain_with(
    store: &MonitorStore,
    registry: &super::notify::ChannelRegistry,
    now: DateTime<Utc>,
) -> Result<DrainReport> {
    let mut rep = DrainReport::default();
    for channel in registry.iter() {
        for p in store.pending_notifications(channel.name(), now, DRAIN_MAX_PER_TICK)? {
            // §通知策略's 「執行過的動作」 field. Scoped to the row's own cycle,
            // so a monitor that was retried does not report the previous
            // episode's actions as this one's.
            let actions: Vec<_> = store
                .actions_for(&p.row.id)?
                .into_iter()
                .filter(|a| a.cycle_id == p.row.cycle_id)
                .collect();
            let n = mur_monitor::notify::render(&p.row, &p.event, &actions);
            match channel.deliver(&n) {
                Ok(()) => {
                    store.mark_delivered(p.event_id, channel.name(), now)?;
                    rep.delivered += 1;
                }
                Err(reason) => {
                    let state = store.mark_delivery_failed(p.event_id, channel.name(), now)?;
                    tracing::warn!(
                        channel = channel.name(),
                        event_id = p.event_id,
                        %reason,
                        "monitor notification delivery failed"
                    );
                    match state {
                        mur_monitor::store::DeliveryState::Failed => rep.parked += 1,
                        _ => rep.failed += 1,
                    }
                }
            }
        }
    }
    Ok(rep)
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

    /// Same rule as `tick_once_against_a_home_with_no_store_creates_nothing`:
    /// a user who has never run `mur monitor` acquires no database, even by
    /// draining. Asserts on the filesystem, not only the returned report —
    /// an empty `DrainReport` would come back either way (nothing pending
    /// either), which would pass without the fix.
    #[test]
    fn a_home_with_no_store_drains_nothing_and_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dir = mur_monitor::store::db_dir(d.path());
        assert!(!dir.exists());
        let r = drain_notifications(d.path(), t0()).unwrap();
        assert_eq!(r, DrainReport::default());
        assert!(!dir.exists(), "draining must not create the store");
    }

    #[test]
    fn a_notifiable_event_is_delivered_once_and_not_again() {
        let d = tempfile::tempdir().unwrap();
        MonitorStore::open(d.path()).unwrap();
        // Prime "log" before any notifiable event exists — this is what
        // "the channel has been watching since before this event" looks
        // like in production, where the daemon's log channel is registered
        // long before a monitor stalls. Finding 1's fix stamps a channel's
        // high-water mark at its first-ever call, so without this priming
        // call the "created"/"stalled" events below would already be
        // history by the time `drain_notifications` first runs, and
        // `first.delivered` would wrongly come back 0.
        drain_notifications(d.path(), t0()).unwrap();

        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec(), t0(), None).unwrap().id;
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0())
            .unwrap();
        drop(s);

        let first = drain_notifications(d.path(), t0()).unwrap();
        assert_eq!(first.delivered, 1);
        let second = drain_notifications(d.path(), t0()).unwrap();
        assert_eq!(
            second.delivered, 0,
            "a delivered notification must not repeat"
        );
    }

    #[test]
    fn bookkeeping_events_are_never_delivered() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec(), t0(), None).unwrap().id; // writes `created`
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        s.append_event(
            &id,
            &cyc,
            "lease_recovered",
            serde_json::json!({}),
            false,
            t0(),
        )
        .unwrap();
        drop(s);
        assert_eq!(
            drain_notifications(d.path(), t0()).unwrap(),
            DrainReport::default()
        );
    }

    /// A `Channel` that always fails, for finding 2's seam: exercising the
    /// `Err(reason)` branch of `drain_with` requires a registry
    /// `drain_notifications` cannot build (it always reads real channels
    /// from config), so these tests call `drain_with` directly instead.
    struct AlwaysFails;
    impl crate::monitor::notify::Channel for AlwaysFails {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn deliver(&self, _n: &mur_monitor::notify::Notification) -> Result<(), String> {
            Err("synthetic failure".into())
        }
    }

    fn broken_registry() -> crate::monitor::notify::ChannelRegistry {
        let mut r = crate::monitor::notify::ChannelRegistry::new();
        r.register(Box::new(AlwaysFails));
        r
    }

    /// Spec §錯誤處理's only stated behavior for notifications: a delivery
    /// failure is recorded and retried, and never fails the tick. Before
    /// this seam, nothing asserted `rep.failed`/`rep.parked` at all — a
    /// production code path with zero test coverage.
    #[test]
    fn a_failing_channel_is_recorded_and_retried_without_failing_the_tick() {
        let registry = broken_registry();
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        // Prime "broken" before the notifiable event exists — see the note
        // on `a_notifiable_event_is_delivered_once_and_not_again` for why.
        drain_with(&s, &registry, t0()).unwrap();

        let id = s.create(&spec(), t0(), None).unwrap().id;
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0())
            .unwrap();

        // `drain_with` returning `Ok` at all is half the assertion: a
        // panic-free `Err` from the channel must not propagate as an `Err`
        // from the drain.
        let rep = drain_with(&s, &registry, t0()).unwrap();
        assert_eq!(
            rep,
            DrainReport {
                delivered: 0,
                failed: 1,
                parked: 0,
            },
            "a delivery error must be counted as failed, not delivered or parked early"
        );

        // The row itself must still be there and still pending — not
        // dropped, not marked delivered, not parked after a single miss.
        let states = s.delivery_states(&id).unwrap();
        let (_, _, state, attempts) = states
            .iter()
            .find(|(_, channel, _, _)| channel == "broken")
            .expect("a delivery row must exist for the failed attempt");
        assert_eq!(*state, mur_monitor::store::DeliveryState::Pending);
        assert_eq!(*attempts, 1, "one failed attempt must be recorded");
    }

    /// The other half of spec §錯誤處理: retrying is not forever. Enough
    /// consecutive failures must park the row (using the real
    /// `DELIVERY_MAX_ATTEMPTS`, never a hardcoded count), and a parked row
    /// must stop coming back as pending.
    #[test]
    fn enough_consecutive_failures_park_the_notification() {
        let registry = broken_registry();
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        drain_with(&s, &registry, t0()).unwrap(); // prime, see note above

        let id = s.create(&spec(), t0(), None).unwrap().id;
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0())
            .unwrap();

        // Each call's `now` is a full day past the last — comfortably past
        // the backoff table's longest step (`unknown_delay`'s ceiling is
        // minutes, not hours) — so the row is always due by the next call.
        let mut rep = DrainReport::default();
        for i in 1..=mur_monitor::store::DELIVERY_MAX_ATTEMPTS {
            let now = t0() + chrono::Duration::days(i64::from(i));
            rep = drain_with(&s, &registry, now).unwrap();
        }
        assert_eq!(
            rep,
            DrainReport {
                delivered: 0,
                failed: 0,
                parked: 1,
            },
            "the attempt that reaches DELIVERY_MAX_ATTEMPTS must be counted as parked"
        );

        // A parked row must not come back as pending, however far `now`
        // moves — this is what "excluded from pending forever" means, as
        // opposed to merely "not due yet".
        let far_future = t0() + chrono::Duration::days(365);
        assert_eq!(
            drain_with(&s, &registry, far_future).unwrap(),
            DrainReport::default(),
            "a parked notification must never be retried"
        );
    }
}
