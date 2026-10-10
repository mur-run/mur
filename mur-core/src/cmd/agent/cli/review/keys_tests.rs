//! P3b-§4.4, §6.1–§6.3, AC-P3b-14…21, 23a–c: Esc and Ctrl+D while a review
//! is attached. Seams: `handle_event` (a key), `handle_stream` (a worker
//! message), and the `keys` functions with a recording `CancelFn`.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::sync::{Arc, Barrier, Mutex, OnceLock};

use anyhow::anyhow;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use super::ReviewSession;
use super::keys::{
    CancelFn, TurnRef, footer_hint, on_abort_turn, on_request_quit, poll_pending_cancel,
};
use super::state::Awaiting;
use super::test_fixtures::{app_at, home, system_lines, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::events::handle_event;
use crate::cmd::agent::cli::stream::{HitlRequest, StreamMsg};
use crate::cmd::agent::cli::stream_handler::handle_stream;
use crate::cmd::fleet::review::constants::{
    REVIEW_CANCEL_UNSUPPORTED, REVIEW_DISCARDED_MARK, REVIEW_FOOTER_CLOSING,
    REVIEW_FOOTER_PAUSE_ARMED, REVIEW_REPLY_ALREADY_ARRIVED,
};
use crate::cmd::fleet::review::driver::SendAnswer;
use crate::cmd::fleet::review::ledger::Ledger;
use crate::cmd::fleet::review::loop_driver::LoopDriverStop;
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, Outcome, ReviewFlags};
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;
use crate::cmd::fleet::review::turn_cell::{TurnCell, TurnState};

const MAIN: &str = "alpha";
const TASK: &str = "TASK-7";

/// Attached, a live handle whose flags the test reads back.
fn attach(app: &mut App) -> ReviewFlags {
    let flags = ReviewFlags::default();
    app.review = Some(ReviewSession {
        name: "review-ab140001".into(),
        channel_id: "ch-1".into(),
        handle: Some(WorkerHandle {
            join: std::thread::spawn(|| {}),
            flags: flags.clone(),
            name: "review-ab140001".into(),
            members: [MAIN.into(), "beta".into()],
        }),
        esc: ReviewEsc::Detached,
        awaiting: None,
        closing: false,
        hint: Default::default(),
        turn: Default::default(),
        label: Default::default(),
    });
    flags
}

/// The worker's `TurnStarted`, through the real stream seam.
fn start_turn(app: &mut App, task_id: Option<&str>) -> TurnRef {
    let slot = Arc::new(OnceLock::new());
    if let Some(id) = task_id {
        slot.set(id.to_string()).unwrap();
    }
    let cell = Arc::new(TurnCell::default());
    let req = DriverReq::TurnStarted {
        member: MAIN.into(),
        task_id: slot.clone(),
        turn: cell.clone(),
    };
    handle_stream(app, StreamMsg::ReviewReq(req), &tx());
    TurnRef {
        member: MAIN.into(),
        cell,
        task_id: slot,
    }
}

fn end_turn(app: &mut App) {
    let req = DriverReq::TurnEnded {
        member: MAIN.into(),
    };
    handle_stream(app, StreamMsg::ReviewReq(req), &tx());
}

type Calls = Arc<Mutex<Vec<(String, String)>>>;

/// Records `(member, task_id)`; `seen` captures the cell state inside the call.
fn recording(
    cell: Option<Arc<TurnCell>>,
    seen: Arc<Mutex<Vec<TurnState>>>,
) -> (Box<CancelFn>, Calls) {
    let calls: Calls = Arc::default();
    let c = calls.clone();
    let f: Box<CancelFn> = Box::new(move |_: PathBuf, member: String, id: String| {
        if let Some(cell) = &cell {
            seen.lock().unwrap().push(cell.state());
        }
        c.lock().unwrap().push((member, id));
        Ok(())
    });
    (f, calls)
}

fn no_cancel() -> (Box<CancelFn>, Calls) {
    recording(None, Arc::default())
}

fn esc() -> Event {
    key(KeyCode::Esc, KeyModifiers::NONE)
}

fn ctrl_d() -> Event {
    key(KeyCode::Char('d'), KeyModifiers::CONTROL)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn session(app: &App) -> &ReviewSession {
    app.review.as_ref().expect("still attached")
}

fn ran() -> Outcome {
    Outcome::Ran(
        LoopDriverStop::Approve,
        Box::<Ledger>::default(),
        "ch-1".into(),
    )
}

/// AC-P3b-14: the HITL modal is innermost — Esc denies it and arms nothing.
#[tokio::test]
async fn esc_in_hitl_modal_denies_and_leaves_pause_flag() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    let flags = attach(&mut app);
    start_turn(&mut app, Some(TASK));
    app.hitl = Some(HitlRequest {
        hitl_id: "h1".into(),
        step_id: None,
        tool_name: "write_file".into(),
        tool_input: serde_json::json!({}),
        prompt: "approve?".into(),
        created_at: std::time::Instant::now(),
        declared_risk: None,
    });

    handle_event(&mut app, esc(), &tx()).await;

    assert!(app.hitl.is_none(), "Esc decided the gate");
    assert!(!flags.pause_requested.load(Ordering::Acquire));
    assert!(!session(&app).turn.pause_armed);
}

/// AC-P3b-15: Esc at the send prompt never answers it.
#[tokio::test]
async fn esc_once_at_awaiting_confirm_does_not_reply() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let (reply, rx): (_, Receiver<SendAnswer>) = sync_channel(1);
    let s = app.review.as_mut().unwrap();
    s.awaiting = Some(Awaiting::Confirm {
        open: Default::default(),
        reply,
    });
    s.esc = ReviewEsc::AwaitingConfirm;
    app.set_input("NOTE-A");

    handle_event(&mut app, esc(), &tx()).await;

    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert!(session(&app).awaiting.is_some(), "still at the prompt");
}

/// AC-P3b-16 (UI half): Esc×1 mid-turn sets the worker's flag and the footer.
#[tokio::test]
async fn esc_once_in_flight_requests_pause_after_turn() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    let flags = attach(&mut app);
    let turn = start_turn(&mut app, Some(TASK));

    handle_event(&mut app, esc(), &tx()).await;

    assert!(flags.pause_requested.load(Ordering::Acquire));
    assert_eq!(turn.cell.state(), TurnState::InFlight, "the turn runs on");
    assert_eq!(footer_hint(&app), Some(REVIEW_FOOTER_PAUSE_ARMED));
}

/// AC-P3b-17: the abort wins the cell, then cancels with the task id.
#[test]
fn esc_twice_in_flight_wins_cell_then_cancels_with_task_id() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let turn = start_turn(&mut app, Some(TASK));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (cancel, calls) = recording(Some(turn.cell.clone()), seen.clone());

    on_abort_turn(&mut app, &*cancel);

    assert_eq!(*calls.lock().unwrap(), vec![(MAIN.into(), TASK.into())]);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![TurnState::Aborted],
        "CAS before cancel"
    );
    assert!(system_lines(&app).contains(&REVIEW_DISCARDED_MARK));
}

/// AC-P3b-20a: the reply committed first → no cancel, one notice, Esc unchanged.
#[test]
fn esc_twice_after_commit_shows_already_arrived_no_cancel() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let turn = start_turn(&mut app, Some(TASK));
    assert!(turn.cell.commit());
    let (cancel, calls) = no_cancel();

    on_abort_turn(&mut app, &*cancel);

    assert!(calls.lock().unwrap().is_empty());
    assert!(system_lines(&app).contains(&REVIEW_REPLY_ALREADY_ARRIVED));
    assert!(!system_lines(&app).contains(&REVIEW_DISCARDED_MARK));
    assert_eq!(session(&app).esc, ReviewEsc::TurnInFlight);
}

/// AC-P3b-20a interleaving: the UI reads `TurnInFlight`, the worker commits,
/// the UI's CAS then loses; and the mirror order where the UI wins.
#[test]
fn esc_twice_interleaved_with_commit_exactly_one_side_wins() {
    for ui_first in [false, true] {
        let tmp = home();
        let mut app = app_at(tmp.path());
        attach(&mut app);
        let turn = start_turn(&mut app, Some(TASK));
        assert_eq!(session(&app).esc, ReviewEsc::TurnInFlight, "the UI's read");
        let (cancel, calls) = no_cancel();
        let gate = Arc::new(Barrier::new(2));
        let (g, cell) = (gate.clone(), turn.cell.clone());
        let worker = std::thread::spawn(move || {
            if ui_first {
                g.wait();
            }
            let won = cell.commit();
            if !ui_first {
                g.wait();
            }
            won
        });
        if !ui_first {
            gate.wait();
        }
        on_abort_turn(&mut app, &*cancel);
        if ui_first {
            gate.wait();
        }
        let worker_won = worker.join().unwrap();

        let cancelled = calls.lock().unwrap().len();
        assert_eq!(worker_won, !ui_first, "ui_first={ui_first}");
        assert_eq!(cancelled, usize::from(ui_first), "ui_first={ui_first}");
    }
}

/// AC-P3b-20: `TurnEnded` after a won abort disarms Esc but keeps the abort.
#[test]
fn turn_ended_after_won_abort_does_not_undo_it() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let turn = start_turn(&mut app, Some(TASK));
    let (cancel, calls) = no_cancel();
    on_abort_turn(&mut app, &*cancel);

    end_turn(&mut app);

    assert_eq!(session(&app).esc, ReviewEsc::Detached);
    assert!(session(&app).turn.current.is_none());
    assert_eq!(turn.cell.state(), TurnState::Aborted);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(system_lines(&app).contains(&REVIEW_DISCARDED_MARK));
}

/// AC-P3b-23a: Ctrl+D mid-turn defers the quit until `Finished`.
#[tokio::test]
async fn request_quit_while_attached_defers_quit() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    let flags = attach(&mut app);
    start_turn(&mut app, Some(TASK));

    handle_event(&mut app, ctrl_d(), &tx()).await;

    assert!(!app.should_quit);
    assert!(session(&app).closing);
    assert!(flags.detach_requested.load(Ordering::Acquire));
    assert_eq!(footer_hint(&app), Some(REVIEW_FOOTER_CLOSING));

    handle_stream(&mut app, StreamMsg::ReviewFinished(ran()), &tx());

    assert!(app.should_quit, "Finished quits a closing session");
    assert!(app.review.is_none());
}

/// Control: `Finished` without closing leaves MURMUR open.
#[test]
fn finished_without_closing_does_not_quit() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);

    handle_stream(&mut app, StreamMsg::ReviewFinished(ran()), &tx());

    assert!(!app.should_quit);
}

/// §4.4.1: closing at a prompt drops its reply, which the worker reads as Stop.
#[test]
fn request_quit_at_awaiting_confirm_answers_stop() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let (reply, rx): (_, Receiver<SendAnswer>) = sync_channel(1);
    app.review.as_mut().unwrap().awaiting = Some(Awaiting::Confirm {
        open: Default::default(),
        reply,
    });

    assert!(!on_request_quit(&mut app));

    assert_eq!(
        rx.try_recv(),
        Err(TryRecvError::Disconnected),
        "reply dropped = Stop"
    );
}

/// AC-P3b-23b: closing does not change Esc×2.
#[tokio::test]
async fn esc_twice_while_closing_still_aborts() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    // No task id yet, so the real cancel is armed, not spawned.
    let turn = start_turn(&mut app, None);
    assert!(!on_request_quit(&mut app));

    handle_event(&mut app, esc(), &tx()).await;
    handle_event(&mut app, esc(), &tx()).await;

    assert_eq!(turn.cell.state(), TurnState::Aborted);
    assert!(session(&app).turn.pending_cancel.is_some(), "cancel armed");
    assert!(system_lines(&app).contains(&REVIEW_DISCARDED_MARK));
}

/// AC-P3b-23c: a second Ctrl+D while closing quits at once.
#[tokio::test]
async fn second_ctrl_d_while_closing_quits_now() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    start_turn(&mut app, Some(TASK));
    handle_event(&mut app, ctrl_d(), &tx()).await;
    assert!(!app.should_quit);

    handle_event(&mut app, ctrl_d(), &tx()).await;

    assert!(app.should_quit);
}

/// AC-P3b-19: the id arrives after the abort; the cancel fires then, once.
#[test]
fn esc_twice_before_task_id_fires_cancel_when_id_arrives() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let turn = start_turn(&mut app, None);
    let (cancel, calls) = no_cancel();

    on_abort_turn(&mut app, &*cancel);
    assert!(calls.lock().unwrap().is_empty());
    poll_pending_cancel(&mut app, &*cancel);
    assert!(calls.lock().unwrap().is_empty(), "still no id");

    turn.task_id.set(TASK.into()).unwrap();
    poll_pending_cancel(&mut app, &*cancel);
    poll_pending_cancel(&mut app, &*cancel);

    assert_eq!(*calls.lock().unwrap(), vec![(MAIN.into(), TASK.into())]);
}

/// AC-P3b-21: a failed cancel keeps the abort and says so in the footer.
#[test]
fn cancel_error_sets_unsupported_footer_text() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    let turn = start_turn(&mut app, Some(TASK));
    let failing: Box<CancelFn> = Box::new(|_, _, _| Err(anyhow!("CANCEL-NOPE")));

    on_abort_turn(&mut app, &*failing);

    assert_eq!(turn.cell.state(), TurnState::Aborted);
    assert!(session(&app).turn.cancel_unsupported);
    assert_eq!(footer_hint(&app), Some(REVIEW_CANCEL_UNSUPPORTED));
}

/// AC-P3b-21, production path: the async cancel's failure lands as a message.
#[test]
fn async_cancel_failure_sets_unsupported() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);

    handle_stream(
        &mut app,
        StreamMsg::ReviewCancelFailed("CANCEL-NOPE".into()),
        &tx(),
    );

    assert_eq!(footer_hint(&app), Some(REVIEW_CANCEL_UNSUPPORTED));
}

/// P3b-§6.3 step 3: unlike `CancelAndRestore`, `last_sent` stays out.
#[tokio::test]
async fn last_sent_not_restored_on_abort() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    attach(&mut app);
    start_turn(&mut app, None);
    app.last_sent = Some("OLD-MSG".into());

    handle_event(&mut app, esc(), &tx()).await;
    handle_event(&mut app, esc(), &tx()).await;

    assert_eq!(app.input_text(), "");
    assert!(system_lines(&app).contains(&REVIEW_DISCARDED_MARK));
}
