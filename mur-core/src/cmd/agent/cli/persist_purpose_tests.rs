//! #2515: resume and the switch listing only ever offer conversations. A run
//! channel the agent takes part in is newer than the last chat far more often
//! than not, and a line typed into it lands in the run's thread.

use super::*;
use mur_channel::service::ChannelOwner;
use tempfile::TempDir;

const AGENT: &str = "qa";
const FLEET: &str = "dev";

/// A conversation, then a NEWER fleet channel with `AGENT` as a member and one
/// event in it (an empty channel is skipped for a different reason). Returns
/// `(conversation id, fleet channel id)`.
fn conversation_then_fleet_run(home: &Path) -> (String, String) {
    crate::channel_writer::plant_writer_identity(home);
    crate::channel_writer::plant_identity_for(home, AGENT);
    crate::channel_writer::plant_human_identity(home);
    let mut s = Session::create(home, AGENT).unwrap();
    s.append("user", "find the bug", None, &[]).unwrap();
    let conversation = s.channel_id().unwrap().to_string();

    let svc = ChannelService::open(home).unwrap();
    let fleet = svc
        .create_for_fleet(
            FLEET,
            crate::channel_writer::CONTROL_PLANE_AGENT,
            &[AGENT.to_string()],
            ChannelOwner::Legacy,
        )
        .unwrap()
        .id;
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        &fleet,
        crate::channel_writer::Authorship::system(),
        EventKind::Note,
        serde_json::json!({ "text": "iteration 1" }),
        None,
    )
    .unwrap();
    (conversation, fleet)
}

#[test]
fn resume_picks_the_conversation_not_a_newer_fleet_run() {
    let tmp = TempDir::new().unwrap();
    let (conversation, fleet) = conversation_then_fleet_run(tmp.path());
    // Sanity: the run really is the newest channel the agent is in, so this
    // test fails for the right reason if the purpose filter is ever dropped.
    let newest = list_followable(tmp.path(), AGENT, 1).unwrap();
    assert_eq!(newest[0].id, fleet, "fixture: the fleet run is newest");

    let resumed = latest(tmp.path(), AGENT).unwrap().expect("a conversation");
    assert_eq!(resumed.id, conversation);
}

#[test]
fn the_switch_listing_leaves_run_channels_out() {
    let tmp = TempDir::new().unwrap();
    let (conversation, fleet) = conversation_then_fleet_run(tmp.path());
    let ids: Vec<String> = list_recent(tmp.path(), AGENT, 10)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, vec![conversation], "{fleet} must not be listed");
}

/// `/channels N --follow` only reads, so a run stays watchable.
#[test]
fn a_run_channel_can_still_be_followed() {
    let tmp = TempDir::new().unwrap();
    let (conversation, fleet) = conversation_then_fleet_run(tmp.path());
    let ids: Vec<String> = list_followable(tmp.path(), AGENT, 10)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, vec![fleet, conversation]);
}

/// The run is no longer in the listing, so its number is not on screen. Its
/// id is `fleet-<name>`, which the user can type without looking it up.
#[test]
fn a_fleet_run_is_followable_by_the_id_the_user_can_guess() {
    use crate::cmd::agent::cli::app::{SlashCmd, parse_slash};
    use crate::cmd::agent::cli::slash_cmds::resolve;
    let tmp = TempDir::new().unwrap();
    let (_, fleet) = conversation_then_fleet_run(tmp.path());
    let typed = format!("/channels #fleet-{FLEET} --follow");
    let Some(SlashCmd::Channels {
        target: Some(target),
        follow: true,
    }) = parse_slash(&typed)
    else {
        panic!("{typed} did not parse as a follow");
    };

    let followable = list_followable(tmp.path(), AGENT, 10).unwrap();
    assert_eq!(resolve(&followable, &target).expect("resolves").id, fleet);
    // The switch path sees the same words and finds nothing to attach to.
    let switchable = list_recent(tmp.path(), AGENT, 10).unwrap();
    assert!(resolve(&switchable, &target).is_err());
}
