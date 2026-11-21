//! C7: the owner record is a `Human` control line, never a turn or a preview.

use super::*;
use crate::hitl::authority_fixtures::{enroll_agent_sealed, human_signer, human_write};
use tempfile::TempDir;

/// A legacy agent channel whose owner record lands before the first typed
/// line, the order C1 leaves on a channel the human writes to first.
fn enrolled_session(home: &Path) -> String {
    crate::channel_writer::plant_writer_identity(home);
    let qa = crate::channel_writer::plant_identity_for(home, "qa");
    let signer = human_signer(home);
    let mut s = Session::create(home, "qa").unwrap();
    s.append("agent", "ready", None, &[]).unwrap();
    let id = s.channel_id().unwrap().to_string();
    enroll_agent_sealed(home, &id, "qa", &qa);
    // The typed line goes through the human writer, as C2 makes `murmur` do.
    human_write(
        home,
        &id,
        &signer,
        EventKind::Message,
        serde_json::json!({ "text": "find the bug" }),
    );
    let evs = ChannelService::open(home)
        .unwrap()
        .load_events(&id)
        .unwrap();
    assert!(
        evs.iter().any(|e| e.kind == EventKind::OwnerKeyEnrolled),
        "fixture wrote an owner record"
    );
    id
}

#[test]
fn owner_record_is_not_a_turn() {
    let tmp = TempDir::new().unwrap();
    let id = enrolled_session(tmp.path());
    let turns = load(tmp.path(), &id, "qa").unwrap();
    let shape: Vec<(&str, &str)> = turns
        .iter()
        .map(|t| (t.role.as_str(), t.text.as_str()))
        .collect();
    assert_eq!(shape, vec![("agent", "ready"), ("user", "find the bug")]);
}

#[test]
fn preview_skips_owner_record() {
    let tmp = TempDir::new().unwrap();
    let id = enrolled_session(tmp.path());
    let rows = list_recent(tmp.path(), "qa", 10).unwrap();
    let row = rows.iter().find(|r| r.id == id).expect("listed");
    assert_eq!(row.preview, "find the bug");
}

/// H1: on an owned session the agent's reply is router-sealed, not refused
/// with "existing head fails signature verification" (the C2 blocker).
#[test]
fn agent_reply_after_enroll_is_router_sealed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let id = enrolled_session(home);
    let mut s = Session::open_existing(home, "qa", &id).unwrap();
    let seq = s.append("agent", "on it", None, &[]).unwrap();
    let head = mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(home), &id)
        .unwrap()
        .expect("anchored");
    assert_eq!(head.seq, seq, "the head covers the agent reply");
    let router = mur_common::identity::AgentIdentity::load(
        &home
            .join("agents")
            .join(crate::channel_writer::CONTROL_PLANE_AGENT),
    )
    .unwrap();
    assert!(head.verify(&router.verifying_key_bytes()));
    assert!(!mur_channel::anchor::is_dirty(home, &id));
}
