//! Guards the `Ctrl+T` / `Alt+M` monitor shortcut through the real
//! `handle_event` dispatch — not a hand-maintained list of which chars are
//! "supposed to" be bound, which can drift from the match arms it claims to
//! guard without ever failing. See `events.rs`'s `KeyCode::Char('t') if
//! ctrl` / `KeyCode::Char('m' | 'M') if alt` arms and the comment on why
//! `Ctrl+M` is never bound (`^M` is Enter on a real terminal).

use super::super::*;

use crossterm::event::{KeyEvent, KeyEventState};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn alt(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::ALT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// A store-backed fixture answers with the real `render_list` text ("no
/// monitors" on an empty store — `cmd/monitor.rs`), so this is the monitor
/// handler's genuine observable effect on `App`, not a mock.
fn opened_the_monitor_list(app: &App) -> bool {
    app.messages
        .iter()
        .any(|m| m.role == Role::System && m.text == "no monitors")
}

#[tokio::test]
async fn ctrl_t_reaches_the_monitor_handler() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    assert!(!opened_the_monitor_list(&app), "sanity: nothing yet");

    handle_event(&mut app, ctrl('t'), &tx).await;

    assert!(
        opened_the_monitor_list(&app),
        "Ctrl+T must dispatch to the monitor handler; messages: {:?}",
        app.messages.iter().map(|m| &m.text).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn alt_m_reaches_the_monitor_handler() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();

    handle_event(&mut app, alt('m'), &tx).await;

    assert!(
        opened_the_monitor_list(&app),
        "Alt+M must dispatch to the same monitor handler as Ctrl+T"
    );
}

/// A real terminal never delivers this event at all (`^M` is parsed as
/// Enter, not `Char('m')` + `CONTROL`), so this is a guard against someone
/// *adding* a `Char('m') if ctrl` arm later — precisely the mistake the
/// comment above the match arms warns about — rather than a case any user
/// can trigger.
#[tokio::test]
async fn synthesized_ctrl_m_never_reaches_the_monitor_handler() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    let before = app.messages.len();

    handle_event(&mut app, ctrl('m'), &tx).await;

    assert!(
        !opened_the_monitor_list(&app),
        "Ctrl+M must never open the monitor list — it would steal Enter"
    );
    assert_eq!(
        app.messages.len(),
        before,
        "Ctrl+M must not push any system message via the monitor path"
    );
}

/// The regression this whole Ctrl+M/Alt+M discussion exists to prevent:
/// binding Alt+M must not disturb Enter's ordinary submit path for the very
/// next keystroke.
#[tokio::test]
async fn enter_still_submits_after_alt_m() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();

    handle_event(&mut app, alt('m'), &tx).await;
    assert!(opened_the_monitor_list(&app), "sanity: Alt+M landed");

    for c in "hello".chars() {
        handle_event(&mut app, key(KeyCode::Char(c)), &tx).await;
    }
    assert_eq!(app.input_text(), "hello");

    handle_event(&mut app, key(KeyCode::Enter), &tx).await;

    assert_eq!(app.input_text(), "", "Enter must clear the composer");
    assert_eq!(app.last_sent.as_deref(), Some("hello"), "Enter must submit");
    assert!(app.streaming, "Enter must start a turn");
    assert!(
        app.messages
            .iter()
            .any(|m| m.role == Role::User && m.text == "hello"),
        "the submitted turn must land in the transcript"
    );
}
