//! §7.0: every state is derived from lock + definition + last event + marker.

use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};

use super::{SessionState, observe};
use crate::cmd::fleet::review::constants::REVIEW_CORRUPTED_MARKER_FILE;
use crate::cmd::fleet::review::run_lock;
use crate::cmd::fleet::review::schema::{ReviewPayload, Role, to_note_payload};
use crate::cmd::fleet::review::verdict::zero_cumulative;
use crate::cmd::fleet::store;

pub(crate) fn session(
    home: &Path,
    name: &str,
    payloads: &[ReviewPayload],
    defined: bool,
) -> String {
    let fleet = crate::cmd::fleet::review::session::create_session_fleet(
        home, name, "main", "reviewer", "task",
    )
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
    if !defined {
        std::fs::remove_dir_all(store::fleet_dir(home, name)).unwrap();
    }
    fleet.channel_id
}

pub(crate) fn stopped() -> ReviewPayload {
    ReviewPayload::SessionStopped {
        reason: "approve".into(),
        unresolved: vec![],
        cumulative: zero_cumulative(),
    }
}

pub(crate) fn paused() -> ReviewPayload {
    ReviewPayload::Paused {
        reason: "detached".into(),
        cumulative: zero_cumulative(),
    }
}

pub(crate) fn mid_round() -> ReviewPayload {
    ReviewPayload::TurnSent {
        round: 1,
        to: Role::Main,
        restart_note: None,
    }
}

fn state_of(home: &Path, ch: &str, name: &str) -> SessionState {
    let svc = ChannelService::open(home).unwrap();
    observe(&svc, home, ch, name).unwrap().state
}

#[test]
fn each_state_is_derived() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    crate::channel_writer::plant_writer_identity(h);

    let ch = session(h, "review-paus0001", &[paused()], true);
    assert_eq!(state_of(h, &ch, "review-paus0001"), SessionState::Paused);

    let ch = session(h, "review-crsh0001", &[mid_round()], true);
    assert_eq!(state_of(h, &ch, "review-crsh0001"), SessionState::Crashed);

    let resumed = ReviewPayload::Resumed {
        cumulative: zero_cumulative(),
    };
    let ch = session(h, "review-crsh0002", &[paused(), resumed], true);
    assert_eq!(state_of(h, &ch, "review-crsh0002"), SessionState::Crashed);

    let ch = session(h, "review-stop0001", &[stopped()], false);
    assert_eq!(state_of(h, &ch, "review-stop0001"), SessionState::Stopped);

    let ch = session(h, "review-orph0001", &[paused()], false);
    assert_eq!(state_of(h, &ch, "review-orph0001"), SessionState::Orphaned);

    let ch = session(h, "review-corr0001", &[paused()], true);
    let marker = ChannelService::open(h)
        .unwrap()
        .store()
        .events_path(&ch)
        .with_file_name(REVIEW_CORRUPTED_MARKER_FILE);
    std::fs::write(&marker, r#"{"detected_at":"2026-01-01T00:00:00Z"}"#).unwrap();
    assert_eq!(state_of(h, &ch, "review-corr0001"), SessionState::Corrupted);
    std::fs::write(&marker, "{nope").unwrap();
    assert!(matches!(
        state_of(h, &ch, "review-corr0001"),
        SessionState::CorruptedUnreadable(_)
    ));
}

/// A held lock is `running` whatever the events say, and observing it does
/// not take the lock away from the holder.
#[test]
fn a_held_lock_is_running_whatever_the_events_say() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    crate::channel_writer::plant_writer_identity(h);
    let ch = session(h, "review-runn0001", &[paused()], true);
    let svc = ChannelService::open(h).unwrap();
    let _held = run_lock::try_acquire(&svc, &ch).unwrap();
    assert!(matches!(
        state_of(h, &ch, "review-runn0001"),
        SessionState::Running(Some(_))
    ));
}
