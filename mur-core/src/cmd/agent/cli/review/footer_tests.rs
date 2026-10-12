//! AC-P3b-23 / AC-P3b-4a: the status bar while a review is attached, read
//! off the rendered row (the seam a user sees).

use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::ReviewSession;
use super::test_fixtures::{app_at, home, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::agent::cli::stream_handler::handle_stream;
use crate::cmd::fleet::review::constants::{
    REVIEW_CANCEL_UNSUPPORTED, REVIEW_FOOTER_CLOSING, REVIEW_FOOTER_HINT, REVIEW_FOOTER_PAUSE_ARMED,
};
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, Outcome};
use crate::cmd::fleet::review::schema::{Cumulative, ReviewPayload, Role, VerdictKind};
use crate::cmd::fleet::review::state::state_tests::session;
use crate::cmd::fleet::review::turn_cell::TurnCell;

const NAME: &str = "review-ab230001";
const WIDTH: u16 = 220;

/// Attached over a real channel holding `payloads`; label facts not yet read.
fn attach(app: &mut App, home: &Path, payloads: &[ReviewPayload]) {
    let channel_id = session(home, NAME, payloads, true);
    app.review = Some(ReviewSession {
        name: NAME.into(),
        channel_id,
        handle: None,
        esc: ReviewEsc::Detached,
        awaiting: None,
        closing: false,
        hint: Default::default(),
        turn: Default::default(),
        label: Default::default(),
    });
}

fn footer(app: &App) -> String {
    let mut term = Terminal::new(TestBackend::new(WIDTH, 1)).unwrap();
    term.draw(|f| crate::cmd::agent::cli::ui::render_status_for_test(f, app, f.area()))
        .unwrap();
    term.backend().to_string()
}

fn spent(usd_micros: u64) -> ReviewPayload {
    ReviewPayload::Verdict {
        round: 1,
        kind: VerdictKind::Revise,
        cumulative: Cumulative {
            exec_time_ms: 1_000,
            cost_usd_micros: usd_micros,
        },
    }
}

fn sent() -> ReviewPayload {
    ReviewPayload::TurnSent {
        round: 1,
        to: Role::Main,
        restart_note: None,
        human_wait_ms: 0,
    }
}

/// AC-P3b-23: the whole attached session shows the review hint and label.
#[test]
fn attached_footer_shows_the_review_hint_and_label() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[]);

    let row = footer(&app);

    assert!(row.contains(REVIEW_FOOTER_HINT), "{row}");
    assert!(row.contains("review review-ab230001 · ≥ $0.00"), "{row}");
}

/// AC-P3b-23: once `Finished` releases the session, hint and label are gone.
#[test]
fn footer_hint_gone_after_finished() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[]);

    handle_stream(
        &mut app,
        StreamMsg::ReviewFinished(Outcome::LeftPaused),
        &tx(),
    );

    let row = footer(&app);
    assert!(!row.contains(REVIEW_FOOTER_HINT), "{row}");
    assert!(!row.contains(NAME), "{row}");
}

/// AC-P3b-4a: closing replaces the plain hint with the finishing text.
#[test]
fn closing_text_takes_precedence_over_the_plain_hint() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[]);
    app.review.as_mut().unwrap().closing = true;

    let row = footer(&app);

    assert!(row.contains(REVIEW_FOOTER_CLOSING), "{row}");
    assert!(!row.contains(REVIEW_FOOTER_HINT), "{row}");
}

/// P3b-§6.2 / AC-P3b-21: the armed-pause and cancel-unsupported texts reach
/// the row, cancel-unsupported first.
#[test]
fn pause_armed_and_cancel_unsupported_reach_the_row() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[]);
    app.review.as_mut().unwrap().turn.pause_armed = true;
    assert!(footer(&app).contains(REVIEW_FOOTER_PAUSE_ARMED));

    app.review.as_mut().unwrap().turn.cancel_unsupported = true;

    let row = footer(&app);
    assert!(row.contains(REVIEW_CANCEL_UNSUPPORTED), "{row}");
    assert!(!row.contains(REVIEW_FOOTER_PAUSE_ARMED), "{row}");
}

/// §6.5: the label is re-read at a turn boundary, never by the status bar.
/// The previous turn's usage is on the channel by the next `TurnStarted`.
#[test]
fn label_refreshes_at_turn_started() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[sent(), spent(1_500_000)]);
    assert!(
        footer(&app).contains("≥ $0.00"),
        "not read before a boundary"
    );

    let req = DriverReq::TurnStarted {
        member: "main".into(),
        task_id: Default::default(),
        turn: std::sync::Arc::new(TurnCell::default()),
    };
    handle_stream(&mut app, StreamMsg::ReviewReq(req), &tx());

    let row = footer(&app);
    assert!(row.contains("≥ $1.50"), "{row}");
    assert!(row.contains("review review-ab230001 · "), "{row}");
}
