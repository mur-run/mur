//! at-mention C1: murmur names the user event a turn answers
//! (`context.channel_id` + `context.channel_seq`) and refuses the turn when
//! that event could not be written.

use super::super::*;
use mur_common::channel::{ChannelActor, EventKind};

/// An app on a home that outlives the test body (the plain fixture drops its
/// tempdir), with a signing identity for agent `a` planted.
fn app_in(home: &tempfile::TempDir) -> App {
    crate::channel_writer::plant_identity_for(home.path(), "a");
    let session = Session::create(home.path(), "a").unwrap();
    App::new(
        home.path().to_path_buf(),
        "a".into(),
        session,
        &super::super::theme::ANSI,
    )
}

/// Every `Human` event's seq in the pane's channel, oldest first.
fn human_seqs(app: &App) -> Vec<u64> {
    let id = app.channel.as_ref().expect("channel created").id.clone();
    mur_channel::ChannelService::open(&app.home)
        .unwrap()
        .load_events(&id)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.actor, ChannelActor::Human { .. }) && e.kind == EventKind::Message)
        .map(|e| e.seq)
        .collect()
}

/// Make every later append to the pane's channel fail: a manifest from a
/// newer build refuses the append under the lock (A3 gate).
fn break_channel(app: &App) {
    let id = app.channel.as_ref().expect("channel created").id.clone();
    let manifest = app.home.join("channels").join(&id).join("channel.yaml");
    std::fs::write(manifest, "v: 999\n").unwrap();
}

#[tokio::test]
async fn a_typed_turn_sends_the_channel_id_and_the_user_events_seq() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    start_turn(&mut app, "hello".into(), &tx);
    let params = app.inflight_params.clone().expect("turn started");
    let seqs = human_seqs(&app);
    assert_eq!(seqs.len(), 1);
    let id = app.channel.as_ref().unwrap().id.clone();
    assert_eq!(params["context"]["channel_id"], id.as_str());
    assert_eq!(params["context"]["channel_seq"], seqs[0]);
    assert_eq!(app.pending_channel, Some((id, seqs[0])));
}

#[tokio::test]
async fn a_failed_append_refuses_the_turn() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    start_turn(&mut app, "first".into(), &tx);
    app.streaming = false;
    app.current_task_id = None;
    app.inflight_params = None;
    break_channel(&app);
    let before = app.messages.len();

    start_turn(&mut app, "second".into(), &tx);

    assert!(!app.streaming, "no turn started");
    assert!(app.inflight_params.is_none(), "nothing dialed");
    assert!(
        !app.messages.iter().any(|m| m.text == "second"),
        "the unsaved user bubble is withdrawn"
    );
    assert!(
        app.messages.iter().any(|m| m
            .text
            .contains("message not saved to the channel; not sent")),
        "{:?}",
        app.messages.iter().map(|m| &m.text).collect::<Vec<_>>()
    );
    assert!(app.messages.len() > before, "the refusal is shown");
}

/// `retry_send` replays the params untouched, so the replay still names the
/// same user event — it is the same turn, not a new one.
#[tokio::test]
async fn a_replayed_send_keeps_the_channel_seq() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    start_turn(&mut app, "hello".into(), &tx);
    let first = app.inflight_params.clone().unwrap();
    slash_cmds::retry_send(&mut app, first.clone(), &tx);
    let replay = app.inflight_params.clone().unwrap();
    assert_ne!(replay["task_id"], first["task_id"]);
    assert_eq!(
        replay["context"]["channel_seq"],
        first["context"]["channel_seq"]
    );
    assert_eq!(
        replay["context"]["channel_id"],
        first["context"]["channel_id"]
    );
}

/// A lost turn is re-sent as a new user turn: a second Human event, and the
/// in-flight params name the second one.
#[tokio::test]
async fn a_lost_turn_resend_names_the_new_user_event() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    start_turn(&mut app, "hello".into(), &tx);
    let task_id = app.current_task_id.clone().unwrap();
    handle_stream(
        &mut app,
        StreamMsg::TurnLost {
            task_id,
            note: "agent restarted".into(),
            resend: Some("hello".into()),
        },
        &tx,
    );
    let seqs = human_seqs(&app);
    assert_eq!(seqs.len(), 2, "{seqs:?}");
    let params = app.inflight_params.clone().unwrap();
    assert_eq!(params["context"]["channel_seq"], seqs[1]);
}

/// S1: a shell-started turn names the shell Note it was derived from.
#[tokio::test]
async fn a_shell_started_turn_sends_the_notes_seq() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
    let gen_id = app.shell.begin(0, c_tx).expect("slot");
    app.begin_shell("ls");
    app.append_shell_output("a");
    handle_stream(
        &mut app,
        StreamMsg::ShellDone {
            gen_id,
            cmd: "ls".into(),
            end: shell::ShellEnd::Exited(0),
        },
        &tx,
    );
    let id = app.channel.as_ref().unwrap().id.clone();
    let note_seq = mur_channel::ChannelService::open(&app.home)
        .unwrap()
        .load_events(&id)
        .unwrap()
        .iter()
        .rfind(|e| e.payload.get("text").is_some())
        .map(|e| e.seq)
        .unwrap();
    let params = app.inflight_params.clone().expect("turn started");
    assert_eq!(params["context"]["channel_id"], id.as_str());
    assert_eq!(params["context"]["channel_seq"], note_seq);
}

/// A new conversation forgets the old channel's seq.
#[tokio::test]
async fn a_new_session_clears_the_pending_channel() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    start_turn(&mut app, "hello".into(), &tx);
    assert!(app.pending_channel.is_some());
    app.start_new_session(Session::create(home.path(), "a").unwrap());
    assert!(app.pending_channel.is_none());
}
