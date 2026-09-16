//! Tests for `store/mod.rs`. Split out as pure code movement to stay
//! under CLAUDE.md's 800-line-per-file rule; nothing changed in the move.

#[test]
fn record_remediation_attempt_counts_up_and_persists() {
    let d = tempfile::tempdir().unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.create(&spec("k"), t0(), None).unwrap().id;
    assert_eq!(s.get(&id).unwrap().unwrap().remediation_attempts, 0);
    assert_eq!(s.record_remediation_attempt(&id).unwrap(), 1);
    assert_eq!(s.record_remediation_attempt(&id).unwrap(), 2);
    assert_eq!(s.get(&id).unwrap().unwrap().remediation_attempts, 2);
}

use super::*;
use chrono::TimeZone;

pub(crate) fn spec(idem: &str) -> MonitorSpec {
    MonitorSpec::from_yaml(&format!(
        r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: run-1 }}
idempotency_key: {idem}
created_by: {{ actor: user:test }}
"#
    ))
    .unwrap()
}

pub(crate) fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
}

#[test]
fn open_twice_and_migrate_is_idempotent() {
    let d = tempfile::tempdir().unwrap();
    MonitorStore::open(d.path()).unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let v: i64 = s
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_USER_VERSION);
    assert!(d.path().join("monitor").join(DB_FILE).exists());
}

#[test]
fn create_is_idempotent_on_active_key() {
    let d = tempfile::tempdir().unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let a = s.create(&spec("k1"), t0(), None).unwrap();
    let b = s.create(&spec("k1"), t0(), None).unwrap();
    assert!(!a.existing);
    assert!(b.existing);
    assert_eq!(a.id, b.id);
    assert_eq!(a.next_check_at, t0(), "first check is immediate");
    let row = s.get(&a.id).unwrap().unwrap();
    assert_eq!(row.state, MonitorState::Active);
    assert_eq!(row.outcome, Outcome::Pending);
    assert_eq!(row.work_started_at, t0());
    assert_eq!(row.fence, 0);
    // a completed monitor no longer reserves its key
    assert!(s.set_state(&a.id, MonitorState::Completed, t0()).unwrap());
    let c = s.create(&spec("k1"), t0(), None).unwrap();
    assert!(!c.existing);
    assert_ne!(c.id, a.id);
}

#[test]
fn work_started_at_is_the_callers_when_given() {
    let d = tempfile::tempdir().unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let started = t0() - chrono::Duration::minutes(30);
    let c = s.create(&spec("k2"), t0(), Some(started)).unwrap();
    assert_eq!(s.get(&c.id).unwrap().unwrap().work_started_at, started);
}

#[test]
fn list_hides_completed_by_default_and_filters_by_state() {
    let d = tempfile::tempdir().unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let a = s.create(&spec("a"), t0(), None).unwrap();
    let b = s.create(&spec("b"), t0(), None).unwrap();
    s.set_state(&a.id, MonitorState::Completed, t0()).unwrap();
    s.set_state(&b.id, MonitorState::Exhausted, t0()).unwrap();
    let ids =
        |f: &ListFilter| -> Vec<String> { s.list(f).unwrap().into_iter().map(|r| r.id).collect() };
    assert_eq!(
        ids(&ListFilter::default()),
        vec![b.id.clone()],
        "exhausted needs a human, completed does not"
    );
    assert_eq!(
        ids(&ListFilter {
            include_completed: true,
            ..Default::default()
        })
        .len(),
        2
    );
    assert_eq!(
        ids(&ListFilter {
            state: Some(MonitorState::Completed),
            include_completed: true
        }),
        vec![a.id]
    );
}

#[test]
fn reactivate_only_from_exhausted() {
    let d = tempfile::tempdir().unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let a = s.create(&spec("a"), t0(), None).unwrap();
    assert!(
        !s.reactivate(&a.id, t0(), false).unwrap(),
        "active is not retryable"
    );
    s.set_state(&a.id, MonitorState::Exhausted, t0()).unwrap();
    s.conn()
        .execute(
            "UPDATE monitors SET remediation_attempts = 3, unknown_streak = 9 WHERE id = ?1",
            [&a.id],
        )
        .unwrap();
    assert!(s.reactivate(&a.id, t0(), false).unwrap());
    let r = s.get(&a.id).unwrap().unwrap();
    assert_eq!(r.state, MonitorState::Active);
    assert_eq!(r.unknown_streak, 0);
    assert_eq!(r.remediation_attempts, 3, "budget kept unless asked");
    s.set_state(&a.id, MonitorState::Exhausted, t0()).unwrap();
    assert!(s.reactivate(&a.id, t0(), true).unwrap());
    assert_eq!(s.get(&a.id).unwrap().unwrap().remediation_attempts, 0);
}

/// Rule 6 under the deployment this file's module doc describes: the
/// CLI, the daemon and (plan-2) an agent runtime each open independent
/// connections to the same `monitors.db`. A single in-process
/// connection (every other test here) can never observe a cross-process
/// race, so this test opens one `MonitorStore` per thread against the
/// same on-disk file and lines them up with a `Barrier` to force
/// concurrent `create()` calls on the same idempotency key.
#[test]
fn concurrent_create_on_one_key_never_errors_and_never_duplicates() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().to_path_buf();
    // Migrate once up front so every thread's own `open()` below only
    // has to race on `create()`, not on schema creation too.
    MonitorStore::open(&dir).unwrap();

    const N: usize = 8;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let dir = dir.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let s = MonitorStore::open(&dir).unwrap();
                barrier.wait();
                s.create(&spec("race"), t0(), None)
            })
        })
        .collect();
    let results: Vec<Result<Created>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let mut ids = std::collections::HashSet::new();
    let (mut created, mut existing) = (0, 0);
    for r in &results {
        let c = r
            .as_ref()
            .expect("create() must never error under a racing insert");
        ids.insert(c.id.clone());
        if c.existing {
            existing += 1;
        } else {
            created += 1;
        }
    }
    assert_eq!(created, 1, "exactly one caller creates the monitor");
    assert_eq!(existing, N - 1, "everyone else finds it existing");
    assert_eq!(ids.len(), 1, "every caller must agree on the same id");

    let s = MonitorStore::open(&dir).unwrap();
    assert_eq!(
        s.list(&ListFilter {
            include_completed: true,
            ..Default::default()
        })
        .unwrap()
        .len(),
        1,
        "no duplicate row was inserted"
    );
}
