//! AC-P3b-23 / AC-P3b-4a: the status bar while a review is attached, read
//! off the rendered row (the seam a user sees).

use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::ReviewSession;
use super::state::Awaiting;
use super::test_fixtures::{app_at, home, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::agent::cli::stream_handler::handle_stream;
use crate::cmd::fleet::review::constants::{
    MURMUR_STOP_COMMAND, REVIEW_CANCEL_UNSUPPORTED, REVIEW_FOOTER_CLOSING, REVIEW_FOOTER_GATE,
    REVIEW_FOOTER_HINT, REVIEW_FOOTER_PAUSE_ARMED, REVIEW_FOOTER_RULING,
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

/// #1787: at the send gate no turn runs and Esc does nothing, so the footer
/// must not offer the Esc turn hint; it names the gate's own keys instead.
/// A pause armed earlier in the session does not hide the gate hint.
#[test]
fn send_gate_footer_shows_the_gate_hint_not_the_esc_turn_hint() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[]);
    let s = app.review.as_mut().unwrap();
    s.turn.pause_armed = true;
    s.awaiting = Some(Awaiting::Confirm {
        open: Default::default(),
        reply: std::sync::mpsc::sync_channel(1).0,
    });

    let row = footer(&app);

    assert!(!row.contains(REVIEW_FOOTER_HINT), "{row}");
    assert!(!row.contains(REVIEW_FOOTER_PAUSE_ARMED), "{row}");
    assert!(row.contains(REVIEW_FOOTER_GATE), "{row}");
    assert!(REVIEW_FOOTER_GATE.contains(MURMUR_STOP_COMMAND));
}

/// #1787: the ruling prompt is an answer prompt too, not a running turn.
#[test]
fn ruling_footer_shows_the_answer_hint_not_the_esc_turn_hint() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[]);
    app.review.as_mut().unwrap().awaiting = Some(Awaiting::Ruling {
        open: Default::default(),
        reply: std::sync::mpsc::sync_channel(1).0,
    });

    let row = footer(&app);

    assert!(!row.contains(REVIEW_FOOTER_HINT), "{row}");
    assert!(row.contains(REVIEW_FOOTER_RULING), "{row}");
}

/// The row as a crowded live session draws it: a numbered channel chip, a
/// long-running label and a full usage readout on the left, `width` columns.
fn crowded_footer(app: &mut App, width: u16) -> String {
    app.channel = Some(crate::cmd::agent::cli::persist::ChannelMeta {
        id: "01a15531-c974-7ba0-985f-68a6b1028be2".into(),
        ordinal: 12,
    });
    app.session_tool_allow.insert("bash".into());
    app.monitor_total = 2;
    app.turn_in = 48_210;
    app.turn_out = 3_904;
    app.session_in = 1_204_118;
    app.session_out = 88_431;
    app.ctx_tokens = 61_000;
    app.pricing = crate::cmd::agent::cli::footer::Pricing {
        in_per_1k: Some(0.003),
        out_per_1k: Some(0.015),
        window: Some(200_000),
    };
    let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
    term.draw(|f| crate::cmd::agent::cli::ui::render_status_for_test(f, app, f.area()))
        .unwrap();
    term.backend().to_string()
}

/// The MURMUR send gate prints no prompt line of its own, so the footer hint
/// is the only place its keys are named. A full usage readout used to take
/// the row first and the hint was dropped, leaving the gate looking idle
/// (`ready`) with nothing saying Enter sends. Usage gives way to it instead.
#[test]
fn send_gate_hint_survives_a_crowded_status_bar() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[sent()]);
    app.review.as_mut().unwrap().awaiting = Some(Awaiting::Confirm {
        open: Default::default(),
        reply: std::sync::mpsc::sync_channel(1).0,
    });

    let row = crowded_footer(&mut app, 140);

    assert!(row.contains(REVIEW_FOOTER_GATE), "{row}");
    assert!(row.contains("review review-ab230001"), "{row}");
}

/// Same for the ruling prompt: its keys live only in the footer too.
#[test]
fn ruling_hint_survives_a_crowded_status_bar() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[sent()]);
    app.review.as_mut().unwrap().awaiting = Some(Awaiting::Ruling {
        open: Default::default(),
        reply: std::sync::mpsc::sync_channel(1).0,
    });

    let row = crowded_footer(&mut app, 140);

    assert!(row.contains(REVIEW_FOOTER_RULING), "{row}");
}

/// Advice still yields to state: with a turn running, the Esc hint is the
/// first thing dropped and the usage readout keeps its room.
#[test]
fn turn_hint_still_yields_to_usage_when_crowded() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app, tmp.path(), &[sent()]);

    let row = crowded_footer(&mut app, 140);

    assert!(row.contains("ctx "), "{row}");
    assert!(!row.contains(REVIEW_FOOTER_HINT), "{row}");
}
