//! P3b-§7.1 / AC-P3b-25: the ruling prompt answered from the composer.
//! Seams: `handle_stream` (the worker's `Ruling`), `submit` (the typed
//! line) and `handle_event` (Esc, Tab).

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use super::ReviewSession;
use super::test_fixtures::{app_at, home, system_lines, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::events::handle_event;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::agent::cli::stream_handler::handle_stream;
use crate::cmd::agent::cli::turn::submit;
use crate::cmd::fleet::review::constants::RULING_PROMPT_HINT;
use crate::cmd::fleet::review::murmur::bridge::DriverReq;
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;
use crate::cmd::fleet::review::ruling::{PromptLine, classify_ruling_line};

const TEXT: &str = "RULING-TXT";

fn attach(app: &mut App) {
    app.review = Some(ReviewSession {
        name: "review-ab250001".into(),
        channel_id: "ch-1".into(),
        handle: Some(WorkerHandle {
            join: std::thread::spawn(|| {}),
            flags: Default::default(),
            name: "review-ab250001".into(),
            members: ["alpha".into(), "beta".into()],
        }),
        esc: ReviewEsc::Detached,
        awaiting: None,
        closing: false,
        hint: Default::default(),
        turn: Default::default(),
        label: Default::default(),
    });
}

/// The worker asks for a ruling with F1 and F2 open; returns the reply end.
fn ruling(app: &mut App) -> Receiver<String> {
    let (reply, rx) = sync_channel(1);
    let req = DriverReq::Ruling {
        text: TEXT.into(),
        open: BTreeSet::from(["F1".to_string(), "F2".to_string()]),
        reply,
    };
    handle_stream(app, StreamMsg::ReviewReq(req), &tx());
    rx
}

async fn type_line(app: &mut App, line: &str) {
    app.set_input(line);
    submit(app, &tx()).await;
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// §7.1: the text is shown and the typed line goes back unchanged; `/stop`
/// is not special here, it is a line like any other.
#[tokio::test]
async fn ruling_req_shows_text_and_replies_verbatim() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);

    let rx = ruling(&mut app);
    assert!(system_lines(&app).iter().any(|l| l.contains(TEXT)));
    type_line(&mut app, "/rule drop F1 because").await;
    assert_eq!(rx.try_recv().unwrap(), "/rule drop F1 because");

    let rx = ruling(&mut app);
    type_line(&mut app, "/stop").await;
    assert_eq!(rx.try_recv().unwrap(), "/stop");
    assert!(app.review.is_some(), "/stop did not end the session here");
}

/// The empty string is EOF to the driver (leave paused). A bare Enter is not
/// EOF on stdin (it reads `"\n"`, which earns the hint), so it must not be
/// here either: the driver's own parser decides, as on stdin.
#[tokio::test]
async fn ruling_bare_enter_is_not_eof() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let rx = ruling(&mut app);

    type_line(&mut app, "").await;

    let got = rx.try_recv().expect("Enter answers the prompt");
    let open = BTreeSet::from(["F1".to_string(), "F2".to_string()]);
    assert_eq!(
        classify_ruling_line(&got, &open),
        PromptLine::Other(RULING_PROMPT_HINT.to_string())
    );
}

/// AC-P3b-25: Esc ×2 answers the empty string, which the driver reads as EOF.
#[tokio::test]
async fn ruling_esc_twice_replies_empty_string() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let rx = ruling(&mut app);

    handle_event(&mut app, key(KeyCode::Esc), &tx()).await;
    assert_eq!(
        rx.try_recv(),
        Err(TryRecvError::Empty),
        "Esc ×1 answers nothing"
    );
    handle_event(&mut app, key(KeyCode::Esc), &tx()).await;

    assert_eq!(rx.try_recv().unwrap(), "");
}

/// §7.1: Tab after `/rule drop ` offers the open finding IDs from the request.
#[tokio::test]
async fn ruling_tab_completes_open_ids() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let _rx = ruling(&mut app);

    app.set_input("/rule drop ");
    handle_event(&mut app, key(KeyCode::Tab), &tx()).await;

    let menu = app.completion.as_ref().expect("a menu of open findings");
    let shown: Vec<&str> = menu.items.iter().map(|c| c.display.as_str()).collect();
    assert_eq!(shown, ["F1", "F2"]);
    assert_eq!(menu.items[0].insert, "/rule drop F1 ");

    app.completion = None; // as Esc would: an open menu takes Tab as accept
    app.set_input("/rule fix F");
    handle_event(&mut app, key(KeyCode::Tab), &tx()).await;
    let menu = app.completion.as_ref().expect("fix takes an ID too");
    assert_eq!(menu.items[1].insert, "/rule fix F2 ");

    app.completion = None;
    app.set_input("/rule ");
    handle_event(&mut app, key(KeyCode::Tab), &tx()).await;
    assert!(app.completion.is_none(), "no IDs before the decision word");
}
