//! A murmur `!cmd` card is a `System` note the SESSION AGENT signs, not the
//! router (`cmd/agent/cli/persist.rs`); the conversation watermark reads it
//! under that key (S1). The check must read it the same way, and no wider.

use super::*;
use crate::channel_writer::{
    append_as_writer, plant_human_identity, plant_identity_for, plant_writer_identity,
};
use crate::cmd::agent::cli::persist::Session;
use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use tempfile::TempDir;

const AGENT: &str = "qa";
const OUTSIDER: &str = "intruder";

/// One typed turn, one reply and one `!cmd` card in `agent`'s session.
/// Returns the channel id and the card's seq.
fn session_with_card(home: &Path, agent: &str) -> (String, u64) {
    let mut s = Session::create(home, agent).unwrap();
    s.append("user", "hi", None, &[]).unwrap();
    s.append("agent", "hello", None, &[]).unwrap();
    let card = s.append("shell", "$ ls", None, &[]).unwrap();
    (s.channel_id().unwrap().to_string(), card)
}

fn home_for(agent: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    plant_identity_for(tmp.path(), agent);
    plant_human_identity(tmp.path());
    tmp
}

/// A `System` note signed with `signer`'s key. The seal may be refused (the
/// head is another key's); the line is on disk either way.
fn system_note_as(home: &Path, channel_id: &str, signer: &str) {
    let svc = ChannelService::open(home).unwrap();
    let _ = append_as_writer(
        &svc,
        home,
        channel_id,
        crate::channel_writer::Signer::Agent(signer),
        ChannelActor::System,
        EventKind::Note,
        serde_json::json!({ "text": "$ rm -rf /" }),
        None,
    );
}

fn last_seq(home: &Path, channel_id: &str) -> u64 {
    let events = ChannelStore::new(home).load_events(channel_id).unwrap();
    events.last().unwrap().seq
}

#[test]
fn a_session_agents_shell_card_is_not_a_bad_signature() {
    let tmp = home_for(AGENT);
    let (id, _) = session_with_card(tmp.path(), AGENT);
    for require_sig in [true, false] {
        let r = check_with(tmp.path(), &id, require_sig).unwrap();
        assert!(
            r.unverified_seqs.is_empty(),
            "require_sig={require_sig}: {:?}",
            r.unverified_seqs
        );
        assert!(r.is_intact(), "require_sig={require_sig}:\n{}", r.render());
        assert_eq!(r.exit_code(), 0);
    }
}

#[test]
fn the_concierges_own_shell_card_still_verifies() {
    let tmp = home_for(crate::channel_writer::CONTROL_PLANE_AGENT);
    let (id, _) = session_with_card(tmp.path(), crate::channel_writer::CONTROL_PLANE_AGENT);
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert!(r.is_intact(), "{}", r.render());
}

#[test]
fn a_card_whose_text_was_edited_is_reported() {
    let tmp = home_for(AGENT);
    let (id, card) = session_with_card(tmp.path(), AGENT);
    let path = ChannelStore::new(tmp.path()).events_path(&id).unwrap();
    let log = std::fs::read_to_string(&path).unwrap();
    assert!(log.contains("$ ls"));
    std::fs::write(&path, log.replace("$ ls", "$ id")).unwrap();
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert!(r.unverified_seqs.contains(&card), "{:?}", r.unverified_seqs);
}

#[test]
fn a_system_note_from_an_agent_outside_the_session_is_reported() {
    let tmp = home_for(AGENT);
    plant_identity_for(tmp.path(), OUTSIDER);
    let (id, card) = session_with_card(tmp.path(), AGENT);
    system_note_as(tmp.path(), &id, OUTSIDER);
    let forged = last_seq(tmp.path(), &id);
    assert_ne!(forged, card, "the outsider's note was not written");
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert_eq!(r.unverified_seqs, vec![forged]);
}

#[test]
fn a_members_system_note_on_a_fleet_channel_is_reported() {
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    plant_identity_for(tmp.path(), AGENT);
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc
        .create_for_fleet(
            "dev",
            crate::channel_writer::CONTROL_PLANE_AGENT,
            &[AGENT.to_string()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap()
        .id;
    drop(svc);
    system_note_as(tmp.path(), &id, AGENT);
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert_eq!(r.unverified_seqs, vec![last_seq(tmp.path(), &id)]);
}
