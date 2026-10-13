//! #1804: `prune-reviews --id` erases exactly the named sessions, with the
//! id itself standing in for `--include-paused` — but never a running one.

use std::path::Path;

use mur_channel::ChannelService;

use super::prune_reviews_by_id;
use super::prune_tests::{exists, home, paused, session, stopped};
use crate::cmd::fleet::review::constants::REVIEW_CORRUPTED_MARKER_FILE;
use crate::cmd::fleet::review::schema::{ReviewPayload, Role};
use crate::cmd::fleet::store;

fn by_id(home: &Path, ids: &[&str], dry_run: bool) -> (anyhow::Result<()>, String) {
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let res = prune_reviews_by_id(home, &ids, dry_run, &mut out);
    (res, String::from_utf8(out).unwrap())
}

fn crashed() -> ReviewPayload {
    ReviewPayload::TurnSent {
        round: 1,
        to: Role::Main,
        restart_note: None,
        human_wait_ms: 0,
    }
}

#[test]
fn erases_a_crashed_session_channel_and_definition_regardless_of_age() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-crsh1804", &[crashed()], true);
    let other = session(h, "review-crsh1805", &[crashed()], true);

    let (res, out) = by_id(h, &["review-crsh1804"], false);
    res.unwrap();
    assert!(out.contains("pruned review-crsh1804 (crashed"), "{out}");
    assert!(!exists(h, &ch));
    assert!(!store::fleet_dir(h, "review-crsh1804").exists());
    assert!(exists(h, &other), "only the named session goes");
}

#[test]
fn bare_id_without_the_review_prefix_is_accepted() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-done1804", &[stopped()], false);
    let (res, out) = by_id(h, &["done1804"], false);
    res.unwrap();
    assert!(out.contains("pruned review-done1804 (stopped"), "{out}");
    assert!(!exists(h, &ch));
}

#[test]
fn paused_session_records_session_stopped_pruned_and_is_erased() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-paus1804", &[paused()], true);
    // Dry run leaves everything in place, so the channel is still readable.
    let (res, out) = by_id(h, &["review-paus1804"], true);
    res.unwrap();
    assert!(out.contains("would prune review-paus1804 (paused"), "{out}");
    assert!(exists(h, &ch));
    assert!(store::fleet_dir(h, "review-paus1804").exists());

    let (res, out) = by_id(h, &["review-paus1804"], false);
    res.unwrap();
    assert!(out.contains("pruned review-paus1804 (paused"), "{out}");
    assert!(!exists(h, &ch));
    assert!(!store::fleet_dir(h, "review-paus1804").exists());
}

#[test]
fn running_session_is_refused_and_untouched() {
    use crate::cmd::fleet::review::run_lock;
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-runn1804", &[paused()], true);
    let svc = ChannelService::open(h).unwrap();
    let _held = run_lock::try_acquire(&svc, &ch).unwrap();

    let (res, out) = by_id(h, &["review-runn1804"], false);
    let err = format!("{:#}", res.unwrap_err());
    assert!(
        err.contains("review-runn1804") && err.contains("running"),
        "{err}"
    );
    assert!(!out.contains("pruned review-runn1804"), "{out}");
    assert!(exists(h, &ch));
    assert!(store::fleet_dir(h, "review-runn1804").exists());
}

#[test]
fn unknown_id_is_an_error() {
    let tmp = home();
    let (res, _) = by_id(tmp.path(), &["review-none1804"], false);
    let err = format!("{:#}", res.unwrap_err());
    assert!(err.contains("no review session `review-none1804`"), "{err}");
}

#[test]
fn unreadable_marker_is_refused_with_its_path() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-corr1804", &[paused()], false);
    let marker = ChannelService::open(h)
        .unwrap()
        .store()
        .events_path(&ch)
        .with_file_name(REVIEW_CORRUPTED_MARKER_FILE);
    std::fs::write(&marker, "{not json").unwrap();

    let (res, out) = by_id(h, &["review-corr1804"], false);
    assert!(res.is_err());
    assert!(out.contains("skipped review-corr1804"), "{out}");
    assert!(out.contains(REVIEW_CORRUPTED_MARKER_FILE), "{out}");
    assert!(exists(h, &ch));
}

/// Repeated ids are processed independently: one refusal does not stop the
/// others, but the run still exits non-zero.
#[test]
fn several_ids_each_get_a_result_and_any_failure_fails_the_run() {
    let tmp = home();
    let h = tmp.path();
    let a = session(h, "review-done1806", &[stopped()], false);
    let b = session(h, "review-crsh1806", &[crashed()], true);

    let (res, out) = by_id(
        h,
        &["review-done1806", "review-gone1806", "crsh1806"],
        false,
    );
    assert!(res.is_err(), "{out}");
    assert!(out.contains("pruned review-done1806"), "{out}");
    assert!(out.contains("pruned review-crsh1806"), "{out}");
    assert!(out.contains("no review session `review-gone1806`"), "{out}");
    assert!(!exists(h, &a) && !exists(h, &b));
}
