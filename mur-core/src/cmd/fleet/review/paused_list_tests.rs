//! Spec §3.4 / AC-P3b-4b: bare `/review` lists paused sessions with the
//! `prune.rs` scan (`observe`), and shows a session another process holds as
//! `running` without offering it.

use mur_channel::ChannelService;

use super::{PausedRow, list_paused};
use crate::cmd::fleet::review::run_lock;
use crate::cmd::fleet::review::state::SessionState;
use crate::cmd::fleet::review::state::state_tests::{mid_round, paused, session, stopped};

fn home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    tmp
}

fn names(rows: &[PausedRow]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

#[test]
fn paused_and_crashed_are_listed_in_name_order() {
    let tmp = home();
    let h = tmp.path();
    session(h, "review-bbbb0001", &[paused()], true);
    session(h, "review-aaaa0001", &[mid_round()], true);
    let rows = list_paused(h).unwrap();
    assert_eq!(names(&rows), ["review-aaaa0001", "review-bbbb0001"]);
    assert_eq!(rows[0].state, SessionState::Crashed);
    assert_eq!(rows[1].state, SessionState::Paused);
    assert!(rows.iter().all(|r| r.last.is_some()));
}

#[test]
fn stopped_and_orphaned_sessions_are_not_listed() {
    let tmp = home();
    let h = tmp.path();
    session(h, "review-stop0001", &[stopped()], false);
    session(h, "review-orph0001", &[paused()], false);
    session(h, "review-keep0001", &[paused()], true);
    assert_eq!(names(&list_paused(h).unwrap()), ["review-keep0001"]);
}

/// AC-P3b-4b: a lock held elsewhere reads as `running`, whatever the events say,
/// and listing it does not take the lock away from the holder.
#[test]
fn a_session_locked_by_another_process_is_listed_as_running() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-runn0001", &[paused()], true);
    let svc = ChannelService::open(h).unwrap();
    let _held = run_lock::try_acquire(&svc, &ch).unwrap();
    let rows = list_paused(h).unwrap();
    assert_eq!(names(&rows), ["review-runn0001"]);
    assert!(matches!(rows[0].state, SessionState::Running(Some(_))));
    assert!(
        run_lock::try_acquire(&svc, &ch).is_err(),
        "holder keeps the lock"
    );
}

#[test]
fn listing_releases_the_locks_it_takes() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-free0001", &[paused()], true);
    list_paused(h).unwrap();
    let svc = ChannelService::open(h).unwrap();
    assert!(run_lock::try_acquire(&svc, &ch).is_ok());
}

#[test]
fn no_sessions_is_an_empty_list() {
    let tmp = home();
    assert!(list_paused(tmp.path()).unwrap().is_empty());
}
