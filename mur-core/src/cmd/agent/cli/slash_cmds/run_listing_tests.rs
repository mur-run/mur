//! `/channels` typed in the composer, on a home where the agent has one
//! conversation and takes part in one fleet run: the listing shows the run's
//! number, that number follows it, and the same number without `--follow`
//! says why it will not switch.

use crate::cmd::agent::cli::Role;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::persist::{self, Session};
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::agent::cli::turn::submit;
use mur_channel::service::ChannelOwner;
use std::path::Path;
use tokio::sync::mpsc;

const AGENT: &str = "qa";
const FLEET_CHANNEL: &str = "fleet-dev";

/// A pane on a conversation with one line in it, plus a newer fleet channel
/// the pane's agent is a member of.
fn pane_with_a_run(home: &Path) -> App {
    crate::channel_writer::plant_writer_identity(home);
    crate::channel_writer::plant_identity_for(home, AGENT);
    crate::channel_writer::plant_human_identity(home);
    let mut session = Session::create(home, AGENT).unwrap();
    session.append("user", "find the bug", None, &[]).unwrap();
    let svc = mur_channel::ChannelService::open(home).unwrap();
    svc.create_for_fleet(
        "dev",
        crate::channel_writer::CONTROL_PLANE_AGENT,
        &[AGENT.to_string()],
        ChannelOwner::Legacy,
    )
    .unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        FLEET_CHANNEL,
        crate::channel_writer::Authorship::system(),
        mur_common::channel::EventKind::Note,
        serde_json::json!({ "text": "iteration 1" }),
        None,
    )
    .unwrap();
    App::new(
        home.to_path_buf(),
        AGENT.into(),
        session,
        &crate::cmd::agent::cli::theme::ANSI,
    )
}

async fn type_line(app: &mut App, line: &str) -> String {
    let before = app.messages.len();
    app.set_input(line);
    submit(app, &mpsc::channel::<StreamMsg>(8).0).await;
    app.messages[before..]
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_number(home: &Path) -> u64 {
    persist::list_runs(home, AGENT, 10).unwrap()[0].ordinal
}

#[tokio::test]
async fn the_listing_shows_a_run_under_its_own_heading_with_its_number() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    let n = run_number(tmp.path());

    let text = type_line(&mut app, "/channels").await;

    let (conversations, runs) = text
        .split_once("runs (follow only")
        .unwrap_or_else(|| panic!("no run section: {text}"));
    assert!(conversations.contains("find the bug"), "{text}");
    assert!(!conversations.contains(FLEET_CHANNEL), "{text}");
    assert!(
        runs.contains(&format!("  {n} · #{FLEET_CHANNEL} · ")),
        "the run's row gives both handles: {text}"
    );
}

#[tokio::test]
async fn the_number_shown_for_a_run_follows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    let n = run_number(tmp.path());

    let text = type_line(&mut app, &format!("/channels {n} --follow")).await;

    assert!(text.contains("following"), "{text}");
    assert_eq!(
        app.follow.as_ref().map(|f| f.channel_id.as_str()),
        Some(FLEET_CHANNEL)
    );
}

/// #2515's guarantee, now that the number is on screen to be typed: the pane
/// stays where it is, and the refusal is the command that works.
#[tokio::test]
async fn the_number_shown_for_a_run_does_not_switch_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    let n = run_number(tmp.path());
    let conversation = persist::list_recent(tmp.path(), AGENT, 1).unwrap()[0]
        .id
        .clone();
    let events_in = |id: &str| {
        mur_channel::ChannelService::open(tmp.path())
            .unwrap()
            .load_events(id)
            .unwrap()
            .len()
    };
    let (chat_before, run_before) = (events_in(&conversation), events_in(FLEET_CHANNEL));

    let text = type_line(&mut app, &format!("/channels {n}")).await;

    assert!(
        text.contains(&format!("/channels {n} --follow")),
        "the refusal names the command that works: {text}"
    );
    assert!(!text.contains("no channel"), "{text}");
    assert!(!text.contains("switched to channel"), "{text}");
    assert!(app.follow.is_none(), "refusing must not start a follow");
    assert_eq!(
        (events_in(&conversation), events_in(FLEET_CHANNEL)),
        (chat_before, run_before),
        "a refused switch writes nothing"
    );

    // By id, the refusal repeats the id form.
    let text = type_line(&mut app, &format!("/channels #{FLEET_CHANNEL}")).await;
    assert!(
        text.contains(&format!("/channels #{FLEET_CHANNEL} --follow")),
        "{text}"
    );
}

/// A home with runs and no conversation yet still has something to list.
#[tokio::test]
async fn a_pane_with_only_runs_lists_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    let conversation = persist::list_recent(tmp.path(), AGENT, 1).unwrap()[0]
        .id
        .clone();
    mur_channel::ChannelService::open(tmp.path())
        .unwrap()
        .delete_channel(&conversation)
        .unwrap();

    let text = type_line(&mut app, "/channels").await;

    assert!(text.starts_with("runs (follow only"), "{text}");
    assert!(!text.contains("no channels yet"), "{text}");
}

/// The run refusal answers "nothing switchable has that name". A prefix that
/// is too short was never looked up, so it keeps its own message even though
/// the same letters would find the run.
#[tokio::test]
async fn a_too_short_prefix_is_not_reported_as_a_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());

    let text = type_line(&mut app, "/channels #fle").await;

    assert!(text.contains("too short"), "{text}");
    assert!(!text.contains("--follow"), "{text}");
}
