//! `/channels` typed in the composer, on a home where the agent has one
//! conversation and takes part in one fleet run: the listing shows the run's
//! number, that number follows it, and the same number without `--follow`
//! says why it will not switch. Once it is followed, every line that names it
//! names it by a handle that can be typed back.

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

/// A second fleet whose id shares its first eight characters with
/// `FLEET_CHANNEL` — the pair an eight-character label cannot tell apart.
const SIBLING_FLEET: &str = "devteam";
const SIBLING_CHANNEL: &str = "fleet-devteam";

fn note_in(home: &Path, channel: &str, text: &str) {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        channel,
        crate::channel_writer::Authorship::system(),
        mur_common::channel::EventKind::Note,
        serde_json::json!({ "text": text }),
        None,
    )
    .unwrap();
}

/// A channel with no events is not listed, so the sibling gets one.
fn add_sibling_fleet(home: &Path) {
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            SIBLING_FLEET,
            crate::channel_writer::CONTROL_PLANE_AGENT,
            &[AGENT.to_string()],
            ChannelOwner::Legacy,
        )
        .unwrap();
    note_in(home, SIBLING_CHANNEL, "iteration 1");
}

/// The word after "following " in a system line.
fn label_after<'a>(text: &'a str, lead: &str) -> &'a str {
    let rest = text
        .split_once(lead)
        .unwrap_or_else(|| panic!("no `{lead}` in: {text}"))
        .1;
    rest.split_whitespace().next().unwrap_or_default()
}

fn followed(app: &App) -> Option<&str> {
    app.follow.as_ref().map(|f| f.channel_id.as_str())
}

/// The label is a handle, so it has to work as one: typed back at
/// `/channels`, it follows the channel it was printed for. With two fleets
/// starting `fleet-de`, eight characters of the id did not.
#[tokio::test]
async fn the_label_of_a_followed_run_types_back_to_that_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    add_sibling_fleet(tmp.path());

    for channel in [FLEET_CHANNEL, SIBLING_CHANNEL] {
        let text = type_line(&mut app, &format!("/channels #{channel} --follow")).await;
        let label = label_after(&text, "following ").to_string();
        assert_eq!(label, format!("#{channel}"), "{text}");

        let stopped = type_line(&mut app, "/channels --stop").await;
        assert_eq!(label_after(&stopped, "stopped following "), label);
        assert!(app.follow.is_none());

        type_line(&mut app, &format!("/channels {label} --follow")).await;
        assert_eq!(followed(&app), Some(channel), "typed back: {label}");
        type_line(&mut app, "/channels --stop").await;
    }
}

/// The same label heads every tailed line and the `--stop` row of the menu,
/// so two followed fleets are told apart there too.
#[tokio::test]
async fn tailed_lines_and_the_stop_row_carry_the_whole_run_id() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    add_sibling_fleet(tmp.path());
    type_line(&mut app, &format!("/channels #{SIBLING_CHANNEL} --follow")).await;

    assert_eq!(
        app.current_values().following.as_deref(),
        Some(format!("#{SIBLING_CHANNEL}").as_str())
    );

    note_in(tmp.path(), SIBLING_CHANNEL, "iteration 2");
    let before = app.messages.len();
    app.poll_follow(std::time::Instant::now());
    let tailed: Vec<&str> = app.messages[before..]
        .iter()
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(tailed.len(), 1, "{tailed:?}");
    assert!(
        tailed[0].starts_with(&format!("⟨#{SIBLING_CHANNEL}⟩ ")),
        "{tailed:?}"
    );
    assert!(tailed[0].contains("iteration 2"), "{tailed:?}");
}

/// A conversation's id is a UUID: its label is the id handle the listing
/// shows for it, and that label typed back follows the same conversation —
/// even though both conversations here are minted milliseconds apart and so
/// share their first eight characters (#2538).
#[tokio::test]
async fn the_label_of_a_followed_conversation_types_back_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = pane_with_a_run(tmp.path());
    let mut other = Session::create(tmp.path(), AGENT).unwrap();
    other.append("user", "another thread", None, &[]).unwrap();
    let listed = persist::list_recent(tmp.path(), AGENT, 10).unwrap();
    let row = listed
        .iter()
        .find(|s| s.preview.contains("another thread"))
        .expect("the second conversation is listed");
    let (other_id, handle) = (row.id.clone(), row.handle.clone());
    assert!(other_id.replace('-', "").starts_with(&handle), "{handle}");

    // A whole UUID carries dashes, so it is an id only behind the `#`.
    let text = type_line(&mut app, &format!("/channels #{other_id} --follow")).await;

    assert_eq!(followed(&app), Some(other_id.as_str()), "{text}");
    assert_eq!(label_after(&text, "following "), handle, "{text}");
    let stopped = type_line(&mut app, "/channels --stop").await;
    assert_eq!(label_after(&stopped, "stopped following "), handle);

    type_line(&mut app, &format!("/channels {handle} --follow")).await;
    assert_eq!(
        followed(&app),
        Some(other_id.as_str()),
        "typed back: {handle}"
    );
}
