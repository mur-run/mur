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
    let fleet = super::super::session::create_session_fleet(home, name, "main", "reviewer", "task")
        .unwrap();
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
    prune_with(home, false, dry_run, days_later)
}

fn prune_with(home: &Path, include_paused: bool, dry_run: bool, days_later: i64) -> String {
    let mut out = Vec::new();
    let now = Utc::now() + ChronoDuration::days(days_later);
    prune_reviews(home, "1d", include_paused, dry_run, &mut out, now).unwrap();
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

/// §7.0: stopped then resumed with no definition is `orphaned` — the row
/// that keeps the classification exhaustive — and default prune takes it.
#[test]
fn a_session_resumed_after_stopping_without_a_definition_is_orphaned() {
    let tmp = home();
    let h = tmp.path();
    let resumed = ReviewPayload::Resumed {
        cumulative: zero_cumulative(),
    };
    let ch = session(h, "review-resu0001", &[stopped(), resumed], false);
    let report = prune(h, false, 2);
    assert!(
        report.contains("pruned review-resu0001 (orphaned"),
        "{report}"
    );
    assert!(!exists(h, &ch));
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
    assert!(prune_reviews(tmp.path(), "soon", false, false, &mut out, Utc::now()).is_err());
}

/// AC15d: one session per §7.0 state, all past the cutoff.
#[test]
fn include_paused_takes_paused_and_crashed_and_never_running() {
    use crate::cmd::fleet::review::run_lock;
    use crate::cmd::fleet::review::schema::Role;
    let tmp = home();
    let h = tmp.path();
    let mid = ReviewPayload::TurnSent {
        round: 1,
        to: Role::Main,
        restart_note: None,
        human_wait_ms: 0,
    };
    let stop = session(h, "review-stop0009", &[stopped()], false);
    let orph = session(h, "review-orph0009", &[paused()], false);
    let paus = session(h, "review-paus0009", &[paused()], true);
    let crsh = session(h, "review-crsh0009", &[mid], true);
    let runn = session(h, "review-runn0009", &[paused()], true);
    let svc = ChannelService::open(h).unwrap();
    let _held = run_lock::try_acquire(&svc, &runn).unwrap();

    let dry = prune_with(h, true, true, 2);
    assert!(dry.contains("would prune review-paus0009 (paused"), "{dry}");
    assert!(
        dry.contains("would prune review-crsh0009 (crashed"),
        "{dry}"
    );
    for ch in [&stop, &orph, &paus, &crsh, &runn] {
        assert!(exists(h, ch), "dry run erased {ch}");
    }

    let default = prune_with(h, false, false, 2);
    assert!(
        default.contains("pruned review-stop0009 (stopped"),
        "{default}"
    );
    assert!(
        default.contains("pruned review-orph0009 (orphaned"),
        "{default}"
    );
    assert!(!exists(h, &stop) && !exists(h, &orph));
    for ch in [&paus, &crsh, &runn] {
        assert!(exists(h, ch), "default prune took {ch}: {default}");
    }

    let all = prune_with(h, true, false, 2);
    assert!(all.contains("pruned review-paus0009 (paused"), "{all}");
    assert!(all.contains("pruned review-crsh0009 (crashed"), "{all}");
    assert!(!exists(h, &paus) && !exists(h, &crsh));
    assert!(!store::fleet_path(h, "review-paus0009").exists());
    assert!(exists(h, &runn), "a running session is never pruned");
    assert!(!all.contains("review-runn0009"), "{all}");
}

/// AC15d: the flag is still gated by age.
#[test]
fn include_paused_keeps_a_paused_session_newer_than_the_cutoff() {
    let tmp = home();
    let h = tmp.path();
    let paus = session(h, "review-paus0010", &[paused()], true);
    let report = prune_with(h, true, false, 0);
    assert!(report.contains("nothing to prune"), "{report}");
    assert!(exists(h, &paus));
}

/// §7.1 crash-safe order: a paused session gains `session_stopped`
/// (reason `pruned`) before anything is removed. Checked by stopping it from
/// outside exactly as prune does, then reading the channel.
#[test]
fn prune_records_session_stopped_pruned_before_removal() {
    use crate::cmd::fleet::review::constants::REVIEW_STOP_REASON_PRUNED;
    use crate::cmd::fleet::review::schema::{NoteClassification, classify_note_payload};
    use crate::cmd::fleet::review::state::{observe, stop_from_outside};
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, "review-paus0011", &[paused()], true);
    let svc = ChannelService::open(h).unwrap();
    let o = observe(&svc, h, &ch, "review-paus0011").unwrap();
    stop_from_outside(
        &svc,
        h,
        "review-paus0011",
        &ch,
        &o.state,
        REVIEW_STOP_REASON_PRUNED,
        o.lock.as_ref().unwrap(),
    )
    .unwrap();
    let last = svc
        .load_events(&ch)
        .unwrap()
        .into_iter()
        .rev()
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .next()
        .unwrap();
    assert!(
        matches!(&last, ReviewPayload::SessionStopped { reason, .. } if reason == REVIEW_STOP_REASON_PRUNED),
        "{last:?}"
    );
    assert!(!store::fleet_path(h, "review-paus0011").exists());
    drop(o);
    // Interrupted here (channel not yet erased) → an ordinary stopped session.
    assert!(prune(h, false, 2).contains("pruned review-paus0011 (stopped"));
}
