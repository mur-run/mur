//! #002: the `!` shell menu must not open uninvited and steal Enter.
//!
//! Reported: typing `!mur agent restart mur` popped a directory menu on the
//! trailing word `mur`, so Enter accepted `mur-agent-gui/` instead of running
//! the command. Esc closes it, but the user never asked for it.

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

async fn type_str(app: &mut App, s: &str, tx: &mpsc::Sender<StreamMsg>) {
    for c in s.chars() {
        handle_event(app, key(KeyCode::Char(c)), tx).await;
    }
}

/// A repo whose directories all start with `mur`, exactly like the reporter's.
fn murmur_tree() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    for d in [
        "mur-agent-gui",
        "mur-agent-launcher",
        "mur-agent-runtime",
        "mur-browser",
        "mur-channel",
        "mur-common",
    ] {
        std::fs::create_dir(t.path().join(d)).unwrap();
    }
    t
}

fn app_in(t: &tempfile::TempDir) -> App {
    let mut app = App::test_fixture();
    app.cwd = Some(t.path().to_path_buf());
    app.path_bins = Some(vec!["mur".into(), "cargo".into()]);
    app
}

/// THE BUG: a finished shell command ending in a word that happens to prefix
/// some directory must leave the composer alone.
#[tokio::test]
async fn typing_a_complete_shell_command_does_not_open_the_path_menu() {
    let (tx, _rx) = mpsc::channel(16);
    let t = murmur_tree();
    let mut app = app_in(&t);

    type_str(&mut app, "!mur agent restart mur", &tx).await;

    assert_eq!(app.input_text(), "!mur agent restart mur");
    assert!(
        app.completion.is_none(),
        "the shell menu opened uninvited: {:?}",
        app.completion.as_ref().map(|c| c
            .items
            .iter()
            .map(|i| i.display.clone())
            .collect::<Vec<_>>())
    );
}

/// The consequence the reporter actually felt: Enter must send the command,
/// not accept `mur-agent-gui/` into the line.
#[tokio::test]
async fn enter_sends_the_command_instead_of_accepting_a_directory() {
    let (tx, _rx) = mpsc::channel(16);
    let t = murmur_tree();
    let mut app = app_in(&t);

    type_str(&mut app, "!mur agent restart mur", &tx).await;
    handle_event(&mut app, key(KeyCode::Enter), &tx).await;

    assert!(
        !app.input_text().contains("mur-agent-gui"),
        "Enter accepted a directory: {:?}",
        app.input_text()
    );
}

/// Tab is the explicit ask — completion still works, it just waits to be
/// invited.
#[tokio::test]
async fn tab_still_opens_the_path_menu_on_demand() {
    let (tx, _rx) = mpsc::channel(16);
    let t = murmur_tree();
    let mut app = app_in(&t);

    type_str(&mut app, "!ls mur-a", &tx).await;
    assert!(app.completion.is_none(), "still not uninvited");

    handle_event(&mut app, key(KeyCode::Tab), &tx).await;
    let c = app.completion.as_ref().expect("Tab opens the menu");
    let shown: Vec<String> = c.items.iter().map(|i| i.display.clone()).collect();
    assert_eq!(
        shown,
        [
            "mur-agent-gui/",
            "mur-agent-launcher/",
            "mur-agent-runtime/"
        ]
    );
}

/// The slash menu is murmur's own vocabulary and keeps opening as you type —
/// this fix must not touch it.
#[tokio::test]
async fn the_slash_menu_still_opens_while_typing() {
    let (tx, _rx) = mpsc::channel(16);
    let t = murmur_tree();
    let mut app = app_in(&t);

    type_str(&mut app, "/sk", &tx).await;
    assert!(
        app.completion.is_some(),
        "the slash menu must still auto-open"
    );
}
