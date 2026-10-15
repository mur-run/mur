//! #1905 manual check 1: `/browser live <task words>` must leave the
//! three-way chooser open after Enter. `submit()` opened it, then the key
//! handler's trailing `refresh_completion` recomputed the menu from the now
//! empty composer and closed it. Unit tests of `task_choices` could not see
//! this — only the real key path does.

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

#[tokio::test]
async fn browser_live_task_words_leave_the_chooser_open_after_enter() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.set_input("/browser live pro360 與家電");

    handle_event(&mut app, key(KeyCode::Enter), &tx).await;

    let c = app
        .completion
        .as_ref()
        .expect("the research-task chooser must survive the Enter key");
    assert!(c.spaced, "it must be the spaced reply chooser");
    assert_eq!(c.items.len(), 3);
}

#[tokio::test]
async fn typing_still_dismisses_an_open_chooser() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.set_input("/browser live pro360 與家電");
    handle_event(&mut app, key(KeyCode::Enter), &tx).await;

    handle_event(&mut app, key(KeyCode::Char('x')), &tx).await;

    assert!(app.completion.is_none(), "free typing replaces the chooser");
}
