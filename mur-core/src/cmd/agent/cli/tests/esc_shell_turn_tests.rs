//! PR #2443 review: double-ESC on a `!cmd` turn, through the real key path.
//! That turn has no User bubble, so `last_sent` is an earlier turn's text;
//! restoring it and withdrawing "the last User bubble" erased that earlier,
//! completed turn from the transcript.

use super::super::*;

use crossterm::event::{KeyEvent, KeyEventState};

fn esc() -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

#[tokio::test]
async fn double_esc_on_shell_turn_keeps_the_earlier_turn() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.last_sent = Some("earlier question".into());
    app.begin_user_turn("earlier question")
        .expect("turn starts");
    app.finish_agent_turn("earlier answer".into(), None);
    app.begin_turn(); // what `start_shell_turn` does

    handle_event(&mut app, esc(), &tx).await;
    handle_event(&mut app, esc(), &tx).await;

    assert!(!app.streaming, "the shell turn was cancelled");
    assert!(app.input_text().is_empty(), "no stale text restored");
    assert!(app.messages.iter().any(|m| m.text == "earlier question"));
    assert!(app.messages.iter().any(|m| m.text == "earlier answer"));
}

#[tokio::test]
async fn double_esc_on_user_turn_still_restores_and_withdraws() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.last_sent = Some("hello".into());
    app.begin_user_turn("hello").expect("turn starts");

    handle_event(&mut app, esc(), &tx).await;
    handle_event(&mut app, esc(), &tx).await;

    assert_eq!(app.input_text(), "hello");
    assert!(!app.messages.iter().any(|m| m.role == Role::User));
}
