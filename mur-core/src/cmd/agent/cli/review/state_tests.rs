//! Spec §3.4 / AC-P3b-4, 4a: what `/review` does while a session is attached
//! or closing. Seams: `handle` (the typed line) and `handle_event` (a key).

use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use super::test_fixtures::{app_at, home, system_lines, tx};
use super::{ReviewSession, handle};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::events::handle_event;
use crate::cmd::fleet::review::constants::REVIEW_ALREADY_ATTACHED;
use crate::cmd::fleet::review::schema::{
    Cumulative, MICROS_PER_USD, ReviewPayload, Role as Member, VerdictKind,
};
use crate::cmd::fleet::review::state::state_tests::{paused, session};

const ATTACHED: &str = "review-ab120001";
const OTHER_PAUSED: &str = "review-cd340002";

/// An attached session whose channel shows one send to `main` and $1.50 spent.
fn attach(app: &mut App, home: &Path, esc: ReviewEsc) {
    let sent = ReviewPayload::TurnSent {
        round: 1,
        to: Member::Main,
        restart_note: None,
        human_wait_ms: 0,
    };
    let spent = ReviewPayload::Verdict {
        round: 1,
        kind: VerdictKind::Revise,
        cumulative: Cumulative {
            exec_time_ms: 1_000,
            cost_usd_micros: (1.5 * MICROS_PER_USD) as u64,
        },
    };
    let channel_id = session(home, ATTACHED, &[sent, spent], true);
    app.review = Some(ReviewSession {
        name: ATTACHED.into(),
        channel_id,
        handle: None,
        esc,
        awaiting: None,
        closing: false,
        hint: Default::default(),
    });
}

fn key(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// AC-P3b-4: a second start is refused, names the attached session, starts nothing.
#[tokio::test]
async fn attached_start_is_refused_naming_the_session() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), ReviewEsc::TurnInFlight);

    handle(&mut app, "--main a --reviewer b do it", &tx()).await;

    let want = REVIEW_ALREADY_ATTACHED.replace("{session}", ATTACHED);
    assert_eq!(system_lines(&app), [want.as_str()]);
    let kept = app.review.as_ref().expect("session stays attached");
    assert_eq!(kept.name, ATTACHED);
    assert!(kept.handle.is_none(), "nothing was spawned");
}

/// AC-P3b-4: `resume` is refused the same way.
#[tokio::test]
async fn attached_resume_is_refused_naming_the_session() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), ReviewEsc::AwaitingConfirm);

    handle(&mut app, "resume review-cd340002", &tx()).await;

    let want = REVIEW_ALREADY_ATTACHED.replace("{session}", ATTACHED);
    assert_eq!(system_lines(&app), [want.as_str()]);
}

/// AC-P3b-4: bare `/review` while attached re-prints the status line and no list.
#[tokio::test]
async fn attached_bare_prints_the_status_line_and_no_list() {
    let tmp = home();
    session(tmp.path(), OTHER_PAUSED, &[paused()], true);
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), ReviewEsc::TurnInFlight);

    handle(&mut app, "", &tx()).await;

    let lines = system_lines(&app);
    assert_eq!(lines.len(), 1, "one status line, got {lines:?}");
    assert!(lines[0].contains(ATTACHED), "{}", lines[0]);
    assert!(lines[0].contains("→main"), "{}", lines[0]);
    assert!(lines[0].contains("≥ $1.50"), "{}", lines[0]);
    assert!(!lines[0].contains(OTHER_PAUSED), "no list: {}", lines[0]);
}

/// AC-P3b-4a: while the worker finishes its turn the input is closed.
#[tokio::test]
async fn closing_drops_a_slash_keypress() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), ReviewEsc::TurnInFlight);
    app.review.as_mut().unwrap().closing = true;

    handle_event(&mut app, key('/'), &tx()).await;

    assert_eq!(app.input_text(), "");
}

/// Control: the same key reaches the composer when nothing is closing.
#[tokio::test]
async fn open_input_takes_a_slash_keypress() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), ReviewEsc::TurnInFlight);

    handle_event(&mut app, key('/'), &tx()).await;

    assert_eq!(app.input_text(), "/");
}
