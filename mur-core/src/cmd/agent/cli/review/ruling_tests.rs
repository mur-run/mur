//! P3b-§7.1 / AC-P3b-25: the ruling prompt answered from the composer.
//! Seams: `handle_stream` (the worker's `Ruling`), `submit` (the typed
//! line) and `handle_event` (Esc, Tab).

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use super::ReviewSession;
use super::state::Awaiting;
use super::test_fixtures::{app_at, home, skill_candidate, system_lines, tx};
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

/// What the driver receives when `line` is typed at the ruling prompt and an
/// installed skill is named `skill`.
async fn ruling_reply_with_skill(skill: &str, line: &str) -> Result<String, TryRecvError> {
    let tmp = home();
    let mut app = app_at(tmp.path());
    app.skills = vec![skill_candidate(skill)];
    attach(&mut app);
    let rx = ruling(&mut app);
    type_line(&mut app, line).await;
    rx.try_recv()
}

/// `/rule` reaches the driver when a skill shares the name. The guard refuses
/// every MURMUR built-in, so this also fails first if MURMUR ever ships a
/// `/rule` command of its own.
#[tokio::test]
async fn slash_rule_reaches_the_ruling_prompt_even_with_a_rule_skill_installed() {
    let line = "/rule fix F2 it still leaks";
    assert_eq!(
        ruling_reply_with_skill("rule", line).await,
        Ok(line.to_string()),
        "something in MURMUR now claims /rule before the ruling prompt sees it"
    );
}

/// `/abandon` reaches the driver when a skill shares the name, and fails
/// first if MURMUR ever ships an `/abandon` command of its own.
#[tokio::test]
async fn slash_abandon_reaches_the_ruling_prompt_even_with_an_abandon_skill_installed() {
    assert_eq!(
        ruling_reply_with_skill("abandon", "/abandon").await,
        Ok("/abandon".to_string()),
        "something in MURMUR now claims /abandon before the ruling prompt sees it"
    );
}

/// Types `line` at an open ruling prompt and checks it did not answer it:
/// the driver saw nothing, the prompt still waits, `warning` was shown and
/// the line is back in the composer. The next line still answers.
async fn assert_refused_at_ruling(app: &mut App, line: &str, warning: &str) {
    let rx = ruling(app);

    type_line(app, line).await;

    assert_eq!(
        rx.try_recv(),
        Err(TryRecvError::Empty),
        "{line:?} reached the driver"
    );
    let awaiting = app.review.as_ref().and_then(|s| s.awaiting.as_ref());
    assert!(
        matches!(awaiting, Some(Awaiting::Ruling { .. })),
        "{line:?}"
    );
    let text = system_lines(app).join("\n");
    assert!(text.contains(warning), "{text}");
    assert_eq!(app.input_text(), line);

    type_line(app, "q").await;
    assert_eq!(rx.try_recv(), Ok("q".to_string()));
}

/// A MURMUR built-in at the ruling prompt is refused with a warning instead
/// of going to the driver as the ruling.
#[tokio::test]
async fn a_murmur_command_at_the_ruling_prompt_is_refused_not_an_answer() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    assert_refused_at_ruling(&mut app, "/review", "/review was not run").await;
}

/// An installed skill parses as `Unknown`, so it rides on the guard's own
/// skill check; at the ruling prompt it is refused like a built-in.
#[tokio::test]
async fn a_skill_command_at_the_ruling_prompt_is_refused_not_an_answer() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    app.skills = vec![skill_candidate("brainstorming")];
    attach(&mut app);
    let line = "/brainstorming the rollout";
    assert_refused_at_ruling(&mut app, line, "/brainstorming was not run").await;
}

/// A bare `/` at the ruling prompt gets the bare-slash warning instead of
/// going to the driver, as at the gate and at `Paused — continue?`.
#[tokio::test]
async fn a_bare_slash_at_the_ruling_prompt_warns_and_keeps_asking() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let hint = crate::cmd::agent::cli::app::BARE_SLASH_HINT;
    assert_refused_at_ruling(&mut app, "/", hint).await;
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
