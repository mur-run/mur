//! Spec §5 / §7.2, AC-P3b-8…11: the send gate answered from the composer,
//! transcript `Show`s, and `Finished`. Seams: `handle_stream` (a worker
//! message) and `submit` (the line typed at the gate).

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use super::ReviewSession;
use super::state::Awaiting;
use super::test_fixtures::{app_at, home, system_lines, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::agent::cli::stream_handler::handle_stream;
use crate::cmd::agent::cli::turn::submit;
use crate::cmd::fleet::review::constants::REVIEW_LEFT_PAUSED_NOTICE;
use crate::cmd::fleet::review::driver::SendAnswer;
use crate::cmd::fleet::review::ledger::Ledger;
use crate::cmd::fleet::review::loop_driver::LoopDriverStop;
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, Outcome};
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;
use crate::cmd::fleet::review::schema::HumanNote;
use crate::cmd::fleet::review::session::render_stop_screen;
use crate::cmd::fleet::review::wire::text_message_params;

const MAIN: &str = "alpha";
const REVIEWER: &str = "beta";
const MESSAGE: &str = "PLEASE-REVIEW-F1";

/// A session with a live worker handle (a thread that has already ended).
fn attach(app: &mut App) {
    app.review = Some(ReviewSession {
        name: "review-ab120001".into(),
        channel_id: "ch-1".into(),
        handle: Some(WorkerHandle {
            join: std::thread::spawn(|| {}),
            flags: Default::default(),
            name: "review-ab120001".into(),
            members: [MAIN.into(), REVIEWER.into()],
        }),
        esc: ReviewEsc::Detached,
        awaiting: None,
        closing: false,
        hint: Default::default(),
    });
}

/// The worker asks to send `MESSAGE` to `MAIN`; returns the reply end.
fn confirm(app: &mut App) -> Receiver<SendAnswer> {
    let (reply, rx) = sync_channel(1);
    let req = DriverReq::Confirm {
        member: MAIN.into(),
        params: text_message_params(MESSAGE),
        open: BTreeSet::from(["F1".to_string()]),
        reply,
    };
    handle_stream(app, StreamMsg::ReviewReq(req), &tx());
    rx
}

async fn type_line(app: &mut App, line: &str) {
    app.set_input(line);
    submit(app, &tx()).await;
}

fn session(app: &App) -> &ReviewSession {
    app.review.as_ref().expect("still attached")
}

/// AC-P3b-8: preview under the stdin header; the input is not prefilled.
#[tokio::test]
async fn confirm_previews_the_message_and_leaves_the_input_empty() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let _rx = confirm(&mut app);
    let header = format!("--- next message to {MAIN} ---\n{MESSAGE}");
    assert!(
        system_lines(&app).contains(&header.as_str()),
        "{:?}",
        system_lines(&app)
    );
    assert_eq!(app.input_text(), "");
    assert_eq!(session(&app).esc, ReviewEsc::AwaitingConfirm);
    assert!(matches!(
        session(&app).awaiting,
        Some(Awaiting::Confirm { .. })
    ));
}

/// AC-P3b-10: Enter, `y`, `yes` are consent.
#[tokio::test]
async fn enter_y_and_yes_send() {
    for line in ["", "y", "yes"] {
        let tmp = home();
        let mut app = app_at(tmp.path());
        attach(&mut app);
        let rx = confirm(&mut app);
        type_line(&mut app, line).await;
        assert_eq!(rx.try_recv(), Ok(SendAnswer::Send), "line {line:?}");
        assert!(session(&app).awaiting.is_none());
        assert_eq!(session(&app).esc, ReviewEsc::Detached);
    }
}

/// AC-P3b-11: `/stop` is the only stop.
#[tokio::test]
async fn slash_stop_stops() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let rx = confirm(&mut app);
    type_line(&mut app, "/stop").await;
    assert_eq!(rx.try_recv(), Ok(SendAnswer::Stop));
}

/// AC-P3b-9: bare `q` is a broadcast note, never Stop and never Send.
#[tokio::test]
async fn bare_q_is_a_broadcast_note() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let rx = confirm(&mut app);
    type_line(&mut app, "q").await;
    let note = HumanNote {
        text: "q".into(),
        target: None,
    };
    assert_eq!(rx.try_recv(), Ok(SendAnswer::Note(note)));
}

/// §5.2: an unknown `/<word>` is a hint and the gate asks again; the next
/// line still answers it.
#[tokio::test]
async fn unknown_command_hints_and_keeps_asking() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let rx = confirm(&mut app);
    type_line(&mut app, "/foo").await;
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert!(system_lines(&app).contains(&"unknown command: /foo"));
    assert!(matches!(
        session(&app).awaiting,
        Some(Awaiting::Confirm { .. })
    ));
    type_line(&mut app, "").await;
    assert_eq!(rx.try_recv(), Ok(SendAnswer::Send));
}

/// §4.1: `Show` is one transcript block, verbatim.
#[tokio::test]
async fn show_is_a_system_block() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let text = "--- reply from alpha ---\nDONE";
    handle_stream(
        &mut app,
        StreamMsg::ReviewReq(DriverReq::Show(text.into())),
        &tx(),
    );
    assert_eq!(system_lines(&app).last(), Some(&text));
}

/// §7.2: each outcome renders as stdin does, the worker is joined, and the
/// session is released.
#[tokio::test]
async fn finished_renders_joins_and_detaches() {
    let ran = || Outcome::Ran(LoopDriverStop::Approve, Box::default(), "ch-1".into());
    let stop_screen = render_stop_screen(&LoopDriverStop::Approve, &Ledger::default(), "ch-1");
    let cases = [
        (ran(), stop_screen),
        (Outcome::LeftPaused, REVIEW_LEFT_PAUSED_NOTICE.to_string()),
        (Outcome::Err("boom".into()), "boom".to_string()),
    ];
    for (outcome, want) in cases {
        let tmp = home();
        let mut app = app_at(tmp.path());
        attach(&mut app);
        handle_stream(&mut app, StreamMsg::ReviewFinished(outcome), &tx());
        assert_eq!(system_lines(&app).last(), Some(&want.as_str()));
        assert!(app.review.is_none());
    }
}
