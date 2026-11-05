//! `latest_for_agent` resume contract: mobile and Hub chat resume only into
//! conversations, never a fleet or workflow run the agent joined later.

use super::*;
use tempfile::TempDir;

#[test]
fn latest_for_agent_skips_a_newer_fleet_channel() {
    // The resume path for mobile and Hub chat. A fleet run the agent joined
    // after its last chat must not hijack "continue my conversation".
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let chat = svc.create_for_agent("mur").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let fleet = svc
        .create_for_fleet("projectx", "mur", &["mur".into()])
        .unwrap();
    svc.append_message(
        &fleet.id,
        ChannelActor::Agent { id: "mur".into() },
        EventKind::Message,
        "much later",
        None,
    )
    .unwrap();

    assert_eq!(
        svc.latest_for_agent("mur").unwrap().as_deref(),
        Some(chat.id.as_str())
    );
}

#[test]
fn latest_for_agent_skips_a_workflow_channel() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let chat = svc.create_for_agent("mur").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let wf = svc.create_for_workflow("nightly").unwrap();
    svc.add_participant(
        &wf.id,
        "mur",
        mur_common::channel::ParticipantRole::Delegate,
    )
    .unwrap();
    assert_eq!(
        svc.latest_for_agent("mur").unwrap().as_deref(),
        Some(chat.id.as_str())
    );
}

#[test]
fn latest_for_agent_is_none_when_only_runs_exist() {
    // With no conversation yet, callers create one rather than writing the
    // user's chat into a fleet run.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    svc.create_for_fleet("projectx", "mur", &["mur".into()])
        .unwrap();
    assert!(svc.latest_for_agent("mur").unwrap().is_none());
}

#[test]
fn latest_for_agent_still_returns_an_empty_conversation() {
    // Hub creates a channel before the first exchange lands and must find it
    // again on the next call, so empty conversations stay eligible.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let chat = svc.create_for_agent("mur").unwrap();
    assert_eq!(
        svc.latest_for_agent("mur").unwrap().as_deref(),
        Some(chat.id.as_str())
    );
}
