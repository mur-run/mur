//! `ChannelIndex::by_ordinal` — the reverse of `ordinal_of` (at-mention A2).
use super::*;
use chrono::Utc;
use mur_common::channel::{ChannelActor, ChannelState, Goal};
use tempfile::TempDir;

fn ch(id: &str) -> Channel {
    let now = Utc::now();
    Channel {
        v: 1,
        id: id.into(),
        title: id.into(),
        goal: Goal::default(),
        state: ChannelState::Working,
        purpose: None,
        owner: ChannelActor::Human { name: "me".into() },
        participants: vec![],
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn by_ordinal_is_the_reverse_of_ordinal_of() {
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("c0")).unwrap();
    idx.upsert(&ch("c1")).unwrap();
    let n = idx.ordinal_of("c1").unwrap().unwrap();
    assert_eq!(n, 2);
    assert_eq!(idx.by_ordinal(n).unwrap().as_deref(), Some("c1"));
    assert_eq!(idx.by_ordinal(1).unwrap().as_deref(), Some("c0"));
    assert_eq!(idx.by_ordinal(3).unwrap(), None);
}

#[test]
fn by_ordinal_zero_is_none() {
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("c1")).unwrap();
    assert_eq!(idx.by_ordinal(0).unwrap(), None);
}

#[test]
fn by_ordinal_survives_remove() {
    // `mur channel repair` deletes the `channels` row but never
    // `channel_ordinals`; #N must still resolve right after a repair.
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("c1")).unwrap();
    let n = idx.ordinal_of("c1").unwrap().unwrap();
    idx.remove("c1").unwrap();
    assert_eq!(idx.by_ordinal(n).unwrap().as_deref(), Some("c1"));
}
