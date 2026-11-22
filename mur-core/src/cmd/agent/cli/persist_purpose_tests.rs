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

/// The follow-only half of the `/channels` listing: runs, and nothing a typed
/// line could be sent into. A run has no human line, so its title is what the
/// row shows.
#[test]
fn the_run_listing_holds_only_runs() {
    let tmp = TempDir::new().unwrap();
    let (_, fleet) = conversation_then_fleet_run(tmp.path());
    let runs = list_runs(tmp.path(), AGENT, 10).unwrap();
    assert_eq!(
        runs.len(),
        1,
        "the conversation must not be listed as a run"
    );
    assert_eq!(runs[0].id, fleet);
    assert_eq!(runs[0].preview, format!("fleet: {FLEET}"));
}

/// A human line inside a run is not what the run is. Channels attached to by
/// mistake before #2515 carry one, and it must not become the row's label.
#[test]
fn a_run_is_labelled_by_its_title_even_with_a_human_line_in_it() {
    let tmp = TempDir::new().unwrap();
    let (_, fleet) = conversation_then_fleet_run(tmp.path());
    let mut stray = Session::open_existing(tmp.path(), AGENT, &fleet).unwrap();
    stray.append("user", "HI", None, &[]).unwrap();

    let runs = list_runs(tmp.path(), AGENT, 10).unwrap();
    assert_eq!(runs[0].preview, format!("fleet: {FLEET}"));
}

/// `/channels N --follow` takes a run's number, so the listing has to show
/// it: a handle the user is told to type and cannot read anywhere is not one.
/// Round trip: the number printed on the run's row follows that run.
#[test]
fn the_number_the_listing_prints_for_a_run_follows_that_run() {
    use crate::cmd::agent::cli::app::{SlashCmd, parse_slash};
    use crate::cmd::agent::cli::slash_cmds::{listing, resolve};
    let tmp = TempDir::new().unwrap();
    let (_, fleet) = conversation_then_fleet_run(tmp.path());
    let conversations = list_recent(tmp.path(), AGENT, 10).unwrap();
    let runs = list_runs(tmp.path(), AGENT, 10).unwrap();

    let text = listing(&conversations, &runs).expect("a listing");
    let (above, below) = text
        .split_once("runs (")
        .unwrap_or_else(|| panic!("no run section: {text}"));
    assert!(
        !above.contains(&fleet),
        "a run must not sit among the switchable rows: {text}"
    );
    let row = below
        .lines()
        .find(|l| l.contains(&fleet))
        .unwrap_or_else(|| panic!("the run's whole id is its other handle: {text}"));
    let number = row.split_whitespace().next().expect("a number column");

    let typed = format!("/channels {number} --follow");
    let Some(SlashCmd::Channels {
        target: Some(target),
        follow: true,
    }) = parse_slash(&typed)
    else {
        panic!("{typed} did not parse as a follow");
    };
    let followable = list_followable(tmp.path(), AGENT, 10).unwrap();
    assert_eq!(resolve(&followable, &target).expect("resolves").id, fleet);
}

/// The run's number is on screen now, so `/channels N` without `--follow`
/// will be typed. "no channel N — /channels to list" would contradict the
/// listing it points at; the refusal names the command that does work.
#[test]
fn switching_to_a_run_is_refused_with_the_follow_command() {
    use crate::cmd::agent::cli::app::ChannelRef;
    use crate::cmd::agent::cli::slash_cmds::follow_only_msg;
    let tmp = TempDir::new().unwrap();
    let (conversation, fleet) = conversation_then_fleet_run(tmp.path());
    let all = list_followable(tmp.path(), AGENT, 10).unwrap();
    let number_of = |id: &str| all.iter().find(|s| s.id == id).unwrap().ordinal;

    let run = ChannelRef::Ordinal(number_of(&fleet));
    let msg = follow_only_msg(tmp.path(), AGENT, &run).expect("a run gets its own refusal");
    assert!(
        msg.contains(&format!("/channels {} --follow", number_of(&fleet))),
        "got: {msg}"
    );

    // Only a run: a conversation is switchable, and an unknown number is
    // simply unknown.
    let chat = ChannelRef::Ordinal(number_of(&conversation));
    assert!(follow_only_msg(tmp.path(), AGENT, &chat).is_none());
    assert!(follow_only_msg(tmp.path(), AGENT, &ChannelRef::Ordinal(999)).is_none());
}
