//! Retract spec §4.4: ESC×2 that puts a message back in the composer also
//! withdraws it from the channel the model reads, by appending one signed
//! `Retract`; replay hides the withdrawn message.

use super::super::*;
use crossterm::event::{KeyEvent, KeyEventState};
use mur_common::channel::{ChannelActor, EventKind, RetractPayload, RetractReason};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

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

fn events(app: &App) -> Vec<mur_common::channel::ChannelEvent> {
    let id = app.channel.as_ref().expect("channel created").id.clone();
    mur_channel::ChannelService::open(&app.home)
        .unwrap()
        .load_events(&id)
        .unwrap()
}

fn retracts(app: &App) -> Vec<RetractPayload> {
    events(app)
        .into_iter()
        .filter(|e| e.kind == EventKind::Retract)
        .map(|e| serde_json::from_value(e.payload).unwrap())
        .collect()
}

/// Send `text` the way the user does: composer, then Enter (`submit`
/// records `last_sent`, which ESC×2 restores).
async fn send(app: &mut App, text: &str, tx: &mpsc::Sender<StreamMsg>) {
    app.set_input(text);
    handle_event(app, key(KeyCode::Enter), tx).await;
}

async fn esc_twice(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    handle_event(app, key(KeyCode::Esc), tx).await;
    handle_event(app, key(KeyCode::Esc), tx).await;
}

#[tokio::test]
async fn esc_cancel_with_empty_composer_retracts_the_user_event() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    send(&mut app, "foo", &tx).await;
    let (_, seq) = app.pending_channel.clone().expect("turn names its event");

    esc_twice(&mut app, &tx).await;

    assert_eq!(app.input_text(), "foo", "message restored");
    assert_eq!(
        retracts(&app),
        vec![RetractPayload {
            target_seq: seq,
            reason: RetractReason::Cancelled
        }]
    );
    let r = events(&app)
        .into_iter()
        .find(|e| e.kind == EventKind::Retract)
        .unwrap();
    assert_eq!(r.actor, ChannelActor::local_human());
    assert_eq!(
        r.idempotency_key.as_deref(),
        Some(format!("retract:{seq}").as_str())
    );
    assert!(r.sig.is_some(), "signed by the channel writer");
    assert!(app.pending_channel.is_none());
    assert!(app.inflight_params.is_none());
}

#[tokio::test]
async fn esc_cancel_over_a_steer_draft_retracts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    send(&mut app, "foo", &tx).await;
    app.set_input("steer this way");

    esc_twice(&mut app, &tx).await;

    assert!(retracts(&app).is_empty());
}

#[tokio::test]
async fn esc_cancel_after_output_retracts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    send(&mut app, "foo", &tx).await;
    app.turn_produced_output = true;

    esc_twice(&mut app, &tx).await;

    assert!(retracts(&app).is_empty());
}

#[tokio::test]
async fn replay_hides_the_withdrawn_message() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    send(&mut app, "foo", &tx).await;
    esc_twice(&mut app, &tx).await;
    app.clear_input();
    send(&mut app, "foo bar", &tx).await;

    let id = app.channel.as_ref().unwrap().id.clone();
    let turns = persist::load(&app.home, &id, "a").unwrap();
    let texts: Vec<_> = turns.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, vec!["foo bar"]);
}
