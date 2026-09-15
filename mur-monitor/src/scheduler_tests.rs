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

fn spec_yaml_with_idem(idem: &str) -> MonitorSpec {
    MonitorSpec::from_yaml(&format!(
        r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: run-1 }}
actions:
  on_success: []
policy: {{ retain_monitoring_after_hard_deadline: true }}
idempotency_key: {idem}
created_by: {{ actor: user:test }}
"#
    ))
    .unwrap()
}

#[test]
fn a_later_claims_lease_is_beaten_after_earlier_observes_burn_time() {
    // A single-monitor version of this test asks the wrong question: the
    // FIRST monitor `tick` observes is claimed and beaten back to back
    // with ~0 real elapsed time in between, so its refreshed expiry is
    // legitimately indistinguishable from the claim_due stamp — there is
    // nothing to "beat past" yet. The actual claim this fix makes is about
    // the Nth monitor in a batch: A and B below are claimed by the SAME
    // `claim_due` call (identical stamped expiry), but B's pre-observe
    // heartbeat only runs after A's observe has already burned real
    // wall-clock time.
    let d = tempfile::tempdir().unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let home = d.path().to_path_buf();

    // Staggered `next_check_at` (1ms apart) makes `claim_due`'s
    // `ORDER BY next_check_at ASC` deterministic: A is always claimed
    // (and therefore observed) strictly before B, rather than relying
    // on how SQLite happens to break a tie on equal values.
    let id_a = s.create(&spec_yaml_with_idem("a"), t0(), None).unwrap().id;
    let id_b = s
        .create(&spec_yaml_with_idem("b"), t0() + CD::milliseconds(1), None)
        .unwrap()
        .id;
    let tick_time = t0() + CD::milliseconds(1);

    struct Probe {
        home: PathBuf,
        id_a: String,
        id_b: String,
        calls: Mutex<usize>,
        seen_a: std::sync::Arc<Mutex<Option<DateTime<Utc>>>>,
        seen_b: std::sync::Arc<Mutex<Option<DateTime<Utc>>>>,
    }
    impl SourceAdapter for Probe {
        fn source_type(&self) -> SourceType {
            SourceType::MurRun
        }
        fn validate_reference(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _: &str, _: Option<&str>) -> Observation {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            let probe = MonitorStore::open(&self.home).unwrap();
            if *calls == 1 {
                // This is A: snapshot its just-beaten expiry, then burn
                // real time so B's own beat (still ahead of it in the
                // loop) has something non-zero to advance past. Without
                // the sleep, the real elapsed time between `claim_due`
                // and each beat is sub-millisecond, and `expires_at` is
                // stored with millisecond precision (`ts()` uses
                // `SecondsFormat::Millis`) — a beaten and an unbeaten
                // lease would then round to the identical string and the
                // assertion below could pass whether or not `heartbeat`
                // was ever called. 20ms is comfortably above that
                // precision floor, making the two genuinely
                // distinguishable.
                *self.seen_a.lock().unwrap() =
                    probe.lease_of(&self.id_a).unwrap().map(|l| l.expires_at);
                std::thread::sleep(std::time::Duration::from_millis(20));
            } else {
                *self.seen_b.lock().unwrap() =
                    probe.lease_of(&self.id_b).unwrap().map(|l| l.expires_at);
            }
            Observation::pending("p", "running")
        }
    }

    let seen_a: std::sync::Arc<Mutex<Option<DateTime<Utc>>>> =
        std::sync::Arc::new(Mutex::new(None));
    let seen_b: std::sync::Arc<Mutex<Option<DateTime<Utc>>>> =
        std::sync::Arc::new(Mutex::new(None));
    let mut reg = AdapterRegistry::new();
    reg.register(Box::new(Probe {
        home,
        id_a: id_a.clone(),
        id_b: id_b.clone(),
        calls: Mutex::new(0),
        seen_a: seen_a.clone(),
        seen_b: seen_b.clone(),
    }));

    let rep = tick(&s, &reg, tick_time, "w", 8).unwrap();
    assert_eq!((rep.claimed, rep.observed, rep.stale_fence), (2, 2, 0));

    // What claim_due itself stamped for both A and B — same `now`,
    // same lease duration, in the same call.
    let claim_time_stamp = tick_time + cd(DEFAULT_LEASE);

    let a_expiry = seen_a.lock().unwrap().expect("A observed");
    let b_expiry = seen_b.lock().unwrap().expect("B observed");

    // A is claimed first: its pre-observe heartbeat fires with ~0 real
    // elapsed time since claim_due, so its refreshed expiry is
    // indistinguishable from the claim-time stamp. This is CORRECT and
    // expected — do not turn this into an inequality later; doing so
    // would make the assertion pass for the wrong reason.
    assert_eq!(a_expiry, claim_time_stamp);

    // B is claimed in the SAME claim_due call (identical original
    // stamp), but its heartbeat only runs after A's observe already
    // burned 20ms of real time, so B's beaten expiry must be strictly
    // later than what claim_due alone gave it.
    assert!(
        b_expiry > claim_time_stamp,
        "expected B's pre-observe heartbeat (after A's observe burned \
         real time) to move its expiry beyond the shared claim_due \
         stamp: {b_expiry} vs {claim_time_stamp}"
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
