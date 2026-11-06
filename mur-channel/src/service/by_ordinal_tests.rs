//! `ChannelService::conversation_by_ordinal` (at-mention A2).
use super::*;
use tempfile::TempDir;

#[test]
fn conversation_by_ordinal_checks_purpose_and_membership() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    let n = svc.index().ordinal_of(&ch.id).unwrap().unwrap();
    assert_eq!(
        svc.conversation_by_ordinal(n, "mur").unwrap().map(|c| c.id),
        Some(ch.id.clone())
    );
    // Not a participant → None (never a side-effect add).
    assert!(svc.conversation_by_ordinal(n, "other").unwrap().is_none());
    assert_eq!(
        svc.store()
            .load_manifest(&ch.id)
            .unwrap()
            .participants
            .len(),
        ch.participants.len(),
        "no participant added"
    );
    // A fleet channel is never a conversation, even for its members.
    let fleet = svc.create_for_fleet("x", "mur", &[]).unwrap();
    let fleet_n = svc.index().ordinal_of(&fleet.id).unwrap().unwrap();
    assert!(
        svc.conversation_by_ordinal(fleet_n, "mur")
            .unwrap()
            .is_none()
    );
    // 0 is the "not numbered" sentinel; an unassigned number is nothing.
    assert!(svc.conversation_by_ordinal(0, "mur").unwrap().is_none());
    assert!(svc.conversation_by_ordinal(99, "mur").unwrap().is_none());
}

#[test]
fn conversation_by_ordinal_resolves_after_the_index_row_is_removed() {
    // The repair path deletes the `channels` row while the manifest and the
    // ordinal stay; `#N` must keep pointing at the same conversation.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    let n = svc.index().ordinal_of(&ch.id).unwrap().unwrap();
    svc.index().remove(&ch.id).unwrap();
    assert_eq!(
        svc.conversation_by_ordinal(n, "mur").unwrap().map(|c| c.id),
        Some(ch.id)
    );
}
