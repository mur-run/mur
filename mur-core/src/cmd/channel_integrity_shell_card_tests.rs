//! A murmur `!cmd` card is a `System` note the SESSION AGENT signs, not the
//! router (`cmd/agent/cli/persist.rs`); the conversation watermark reads it
//! under that key (S1). The check must read it the same way: for an agent
//! participant of a conversation only, and naming the signer (#2519).

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
/// An agent the `ask` tool added to `AGENT`'s conversation.
const GUEST: &str = "helper";

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
        crate::channel_writer::Authorship::out_of_role(
            crate::channel_writer::Signer::Agent(signer),
            ChannelActor::System,
        ),
        EventKind::Note,
        serde_json::json!({ "text": "$ rm -rf /" }),
        None,
    );
}

/// What the `ask` tool does to the asker's conversation
/// (`mur-agent-runtime/src/tools/ask/mod.rs`).
fn join(home: &Path, channel_id: &str, agent: &str) {
    plant_identity_for(home, agent);
    ChannelService::open(home)
        .unwrap()
        .add_participant(
            channel_id,
            agent,
            mur_common::channel::ParticipantRole::Delegate,
        )
        .unwrap();
}

fn cards(agent: &str, seqs: &[u64]) -> ParticipantCards {
    ParticipantCards {
        agent: agent.to_string(),
        seqs: seqs.to_vec(),
    }
}

fn last_seq(home: &Path, channel_id: &str) -> u64 {
    let events = ChannelStore::new(home).load_events(channel_id).unwrap();
    events.last().unwrap().seq
}

#[test]
fn a_session_agents_shell_card_is_not_a_bad_signature() {
    let tmp = home_for(AGENT);
    let (id, card) = session_with_card(tmp.path(), AGENT);
    for require_sig in [true, false] {
        let r = check_with(tmp.path(), &id, require_sig).unwrap();
        assert!(
            r.unverified_seqs.is_empty(),
            "require_sig={require_sig}: {:?}",
            r.unverified_seqs
        );
        assert!(r.is_intact(), "require_sig={require_sig}:\n{}", r.render());
        assert_eq!(r.exit_code(), 0);
        assert_eq!(r.participant_cards, vec![cards(AGENT, &[card])]);
    }
}

#[test]
fn the_concierges_own_shell_card_still_verifies() {
    let tmp = home_for(crate::channel_writer::CONTROL_PLANE_AGENT);
    let (id, _) = session_with_card(tmp.path(), crate::channel_writer::CONTROL_PLANE_AGENT);
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert!(r.is_intact(), "{}", r.render());
    // The router's own key signed it, so it is not a participant's card.
    assert!(r.participant_cards.is_empty(), "{}", r.render());
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

/// #2519: the asked agent is a participant, so its `System` note passes the
/// signer check. It is not damage, and the report says who signed it.
#[test]
fn a_second_participants_system_note_is_named_in_the_report() {
    let tmp = home_for(AGENT);
    let (id, card) = session_with_card(tmp.path(), AGENT);
    join(tmp.path(), &id, GUEST);
    system_note_as(tmp.path(), &id, GUEST);
    let noted = last_seq(tmp.path(), &id);
    assert_ne!(noted, card, "the guest's note was not written");
    for require_sig in [true, false] {
        let r = check_with(tmp.path(), &id, require_sig).unwrap();
        assert_eq!(
            r.participant_cards,
            vec![cards(AGENT, &[card]), cards(GUEST, &[noted])],
            "require_sig={require_sig}"
        );
        assert!(r.unverified_seqs.is_empty(), "{:?}", r.unverified_seqs);
        let text = r.render();
        assert!(
            text.lines()
                .any(|l| l.contains(&format!("`{GUEST}`")) && l.ends_with(&format!(": {noted}"))),
            "seq {noted} by `{GUEST}` is not in the report:\n{text}"
        );
    }
}

/// Why the signer is named and not refused: a participant may run its own
/// session on the conversation (`persist::list_recent`), and the card it
/// writes there is as genuine as the first agent's.
#[test]
fn a_second_participants_own_session_card_is_not_damage() {
    let tmp = home_for(AGENT);
    let (id, card) = session_with_card(tmp.path(), AGENT);
    join(tmp.path(), &id, GUEST);
    assert!(
        crate::cmd::agent::cli::persist::list_recent(tmp.path(), GUEST, 1)
            .unwrap()
            .iter()
            .any(|s| s.id == id),
        "the guest cannot resume into the conversation"
    );
    let mut s = Session::open_existing(tmp.path(), GUEST, &id).unwrap();
    let theirs = s.append("shell", "$ pwd", None, &[]).unwrap();
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(
        r.participant_cards,
        vec![cards(AGENT, &[card]), cards(GUEST, &[theirs])]
    );
}

/// A line that is reported as damage is not also listed as a card.
#[test]
fn a_reported_line_is_not_listed_as_a_card() {
    let tmp = home_for(AGENT);
    plant_identity_for(tmp.path(), OUTSIDER);
    let (id, card) = session_with_card(tmp.path(), AGENT);
    system_note_as(tmp.path(), &id, OUTSIDER);
    let r = check_with(tmp.path(), &id, true).unwrap();
    assert_eq!(r.participant_cards, vec![cards(AGENT, &[card])]);
}
