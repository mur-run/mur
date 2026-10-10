//! AC-P3b-13: the inline `@<unknown>` hint is recomputed only when the input
//! changes (counting resolver), and is shown only while the send gate is open.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::mpsc::sync_channel;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::ReviewSession;
use super::hint::{InlineHint, current_hint};
use super::state::Awaiting;
use super::test_fixtures::{app_at, home, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::events::handle_event;
use crate::cmd::agent::cli::turn::submit;
use crate::cmd::fleet::review::constants::TARGET_UNKNOWN_INLINE_HINT;
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;

const MAIN: &str = "alpha";
const REVIEWER: &str = "beta";

fn members() -> [String; 2] {
    [MAIN.into(), REVIEWER.into()]
}

fn ghost_hint() -> String {
    TARGET_UNKNOWN_INLINE_HINT.replace("{name}", "ghost")
}

#[test]
fn same_text_resolves_once() {
    let calls = Cell::new(0);
    let counting = |s: &str| {
        calls.set(calls.get() + 1);
        s.to_string()
    };
    let mut h = InlineHint::default();
    for _ in 0..5 {
        h.refresh("@ghost fix it", &members(), counting);
    }
    assert_eq!(calls.get(), 1, "five frames, one resolve");
    assert_eq!(h.for_text("@ghost fix it"), Some(ghost_hint().as_str()));
}

#[test]
fn a_changed_text_resolves_again() {
    let calls = Cell::new(0);
    let counting = |s: &str| {
        calls.set(calls.get() + 1);
        s.to_string()
    };
    let mut h = InlineHint::default();
    h.refresh("@ghost fix it", &members(), counting);
    h.refresh("@ghost fix it!", &members(), counting);
    h.refresh("@ghost fix it!", &members(), counting);
    assert_eq!(calls.get(), 2);
    h.refresh(&format!("@{MAIN} fix it"), &members(), counting);
    assert_eq!(calls.get(), 3);
    assert_eq!(
        h.for_text(&format!("@{MAIN} fix it")),
        None,
        "a member has no hint"
    );
}

/// A cache entry never answers for a different text.
#[test]
fn a_stale_entry_is_not_shown() {
    let mut h = InlineHint::default();
    h.refresh("@ghost fix it", &members(), |s: &str| s.to_string());
    assert_eq!(h.for_text("@ghost fix i"), None);
    assert_eq!(h.for_text(""), None);
}

fn attach(app: &mut App, gate: bool) {
    let awaiting = gate.then(|| Awaiting::Confirm {
        open: BTreeSet::new(),
        reply: sync_channel(1).0,
    });
    app.review = Some(ReviewSession {
        name: "review-ab130001".into(),
        channel_id: "ch-1".into(),
        handle: Some(WorkerHandle {
            join: std::thread::spawn(|| {}),
            flags: Default::default(),
            name: "review-ab130001".into(),
            members: members(),
        }),
        esc: ReviewEsc::AwaitingConfirm,
        awaiting,
        closing: false,
        hint: InlineHint::default(),
    });
}

async fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        let ev = Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        handle_event(app, ev, &tx()).await;
    }
}

fn footer(app: &App) -> String {
    let mut term = Terminal::new(TestBackend::new(160, 1)).unwrap();
    term.draw(|f| crate::cmd::agent::cli::ui::render_status_for_test(f, app, f.area()))
        .unwrap();
    term.backend().to_string()
}

/// Typing at the open gate shows the hint in the footer; the hint goes once
/// the line is submitted (the gate's answer is a broadcast note).
#[tokio::test]
async fn typing_at_the_gate_shows_the_hint_and_submit_clears_it() {
    let home = home();
    let mut app = app_at(home.path());
    attach(&mut app, true);
    type_text(&mut app, "@ghost fix it").await;
    assert_eq!(current_hint(&app), Some(ghost_hint()));
    assert!(
        footer(&app).contains(&ghost_hint()),
        "footer: {}",
        footer(&app)
    );

    submit(&mut app, &tx()).await;
    assert_eq!(current_hint(&app), None);
}

#[tokio::test]
async fn no_hint_without_the_send_gate() {
    let home = home();
    let mut app = app_at(home.path());
    type_text(&mut app, "@ghost fix it").await;
    assert_eq!(current_hint(&app), None, "no review attached");

    attach(&mut app, false);
    app.clear_input();
    type_text(&mut app, "@ghost fix it").await;
    assert_eq!(current_hint(&app), None, "attached, gate closed");
}
