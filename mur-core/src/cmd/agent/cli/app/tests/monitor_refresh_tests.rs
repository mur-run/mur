//! `refresh_monitor_counts` must never create the monitor store just by
//! asking — Fix round 3 finding 1: `MonitorStore::open` unconditionally
//! creates `monitors.db` (+ WAL/SHM), and this refresh runs on a timer for
//! the life of every murmur session, so calling `open` here would
//! materialise a database for every user who has never touched `mur
//! monitor`. Asserts on the filesystem, not just the count — a count of 0
//! would pass either way and prove nothing about whether the fix landed.

use super::super::*;

#[test]
fn refresh_against_a_home_with_no_monitor_store_creates_nothing() {
    let mut app = App::test_fixture();
    let dir = mur_monitor::store::db_dir(&app.home);
    assert!(!dir.exists(), "sanity: fixture home has no monitor dir yet");

    app.refresh_monitor_counts(std::time::Instant::now());

    assert!(
        !dir.exists(),
        "a refresh against a store-less home must not create the monitor directory"
    );
    assert_eq!(app.monitor_total, 0);
    assert_eq!(app.monitor_conditions, 0);
}
