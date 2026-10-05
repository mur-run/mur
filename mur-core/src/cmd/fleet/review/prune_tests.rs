//! AC15b: prune removes only stopped or corrupted review channels older than
//! the cutoff; running/paused sessions are never removed; `--dry-run`
//! erases nothing. (`now` is pushed forward instead of back-dating events,
//! since the store stamps `ts` at append time.)

use std::path::Path;

use chrono::{Duration as ChronoDuration, Utc};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};

use super::prune_reviews;
use crate::cmd::fleet::review::constants::REVIEW_CORRUPTED_MARKER_FILE;
use crate::cmd::fleet::review::schema::{ReviewPayload, to_note_payload};
use crate::cmd::fleet::review::verdict::zero_cumulative;
use crate::cmd::fleet::store;

/// A review session's channel, with `payloads` appended. `keep_fleet`
/// leaves the fleet definition in place, as a running/paused session has.
fn session(home: &Path, name: &str, payloads: &[ReviewPayload], keep_fleet: bool) -> String {
    let fleet =
        super::super::session::create_session_fleet(home, name, "main", "reviewer").unwrap();
    let svc = ChannelService::open(home).unwrap();
    for p in payloads {
        crate::channel_writer::append_as_writer(
            &svc,
            home,
            &fleet.channel_id,
            crate::channel_writer::ROUTER_AGENT,
            ChannelActor::System,
            EventKind::Note,
            to_note_payload(p),
            None,
        )
        .unwrap();
    }
    if !keep_fleet {
        std::fs::remove_dir_all(store::fleet_dir(home, name)).unwrap();
    }
    fleet.channel_id
}

fn stopped() -> ReviewPayload {
    ReviewPayload::SessionStopped {
        reason: "approve".into(),
        unresolved: vec![],
        cumulative: zero_cumulative(),
    }
}

fn paused() -> ReviewPayload {
    ReviewPayload::Paused {
        reason: "detached".into(),
        cumulative: zero_cumulative(),
    }
}

fn exists(home: &Path, channel_id: &str) -> bool {
    ChannelService::open(home)
        .unwrap()
        .store()
        .events_path(channel_id)
        .parent()
        .unwrap()
        .exists()
}

fn prune(home: &Path, dry_run: bool, days_later: i64) -> String {
    let mut out = Vec::new();
    let now = Utc::now() + ChronoDuration::days(days_later);
    prune_reviews(home, "1d", dry_run, &mut out, now).unwrap();
    String::from_utf8(out).unwrap()
}

fn home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    tmp
}

#[test]
fn removes_only_stopped_sessions_past_the_cutoff() {
    let tmp = home();
    let h = tmp.path();
    let done = session(h, "review-done0001", &[stopped()], false);
    let paused_ch = session(h, "review-paus0001", &[paused()], true);
    let running = session(h, "review-runn0001", &[], true);
    // A stopped session whose fleet definition is still present is treated
    // as live: the definition is the authority on "not over".
    let stale_def = session(h, "review-defn0001", &[stopped()], true);
    let user = ChannelService::open(h)
        .unwrap()
        .create_for_fleet("dev", "router", &[])
        .unwrap()
        .id;

    let report = prune(h, false, 2);
    assert!(
        report.contains("pruned review-done0001 (stopped"),
        "{report}"
    );
    assert!(!exists(h, &done));
    for kept in [&paused_ch, &running, &stale_def, &user] {
        assert!(exists(h, kept), "{kept} must survive: {report}");
    }
}

#[test]
fn nothing_younger_than_the_cutoff_is_removed() {
    let tmp = home();
    let h = tmp.path();
    let done = session(h, "review-done0002", &[stopped()], false);
    let report = prune(h, false, 0);
    assert!(report.contains("nothing to prune"), "{report}");
    assert!(exists(h, &done));
}

#[test]
fn dry_run_lists_and_erases_nothing() {
    let tmp = home();
    let h = tmp.path();
    let done = session(h, "review-done0003", &[stopped()], false);
    let report = prune(h, true, 2);
    assert!(report.contains("would prune review-done0003"), "{report}");
    assert!(report.contains("nothing erased"), "{report}");
    assert!(exists(h, &done));
}

#[test]
fn a_session_resumed_after_stopping_is_not_a_candidate() {
    let tmp = home();
    let h = tmp.path();
    let resumed = ReviewPayload::Resumed {
        cumulative: zero_cumulative(),
    };
    let ch = session(h, "review-resu0001", &[stopped(), resumed], false);
    prune(h, false, 2);
    assert!(exists(h, &ch));
}

/// §8.2 prune equivalence: a readable marker counts as stopped; its
/// `detected_at` extends last activity.
#[test]
fn readable_corrupted_marker_is_a_candidate_and_dates_last_activity() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-corr0001", &[paused()], false);
    let marker = ChannelService::open(h)
        .unwrap()
        .store()
        .events_path(&ch)
        .with_file_name(REVIEW_CORRUPTED_MARKER_FILE);
    let detected = Utc::now() + ChronoDuration::days(5);
    std::fs::write(
        &marker,
        serde_json::json!({
            "session": "review-corr0001", "channel_id": ch, "reason": "corrupted",
            "detected_at": detected, "damaged_lines": [3],
        })
        .to_string(),
    )
    .unwrap();

    // Events are 2 days old, but the marker is newer than the cutoff.
    assert!(prune(h, false, 2).contains("nothing to prune"));
    assert!(exists(h, &ch));
    let report = prune(h, false, 7);
    assert!(
        report.contains("pruned review-corr0001 (corrupted"),
        "{report}"
    );
    assert!(
        !exists(h, &ch),
        "the marker goes with the channel directory"
    );
}

#[test]
fn unreadable_marker_is_reported_and_never_pruned() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-corr0002", &[paused()], false);
    let marker = ChannelService::open(h)
        .unwrap()
        .store()
        .events_path(&ch)
        .with_file_name(REVIEW_CORRUPTED_MARKER_FILE);
    std::fs::write(&marker, "{not json").unwrap();
    let report = prune(h, false, 30);
    assert!(report.contains("skipped review-corr0002"), "{report}");
    assert!(report.contains(REVIEW_CORRUPTED_MARKER_FILE), "{report}");
    assert!(exists(h, &ch));
}

#[test]
fn bad_duration_is_an_error() {
    let tmp = home();
    let mut out = Vec::new();
    assert!(prune_reviews(tmp.path(), "soon", false, &mut out, Utc::now()).is_err());
}
