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
    crate::channel_writer::plant_human_identity(home.path());
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
    // C2: signed by the human key, the same actor as the turn it withdraws.
    let target = events(&app).into_iter().find(|e| e.seq == seq).unwrap();
    assert!(matches!(&r.actor, ChannelActor::Human { key: Some(_), .. }));
    assert_eq!(
        r.actor, target.actor,
        "a retract's actor must equal its target's"
    );
    assert_eq!(
        r.idempotency_key.as_deref(),
        Some(format!("retract:{seq}").as_str())
    );
    assert!(r.sig.is_some(), "signed by the human key");
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

/// #2446 review: when the runtime refuses the retract (the turn had already
/// run tools) it records that in the log; replay then keeps the message the
/// model still receives, and shows the refusal as a local notice, never as
/// an agent reply.
#[tokio::test]
async fn replay_keeps_a_message_whose_retract_the_runtime_refused() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    send(&mut app, "foo", &tx).await;
    let (id, seq) = app.pending_channel.clone().unwrap();
    esc_twice(&mut app, &tx).await;
    // What the runtime's gate appends when it refuses (§4.3).
    let agent =
        mur_common::identity::AgentIdentity::load(&home.path().join("agents").join("a")).unwrap();
    mur_channel::ChannelService::open(home.path())
        .unwrap()
        .append_signed(
            &id,
            &agent,
            1,
            ChannelActor::Agent { id: "a".into() },
            EventKind::Note,
            mur_common::channel::RetractRefusal { target_seq: seq }.to_payload(),
            Some(mur_common::channel::retract_refused_idempotency_key(seq)),
        )
        .unwrap();
    app.clear_input();
    send(&mut app, "foo bar", &tx).await;

    let turns = persist::load(&app.home, &id, "a").unwrap();
    let got: Vec<_> = turns
        .iter()
        .map(|t| (t.role.as_str(), t.text.as_str()))
        .collect();
    assert_eq!(got[0], ("user", "foo"), "{got:?}");
    assert_eq!(got[1].0, persist::REFUSAL_ROLE, "{got:?}");
    assert!(got[1].1.contains("already run tools"), "{got:?}");
    assert_eq!(got[2], ("user", "foo bar"), "{got:?}");

    let mut fresh = App::new(
        home.path().to_path_buf(),
        "a".into(),
        Session::create(home.path(), "a").unwrap(),
        &super::super::theme::ANSI,
    );
    fresh.load_history(turns);
    let roles: Vec<_> = fresh.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::System, Role::User]);
}
