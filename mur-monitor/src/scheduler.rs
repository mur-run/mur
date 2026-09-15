//! The check loop (spec §排程, §daemon 恢復). `plan_cycle` is the entire
//! state machine as a pure function of (row, observation, now) — every rule
//! about backoff, deadlines, terminal settlement and health lives there and
//! is table-testable. `tick` is the thin I/O wrapper the daemon calls.

use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::adapter::{AdapterRegistry, Observation, SourceAdapter};
use crate::backoff::{
    RETAIN_INTERVAL, UNHEALTHY_AFTER_UNKNOWN, clamp_recommended, pending_delay, seed,
    unknown_delay, with_jitter,
};
use crate::deadline;
use crate::state::{MonitorState, Outcome};
use crate::store::{CycleUpdate, Event, ListFilter, MonitorRow, MonitorStore};

/// Longer than any single `observe` may take (GitHub client timeout is 30 s,
/// the others are local reads). A batch of `max_claims` claims is NOT
/// covered by one lease each staying alive for the whole batch — `tick`
/// beats each monitor's own lease immediately before its `observe` call, so
/// this only ever has to outlast one observe, not the sum of several.
pub const DEFAULT_LEASE: Duration = Duration::from_secs(120);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub claimed: usize,
    pub observed: usize,
    pub completed: usize,
    pub action_pending: usize,
    pub exhausted: usize,
    pub unknown: usize,
    pub stale_fence: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub recovered_leases: Vec<String>,
    pub overdue: usize,
}

fn ev(kind: &'static str, payload: serde_json::Value) -> Event {
    Event {
        kind,
        payload,
        dedup: true,
    }
}

fn plus(now: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

/// The entire scheduling state machine, as a pure function of
/// `(row, observation, now)`. No I/O, no clock read — every rule about
/// backoff, deadlines, terminal settlement and health lives here so all ten
/// tests in this module can address it directly.
pub fn plan_cycle(row: &MonitorRow, obs: Observation, now: DateTime<Utc>) -> CycleUpdate {
    let policy = &row.spec.policy;
    let mut events = Vec::new();
    let mut u = CycleUpdate {
        outcome: obs.outcome,
        observation: obs.clone(),
        observed_at: now,
        new_state: MonitorState::Sleeping,
        next_check_at: now,
        pending_attempts: row.pending_attempts,
        unknown_streak: row.unknown_streak,
        last_progress_at: row.last_progress_at,
        progress_token: row.progress_token.clone(),
        stalled_since: row.stalled_since,
        soft_notified: row.soft_notified,
        hard_reached: row.hard_reached,
        finish_cycle: false,
        events: Vec::new(),
    };

    if obs.outcome.is_terminal() {
        // Settlement. With no actions to run the monitor is done; with some,
        // it parks for the executor (plan-2). Either way it is never claimed
        // again (`MonitorState::is_claimable`), which is what makes the
        // terminal side-effect fire exactly once — requirement 3.
        u.unknown_streak = 0;
        let actions = match obs.outcome {
            Outcome::Succeeded => &row.spec.actions.on_success,
            _ => &row.spec.actions.on_failure,
        };
        u.new_state = if actions.is_empty() {
            MonitorState::Completed
        } else {
            MonitorState::ActionPending
        };
        u.finish_cycle = true;
        events.push(ev(
            "terminal",
            serde_json::json!({ "outcome": obs.outcome.as_str(), "evidence": obs.evidence }),
        ));
        u.events = events;
        return u;
    }

    let (progress_at, token) = deadline::advance_progress(
        row.last_progress_at,
        row.progress_token.as_deref(),
        obs.progress_token.as_deref(),
        now,
    );
    u.last_progress_at = progress_at;
    u.progress_token = token;
    let v = deadline::evaluate(
        policy,
        row.work_started_at,
        progress_at,
        row.stalled_since,
        row.soft_notified,
        row.hard_reached,
        now,
    );
    u.stalled_since = v.stalled_since;
    u.soft_notified = v.soft_notified;
    u.hard_reached = v.hard_reached;
    // dedup is per (monitor, cycle, kind): a second stall inside the same
    // cycle after a recovery is not re-announced. Acceptable for MVP; a
    // per-stall dedup key is the upgrade if that proves noisy in practice.
    if v.stalled_newly {
        events.push(ev(
            "stalled",
            serde_json::json!({ "since": v.stalled_since }),
        ));
    }
    if v.recovered {
        events.push(ev("stalled_recovered", serde_json::json!({ "at": now })));
    }
    if v.soft_newly {
        events.push(ev("soft_deadline", serde_json::json!({ "at": now })));
    }
    if v.hard_newly {
        events.push(ev("hard_deadline", serde_json::json!({ "at": now })));
    }

    // requirement 4: `unknown` gets its own (shorter) backoff table and never
    // settles the monitor; requirement 6: the seed uses the NEW attempt
    // count (post-increment) while the delay TABLE lookup uses the OLD
    // attempt/streak — matching the deterministic-jitter test formulas.
    let (base, attempt_seed) = if obs.outcome == Outcome::Unknown {
        u.unknown_streak = row.unknown_streak + 1;
        if u.unknown_streak == UNHEALTHY_AFTER_UNKNOWN {
            events.push(ev(
                "monitor_unhealthy",
                serde_json::json!({ "streak": u.unknown_streak, "error": obs.adapter_error }),
            ));
        }
        (unknown_delay(u.unknown_streak - 1), u.unknown_streak)
    } else {
        u.unknown_streak = 0;
        u.pending_attempts = row.pending_attempts + 1;
        (pending_delay(row.pending_attempts), u.pending_attempts)
    };

    // requirement 5: the hard deadline is not a failure verdict. Past it, a
    // monitor whose policy retains monitoring keeps polling read-only at
    // `RETAIN_INTERVAL`; one that does not goes to `Exhausted`. Neither
    // marks the work failed — `obs.outcome` (still `Pending`) is untouched.
    let base = if v.hard_reached && obs.outcome == Outcome::Pending {
        if policy.retain_monitoring_after_hard_deadline {
            RETAIN_INTERVAL
        } else {
            u.new_state = MonitorState::Exhausted;
            events.push(ev(
                "exhausted",
                serde_json::json!({ "reason": "hard deadline, monitoring not retained" }),
            ));
            base
        }
    } else {
        base
    };

    // Clamp BEFORE jitter, not after: every base above comes from a table
    // (`pending_delay`/`unknown_delay`/`RETAIN_INTERVAL`) that is already
    // >= MIN_INTERVAL, so `clamp_recommended` is a no-op here and jitter is
    // free to move within its full +/-20% band, including below the floor
    // for `unknown_delay(0) == MIN_INTERVAL`. Clamping AFTER jitter would
    // re-floor exactly that low tail and silently disagree with the
    // deterministic `with_jitter(base, seed)` the tests assert against. An
    // adapter's `recommended_poll_after`, when present, still gets floored
    // here before jitter spreads it.
    let delay = with_jitter(
        clamp_recommended(obs.recommended_poll_after, base),
        seed(&row.id, attempt_seed),
    );
    u.next_check_at = plus(now, delay);
    u.events = events;
    u
}

/// Thin I/O wrapper: claim due monitors, observe each one, plan the cycle,
/// write it back exactly once (requirement 2 — never call `apply_cycle`
/// twice for the same claim; there is no retry loop here).
pub fn tick(
    store: &MonitorStore,
    registry: &AdapterRegistry,
    now: DateTime<Utc>,
    owner: &str,
    max_claims: usize,
) -> Result<TickReport> {
    let claimed = store.claim_due(now, owner, DEFAULT_LEASE, max_claims)?;
    let mut rep = TickReport {
        claimed: claimed.len(),
        ..Default::default()
    };
    let started = Instant::now();
    for c in claimed {
        // Refresh this monitor's own lease right before its observe, so the
        // lease only has to cover one observe (worst case ~30s for the
        // GitHub adapter) rather than the whole batch of `max_claims`
        // observes running serially. `now` advanced by real elapsed time
        // keeps the stored expiry honest about wall-clock progress through
        // the batch while `now` itself stays the logical clock the rest of
        // this function (and `plan_cycle`) reasons with.
        let beat_at = now
            + chrono::Duration::from_std(started.elapsed())
                .unwrap_or_else(|_| chrono::Duration::zero());
        if !store.heartbeat(&c.row.id, c.fence, beat_at, DEFAULT_LEASE)? {
            // Someone else already reclaimed this monitor (its lease
            // expired and another worker took it, bumping the fence).
            // Observing it now would be wasted work: `apply_cycle` would
            // reject the write with the same stale fence anyway. Skip
            // straight to the next claim without calling the adapter.
            rep.stale_fence += 1;
            tracing::warn!(monitor = %c.row.id, fence = c.fence, "monitor: stale fence before observe, skipped");
            continue;
        }
        // requirement 4: a missing adapter is an `unknown` observation, not
        // a crash — `registry.get()` returning `None` never panics or bails.
        let obs = match registry.get(c.row.source_type) {
            Some(a) => a.observe(
                &c.row.reference,
                c.row.spec.source.credential_ref.as_deref(),
            ),
            None => Observation::unknown(format!(
                "no adapter enabled for {}",
                c.row.source_type.as_str()
            )),
        }
        .redacted();
        let u = plan_cycle(&c.row, obs, now);
        // requirement 2: exactly one `apply_cycle` call per claim, no retry.
        // The semantic counters below only reflect a cycle that ACTUALLY
        // LANDED: a stale fence (another worker reclaimed this monitor
        // mid-cycle) means `apply_cycle` wrote nothing at all — no
        // observation row, no column change, no event, no cycle finish —
        // so counting `u.new_state`/`u.outcome` in that case would report a
        // write that never happened on disk.
        if store.apply_cycle(&c.row.id, c.fence, &u)? {
            rep.observed += 1;
            match u.new_state {
                MonitorState::Completed => rep.completed += 1,
                MonitorState::ActionPending => rep.action_pending += 1,
                MonitorState::Exhausted => rep.exhausted += 1,
                _ => {}
            }
            if u.outcome == Outcome::Unknown {
                rep.unknown += 1;
            }
        } else {
            rep.stale_fence += 1;
            tracing::warn!(monitor = %c.row.id, fence = c.fence, "monitor: stale fence, cycle dropped");
        }
    }
    Ok(rep)
}

/// Daemon start (spec §daemon 恢復 steps 2 and 4). Step 3 (outbox) and
/// step 5 (claimed actions) are plan-2.
pub fn recover(store: &MonitorStore, now: DateTime<Utc>) -> Result<RecoveryReport> {
    let recovered_leases = store.expire_leases(now)?;
    let overdue = store
        .list(&ListFilter::default())?
        .iter()
        .filter(|r| r.state.is_claimable() && r.next_check_at <= now)
        .count();
    Ok(RecoveryReport {
        recovered_leases,
        overdue,
    })
}

// keep the trait import used when no adapter is registered in a build
#[allow(dead_code)]
fn _assert_object_safe(_: &dyn SourceAdapter) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backoff::{
        RETAIN_INTERVAL, UNHEALTHY_AFTER_UNKNOWN, pending_delay, seed, unknown_delay, with_jitter,
    };
    use crate::spec::{MonitorSpec, SourceType};
    use crate::store::tests::t0;
    use chrono::Duration as CD;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Hands back a scripted sequence; `unknown("script exhausted")` after.
    struct Scripted(Mutex<VecDeque<Observation>>);
    impl Scripted {
        fn new(v: Vec<Observation>) -> Self {
            Self(Mutex::new(v.into()))
        }
    }
    impl SourceAdapter for Scripted {
        fn source_type(&self) -> SourceType {
            SourceType::MurRun
        }
        fn validate_reference(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _: &str, _: Option<&str>) -> Observation {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Observation::unknown("script exhausted"))
        }
    }

    fn registry(obs: Vec<Observation>) -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Box::new(Scripted::new(obs)));
        r
    }

    fn spec_yaml(actions: &str, retain: bool) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: run-1 }}
actions:
{actions}
policy: {{ retain_monitoring_after_hard_deadline: {retain} }}
idempotency_key: k
created_by: {{ actor: user:test }}
"#
        ))
        .unwrap()
    }

    fn fresh(actions: &str, retain: bool) -> (tempfile::TempDir, MonitorStore, String) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s
            .create(&spec_yaml(actions, retain), t0(), None)
            .unwrap()
            .id;
        (d, s, id)
    }

    fn cd(d: std::time::Duration) -> CD {
        CD::from_std(d).unwrap()
    }

    #[test]
    fn pending_sleeps_by_the_pending_table_with_deterministic_jitter() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let rep = tick(
            &s,
            &registry(vec![Observation::pending("p1", "running")]),
            t0(),
            "w",
            8,
        )
        .unwrap();
        assert_eq!((rep.claimed, rep.observed, rep.unknown), (1, 1, 0));
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Sleeping);
        assert_eq!(r.pending_attempts, 1);
        assert_eq!(
            r.next_check_at,
            t0() + cd(with_jitter(pending_delay(0), seed(&id, 1)))
        );
        assert!(s.lease_of(&id).unwrap().is_none());
    }

    #[test]
    fn tick_beats_the_lease_before_each_observe() {
        // Proves the fix for the batch-vs-single-lease finding: `tick` must
        // refresh a monitor's own lease immediately before calling its
        // adapter, not rely on the lease `claim_due` stamped for the whole
        // batch. The adapter below opens a second connection to the same
        // store and snapshots the lease's `expires_at` from INSIDE its own
        // `observe()` call — i.e. after `tick`'s pre-observe heartbeat (if
        // any) has already run, before `apply_cycle` releases the lease.
        //
        // The adapter sleeps briefly first. Without that, the real elapsed
        // time between `claim_due` and the heartbeat is sub-millisecond,
        // and `expires_at` is stored with millisecond precision (`ts()`
        // uses `SecondsFormat::Millis`) — a beaten and an unbeaten lease
        // would then round to the identical string and the assertion could
        // pass whether or not `heartbeat` was ever called. The sleep makes
        // the two genuinely distinguishable.
        let (d, s, id) = fresh("  on_success: []", true);
        let home = d.path().to_path_buf();
        let seen: std::sync::Arc<Mutex<Option<DateTime<Utc>>>> =
            std::sync::Arc::new(Mutex::new(None));

        struct SleepyProbe {
            home: PathBuf,
            id: String,
            seen: std::sync::Arc<Mutex<Option<DateTime<Utc>>>>,
        }
        impl SourceAdapter for SleepyProbe {
            fn source_type(&self) -> SourceType {
                SourceType::MurRun
            }
            fn validate_reference(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn observe(&self, _: &str, _: Option<&str>) -> Observation {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let probe = MonitorStore::open(&self.home).unwrap();
                *self.seen.lock().unwrap() =
                    probe.lease_of(&self.id).unwrap().map(|l| l.expires_at);
                Observation::pending("p", "running")
            }
        }

        let mut reg = AdapterRegistry::new();
        reg.register(Box::new(SleepyProbe {
            home,
            id: id.clone(),
            seen: seen.clone(),
        }));

        let claim_time = t0();
        let rep = tick(&s, &reg, claim_time, "w", 8).unwrap();
        assert_eq!((rep.claimed, rep.observed, rep.stale_fence), (1, 1, 0));

        let original_expiry = claim_time + cd(DEFAULT_LEASE);
        let beat_expiry = seen
            .lock()
            .unwrap()
            .expect("observe should have seen a live lease");
        assert!(
            beat_expiry > original_expiry,
            "expected the pre-observe heartbeat to move expires_at beyond \
             the claim_due stamp: {beat_expiry} vs {original_expiry}"
        );
    }

    #[test]
    fn unknown_uses_its_own_backoff_never_fails_and_flags_health_once() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![]); // every observe → unknown("script exhausted")
        let mut now = t0();
        for i in 1..=UNHEALTHY_AFTER_UNKNOWN + 2 {
            let rep = tick(&s, &reg, now, "w", 8).unwrap();
            assert_eq!(rep.unknown, 1, "tick {i}");
            let r = s.get(&id).unwrap().unwrap();
            assert_eq!(r.outcome, Outcome::Unknown);
            assert_ne!(r.state, MonitorState::Completed);
            assert_eq!(r.unknown_streak, i);
            assert_eq!(
                r.next_check_at,
                now + cd(with_jitter(unknown_delay(i - 1), seed(&id, i)))
            );
            now = r.next_check_at;
        }
        let health = s
            .events(&id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "monitor_unhealthy")
            .count();
        assert_eq!(health, 1);
    }

    #[test]
    fn terminal_completes_once_and_is_never_reclaimed() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![
            Observation::terminal(Outcome::Succeeded, "done"),
            Observation::terminal(Outcome::Succeeded, "done again"),
        ]);
        let rep = tick(&s, &reg, t0(), "w", 8).unwrap();
        assert_eq!(rep.completed, 1);
        assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Completed);
        let rep2 = tick(&s, &reg, t0() + CD::hours(1), "w", 8).unwrap();
        assert_eq!(
            rep2.claimed, 0,
            "a completed monitor is never observed again"
        );
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
    fn terminal_with_actions_parks_in_action_pending_for_plan_2() {
        let (_d, s, id) = fresh("  on_failure:\n    - type: collect_logs", true);
        let rep = tick(
            &s,
            &registry(vec![Observation::terminal(Outcome::Failed, "exit 1")]),
            t0(),
            "w",
            8,
        )
        .unwrap();
        assert_eq!(rep.action_pending, 1);
        assert_eq!(
            s.get(&id).unwrap().unwrap().state,
            MonitorState::ActionPending
        );
        assert_eq!(
            tick(&s, &registry(vec![]), t0() + CD::hours(1), "w", 8)
                .unwrap()
                .claimed,
            0
        );
    }

    #[test]
    fn cancelled_takes_the_failure_branch() {
        let (_d, s, id) = fresh("  on_failure:\n    - type: notify", true);
        tick(
            &s,
            &registry(vec![Observation::terminal(Outcome::Cancelled, "cancelled")]),
            t0(),
            "w",
            8,
        )
        .unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(
            (r.state, r.outcome),
            (MonitorState::ActionPending, Outcome::Cancelled)
        );
    }

    #[test]
    fn stalled_then_recovered_are_each_one_event() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![
            Observation::pending("p1", "a"),
            Observation::pending("p1", "same"),
            Observation::pending("p1", "same"),
            Observation::pending("p2", "moved"),
        ]);
        tick(&s, &reg, t0(), "w", 8).unwrap();
        tick(&s, &reg, t0() + CD::minutes(21), "w", 8).unwrap();
        tick(&s, &reg, t0() + CD::minutes(60), "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.stalled_since, Some(t0() + CD::minutes(21)));
        tick(&s, &reg, t0() + CD::minutes(90), "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert!(r.stalled_since.is_none());
        let kinds: Vec<_> = s
            .events(&id)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .filter(|k| k.starts_with("stalled"))
            .collect();
        assert_eq!(kinds, vec!["stalled", "stalled_recovered"]);
    }

    #[test]
    fn hard_deadline_retains_at_two_hours_or_exhausts() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let reg = registry(vec![
            Observation::pending("p", "x"),
            Observation::pending("p", "x"),
        ]);
        tick(&s, &reg, t0(), "w", 8).unwrap();
        let now = t0() + CD::hours(8);
        tick(&s, &reg, now, "w", 8).unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert!(r.hard_reached);
        assert_eq!(r.state, MonitorState::Sleeping);
        assert_eq!(
            r.next_check_at,
            now + cd(with_jitter(RETAIN_INTERVAL, seed(&id, 2)))
        );
        assert_eq!(
            s.events(&id)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "hard_deadline")
                .count(),
            1
        );

        let (_d2, s2, id2) = fresh("  on_success: []", false);
        tick(
            &s2,
            &registry(vec![Observation::pending("p", "x")]),
            t0() + CD::hours(8),
            "w",
            8,
        )
        .unwrap();
        let r2 = s2.get(&id2).unwrap().unwrap();
        assert_eq!(r2.state, MonitorState::Exhausted);
        assert_eq!(
            s2.events(&id2)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "exhausted")
                .count(),
            1
        );
    }

    #[test]
    fn soft_deadline_is_an_event_not_a_failure() {
        let (_d, s, id) = fresh("  on_success: []", true);
        tick(
            &s,
            &registry(vec![Observation::pending("p", "x")]),
            t0() + CD::hours(3),
            "w",
            8,
        )
        .unwrap();
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.outcome, Outcome::Pending);
        assert!(r.soft_notified && !r.hard_reached);
        assert_eq!(
            s.events(&id)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "soft_deadline")
                .count(),
            1
        );
    }

    #[test]
    fn missed_check_is_caught_up_after_recovery() {
        let (_d, s, id) = fresh("  on_success: []", true);
        // worker claims and dies mid-check
        let cl = s.claim_due(t0(), "dead", DEFAULT_LEASE, 8).unwrap();
        assert_eq!(cl.len(), 1);
        let later = t0() + CD::hours(2);
        let rec = recover(&s, later).unwrap();
        assert_eq!(rec.recovered_leases, vec![id.clone()]);
        assert_eq!(rec.overdue, 1);
        assert!(
            s.events(&id)
                .unwrap()
                .iter()
                .any(|e| e.kind == "lease_recovered")
        );
        let rep = tick(
            &s,
            &registry(vec![Observation::pending("p", "x")]),
            later,
            "w2",
            8,
        )
        .unwrap();
        assert_eq!(rep.observed, 1);
    }

    #[test]
    fn missing_adapter_is_an_unknown_not_a_crash() {
        let (_d, s, id) = fresh("  on_success: []", true);
        let rep = tick(&s, &AdapterRegistry::new(), t0(), "w", 8).unwrap();
        assert_eq!((rep.observed, rep.unknown), (1, 1));
        let obs = s.observations(&id, 1).unwrap();
        assert!(
            obs[0]
                .adapter_error
                .as_deref()
                .unwrap()
                .contains("no adapter"),
            "{obs:?}"
        );
    }

    /// A one-shot adapter whose `observe` call yanks the fence out from
    /// under the monitor `tick` just claimed — by releasing the lease back
    /// to `Sleeping` and letting a second, unrelated worker reclaim it
    /// (which bumps the fence) — before returning an observation. `tick`
    /// still holds the OLD (pre-bump) fence it claimed under, so its
    /// `apply_cycle` call afterwards is exactly the stale-fence case:
    /// nothing lands on disk. The "second worker" opens its own connection
    /// to the same SQLite file (`MonitorStore::open` on the shared temp
    /// dir) rather than reaching back into the caller's `MonitorStore` —
    /// the same pattern the race tests in `store/mod.rs`/`store/lease.rs`
    /// use, and a closer match for the real scenario (another process, its
    /// own connection) than sharing one `&MonitorStore` across threads
    /// would be.
    struct FenceYanker(PathBuf, String);
    impl SourceAdapter for FenceYanker {
        fn source_type(&self) -> SourceType {
            SourceType::MurRun
        }
        fn validate_reference(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _: &str, _: Option<&str>) -> Observation {
            let intruder = MonitorStore::open(&self.0).unwrap();
            let fence = intruder.lease_of(&self.1).unwrap().unwrap().fence;
            assert!(
                intruder
                    .release(&self.1, fence, MonitorState::Sleeping)
                    .unwrap(),
                "releasing under the fence tick claimed must succeed"
            );
            let reclaimed = intruder
                .claim_due(t0(), "intruder", DEFAULT_LEASE, 8)
                .unwrap();
            assert_eq!(reclaimed.len(), 1, "the intruder must reclaim the monitor");
            Observation::terminal(Outcome::Succeeded, "done")
        }
    }

    #[test]
    fn a_stale_fence_cycle_is_not_counted_as_landed() {
        // No actions: a normal (non-yanked) terminal-success cycle would set
        // `new_state = Completed`, exercising the counter that the plan's
        // sample code (wrongly) set from the plan BEFORE `apply_cycle`
        // rather than from its result.
        let (d, s, id) = fresh("  on_success: []", true);
        let mut reg = AdapterRegistry::new();
        reg.register(Box::new(FenceYanker(d.path().to_path_buf(), id.clone())));
        let rep = tick(&s, &reg, t0(), "w", 8).unwrap();

        assert_eq!(rep.claimed, 1);
        assert_eq!(rep.stale_fence, 1);
        assert_eq!(rep.observed, 0);
        // The dropped cycle planned `Completed`, but since `apply_cycle`
        // wrote nothing, the report must not say so.
        assert_eq!(
            rep.completed, 0,
            "a write that never landed must not be counted"
        );
        assert_eq!((rep.action_pending, rep.exhausted, rep.unknown), (0, 0, 0));

        // Confirms nothing landed: the row is exactly where the intruder's
        // reclaim left it (`Checking`, under the intruder's lease), not
        // `Completed` and not back on `Sleeping`.
        let r = s.get(&id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Checking);
        assert_eq!(s.lease_of(&id).unwrap().unwrap().owner, "intruder");
    }
}
