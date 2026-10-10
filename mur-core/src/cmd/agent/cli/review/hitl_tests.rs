//! P3b-§10 / AC-P3b-31: a review member's tool approval through the modal.
//! Seams: `handle_stream` (the worker's `Hitl`), `handle_event` (the keys),
//! `expire_stale_hitl` (the timeout) and the rendered modal.

use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::ReviewSession;
use super::test_fixtures::{app_at, home, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::events::handle_event;
use crate::cmd::agent::cli::hitl::expire_stale_hitl;
use crate::cmd::agent::cli::stream::{HitlRequest, StreamMsg};
use crate::cmd::agent::cli::stream_handler::handle_stream;
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, HitlOrigin};
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;

fn attach(app: &mut App) {
    app.review = Some(ReviewSession {
        name: "review-ab310001".into(),
        channel_id: "ch-1".into(),
        handle: Some(WorkerHandle {
            join: std::thread::spawn(|| {}),
            flags: Default::default(),
            name: "review-ab310001".into(),
            members: ["alpha".into(), "beta".into()],
        }),
        esc: ReviewEsc::TurnInFlight,
        awaiting: None,
        closing: false,
        hint: Default::default(),
        turn: Default::default(),
        label: Default::default(),
    });
}

fn req(id: &str, cmd: &str) -> HitlRequest {
    HitlRequest {
        hitl_id: id.into(),
        step_id: None,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "command": cmd }),
        prompt: "Run `bash`?".into(),
        created_at: std::time::Instant::now(),
        declared_risk: None,
    }
}

/// The worker raises one gated call for `alpha`; returns the reply end.
fn gate(app: &mut App, r: HitlRequest) -> Receiver<bool> {
    let (reply, rx) = sync_channel(1);
    let msg = StreamMsg::ReviewReq(DriverReq::Hitl {
        member: "alpha".into(),
        req: r,
        reply,
    });
    handle_stream(app, msg, &tx());
    rx
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn attached_app() -> (tempfile::TempDir, App) {
    let h = home();
    let mut app = app_at(h.path());
    attach(&mut app);
    (h, app)
}

/// The modal as a user sees it.
fn modal_text(app: &mut App) -> String {
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::cmd::agent::cli::ui::render(f, app))
        .unwrap();
    let buf = term.backend().buffer().clone();
    let w = buf.area.width as usize;
    buf.content
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn review_hitl_shows_once_deny_only() {
    let (_h, mut app) = attached_app();
    // `App::new` turns `/auto` on; start from off so a widening would show.
    app.auto_approve = false;
    let rx = gate(&mut app, req("H-1", "ls"));
    assert_eq!(app.hitl.as_ref().map(|r| r.hitl_id.as_str()), Some("H-1"));
    assert_eq!(app.hitl_origin, HitlOrigin::Review);
    let screen = modal_text(&mut app);
    assert!(screen.contains("1. Yes"), "{screen}");
    assert!(
        screen.contains("2. No"),
        "Deny is row 2 for a review gate: {screen}"
    );
    assert!(
        !screen.contains("stop asking"),
        "no session grants: {screen}"
    );
    assert!(!screen.contains("3."), "two rows only: {screen}");
    assert!(
        screen.contains("review member alpha"),
        "the title names whose call it is: {screen}"
    );
    assert!(
        !screen.contains("tell MUR"),
        "a member's deny carries no composer text: {screen}"
    );
    // The grant keys of the own-turn menu do nothing here.
    for c in ['a', 't', '3', '4'] {
        handle_event(&mut app, key(KeyCode::Char(c)), &tx()).await;
        assert!(app.hitl.is_some(), "`{c}` must not decide a review gate");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty), "`{c}`");
    }
    // ↓ stops at Deny: there is no third row to land on.
    for _ in 0..5 {
        handle_event(&mut app, key(KeyCode::Down), &tx()).await;
    }
    assert_eq!(app.hitl_selected, 1);
    // Typed text from the `a`/`t` presses rides in the composer; clear it so
    // `2` is a shortcut, not a character.
    app.clear_input();
    handle_event(&mut app, key(KeyCode::Char('2')), &tx()).await;
    assert_eq!(rx.try_recv(), Ok(false), "row 2 denies");
    assert!(app.hitl.is_none());
    assert!(!app.auto_approve, "nothing widened the session");
    assert!(app.session_tool_allow.is_empty());
}

#[tokio::test]
async fn review_hitl_enter_on_once_allows_and_esc_denies() {
    let (_h, mut app) = attached_app();
    let rx = gate(&mut app, req("H-1", "ls"));
    handle_event(&mut app, key(KeyCode::Enter), &tx()).await;
    assert_eq!(rx.try_recv(), Ok(true), "Enter on `Yes` allows this call");
    let receipt = super::test_fixtures::system_lines(&app).join("\n");
    let last = app
        .messages
        .last()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        last.starts_with("alpha: approved"),
        "receipt names the member: {last} / {receipt}"
    );
    assert_eq!(app.hitl_origin, HitlOrigin::Own, "the slot resets to Own");

    let rx = gate(&mut app, req("H-2", "ls"));
    handle_event(&mut app, key(KeyCode::Esc), &tx()).await;
    assert_eq!(rx.try_recv(), Ok(false), "Esc in the modal denies (§6.1)");
    assert!(app.hitl.is_none());
}

#[tokio::test]
async fn review_hitl_ignores_auto_approve() {
    let (_h, mut app) = attached_app();
    app.auto_approve = true;
    app.auto_reads = true;
    app.session_tool_allow.insert("bash".into());
    // `ls` is a read: every auto lane of the own-turn path would take it.
    let rx = gate(&mut app, req("H-1", "ls"));
    assert!(app.hitl.is_some(), "the review gate always asks");
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn review_hitl_ui_never_calls_respond_hitl() {
    // `respond_hitl` dials the ATTACHED agent; the review's `hitl_id` belongs
    // to a member, so a call would surface as a "failed to deliver" note on
    // the stream. The decision must arrive on `reply` and nowhere else.
    let (_h, mut app) = attached_app();
    let (stx, mut srx) = super::test_fixtures::stream();
    let (reply, rx) = sync_channel(1);
    let msg = StreamMsg::ReviewReq(DriverReq::Hitl {
        member: "alpha".into(),
        req: req("H-1", "rm -rf build"),
        reply,
    });
    handle_stream(&mut app, msg, &stx);
    handle_event(&mut app, key(KeyCode::Enter), &stx).await;
    assert_eq!(rx.try_recv(), Ok(true));
    // Let any spawned dial run to its failure, then look for its note.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(srx.try_recv().is_err(), "nothing dialled on the UI side");
}

#[tokio::test]
async fn review_hitl_timeout_denies() {
    let (_h, mut app) = attached_app();
    let mut r = req("H-1", "ls");
    r.created_at = std::time::Instant::now()
        .checked_sub(crate::hitl::gate::DEFAULT_TIMEOUT)
        .unwrap();
    let rx = gate(&mut app, r);
    assert!(expire_stale_hitl(&mut app), "the stale gate retires");
    assert_eq!(rx.try_recv(), Ok(false), "timeout → deny");
    assert_eq!(app.hitl_origin, HitlOrigin::Own);
}

#[tokio::test]
async fn review_hitl_queues_behind_an_open_gate_and_keeps_its_reply() {
    let (_h, mut app) = attached_app();
    let rx1 = gate(&mut app, req("H-1", "ls"));
    let rx2 = gate(&mut app, req("H-2", "pwd"));
    assert_eq!(app.hitl.as_ref().map(|r| r.hitl_id.as_str()), Some("H-1"));
    handle_event(&mut app, key(KeyCode::Esc), &tx()).await;
    assert_eq!(rx1.try_recv(), Ok(false));
    crate::cmd::agent::cli::hitl::promote_queued_hitl(&mut app, &tx());
    assert_eq!(app.hitl.as_ref().map(|r| r.hitl_id.as_str()), Some("H-2"));
    assert_eq!(app.hitl_origin, HitlOrigin::Review);
    handle_event(&mut app, key(KeyCode::Enter), &tx()).await;
    assert_eq!(rx2.try_recv(), Ok(true), "the second gate's own reply");
}

#[tokio::test]
async fn own_gate_menu_is_unchanged() {
    let (_h, mut app) = attached_app();
    app.hitl = Some(req("O-1", "ls"));
    assert_eq!(app.hitl_origin, HitlOrigin::Own);
    let screen = modal_text(&mut app);
    assert!(screen.contains("4. No"), "own gate keeps 4 rows: {screen}");
    assert!(screen.contains("stop asking"), "{screen}");
}

#[tokio::test]
async fn review_hitl_reply_dropped_when_the_review_ends() {
    // A gate still open when `Finished` arrives must not linger as a modal
    // the worker can no longer hear: the worker already took deny.
    let (_h, mut app) = attached_app();
    let rx = gate(&mut app, req("H-1", "ls"));
    super::on_finished(
        &mut app,
        crate::cmd::fleet::review::murmur::bridge::Outcome::Err("x".into()),
    );
    assert!(app.hitl.is_none(), "no orphan review modal");
    assert_eq!(app.hitl_origin, HitlOrigin::Own);
    assert!(matches!(
        rx.try_recv(),
        Ok(false) | Err(TryRecvError::Disconnected)
    ));
}

#[tokio::test]
async fn a_new_conversation_keeps_the_review_gate() {
    // `/new` resets the attached agent's conversation; the review and the
    // member waiting on this gate carry on.
    let (h, mut app) = attached_app();
    let rx = gate(&mut app, req("H-1", "ls"));
    let s = crate::cmd::agent::cli::persist::Session::create(h.path(), "a").unwrap();
    app.start_new_session(s);
    assert_eq!(app.hitl.as_ref().map(|r| r.hitl_id.as_str()), Some("H-1"));
    assert_eq!(app.hitl_origin, HitlOrigin::Review);
    handle_event(&mut app, key(KeyCode::Enter), &tx()).await;
    assert_eq!(rx.try_recv(), Ok(true));
}
