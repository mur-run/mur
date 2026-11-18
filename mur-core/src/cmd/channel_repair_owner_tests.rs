//! C5 through `mur channel repair`: an owned channel keeps its owner across
//! the restart; Gate B's decision channel with an owner record is refused.

use super::*;
use crate::channel_owner::HumanOwner;
use crate::cmd::channel_integrity::check;
use mur_common::channel::EventKind;
use tempfile::TempDir;

/// A home with the router key and an enrolled human, and one owned
/// conversation (B3) holding a human message after the record.
fn owned() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let owner = HumanOwner::load(tmp.path()).unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("qa", owner.owner()).unwrap();
    svc.append_message(&ch.id, ChannelActor::System, EventKind::Note, "hello", None)
        .unwrap();
    (tmp, ch.id)
}

/// Mark the channel dirty: damage the repair must clear.
fn damage(home: &Path, ch: &str) {
    mur_channel::anchor::mark_dirty(home, ch).unwrap();
}

fn kinds(home: &Path, ch: &str) -> Vec<EventKind> {
    mur_channel::ChannelStore::new(home)
        .load_events(ch)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn repair_preserves_ownership() {
    let (tmp, ch) = owned();
    assert_eq!(
        check(tmp.path(), &ch).unwrap().owner.live_record_seq,
        Some(0)
    );
    damage(tmp.path(), &ch);
    assert!(matches!(
        repair(tmp.path(), &ch, Confirm::Yes).unwrap(),
        Outcome::Repaired { seq: 0, .. }
    ));
    assert_eq!(
        kinds(tmp.path(), &ch),
        [EventKind::Note, EventKind::OwnerKeyEnrolled]
    );
    let after = check(tmp.path(), &ch).unwrap();
    assert!(after.is_intact(), "{after:?}");
    assert_eq!(after.owner.live_record_seq, Some(1));
}

/// Without the human key an owned channel is refused before anything moves.
#[test]
fn owned_repair_without_the_human_key_touches_nothing() {
    let (tmp, ch) = owned();
    damage(tmp.path(), &ch);
    std::fs::remove_file(mur_common::identity::human::human_private_key_path(
        tmp.path(),
    ))
    .unwrap();
    let log = mur_channel::ChannelStore::new(tmp.path())
        .events_path(&ch)
        .unwrap();
    let before = std::fs::read(&log).unwrap();
    let err = repair(tmp.path(), &ch, Confirm::Yes).unwrap_err();
    assert!(format!("{err:#}").contains("human owner"), "{err:#}");
    assert_eq!(std::fs::read(&log).unwrap(), before);
    assert!(mur_channel::anchor::is_dirty(tmp.path(), &ch), "untouched");
}

/// Gate B's fresh chain is agent-sealed and cannot carry an owner, so an
/// owned decision channel is refused and pointed at `mur channel verify`.
#[test]
fn gate_b_refuses_on_owned_channel() {
    let (tmp, ch) = owned();
    damage(tmp.path(), &ch);
    let agent = tmp.path().join("agents").join("qa");
    std::fs::create_dir_all(&agent).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&agent)
        .unwrap();
    std::fs::write(agent.join(HITL_CHANNEL_FILE), &ch).unwrap();
    let err = repair(tmp.path(), &ch, Confirm::Yes).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("human owner record"), "{msg}");
    assert!(msg.contains("mur channel verify"), "{msg}");
    assert!(mur_channel::anchor::is_dirty(tmp.path(), &ch), "untouched");
}

/// A legacy channel repairs as before: one record, no owner.
#[test]
fn repair_on_legacy_channel_unchanged() {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("legacy", mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id;
    damage(tmp.path(), &ch);
    repair(tmp.path(), &ch, Confirm::Yes).unwrap();
    assert_eq!(kinds(tmp.path(), &ch), [EventKind::Note]);
    let after = check(tmp.path(), &ch).unwrap();
    assert!(after.is_intact() && after.owner.live_record_seq.is_none());
}
