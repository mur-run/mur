//! Esc and Ctrl+D while a review is attached (P3b-§4.4, §6.2, §6.3).
//!
//! Esc×1 arms a pause after the current turn. Esc×2 races the worker for the
//! turn's [`TurnCell`]: a won abort cancels the task (now, or as soon as its
//! id arrives); a lost one only says the reply already arrived. Ctrl+D
//! detaches gracefully: the turn finishes, then the worker pauses.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use tokio::sync::mpsc::Sender;

use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::{StreamMsg, cancel_task};
use crate::cmd::fleet::review::constants::{
    REVIEW_CANCEL_UNSUPPORTED, REVIEW_DISCARDED_MARK, REVIEW_FOOTER_CLOSING,
    REVIEW_FOOTER_PAUSE_ARMED, REVIEW_FOOTER_WILL_PAUSE, REVIEW_REPLY_ALREADY_ARRIVED,
};
use crate::cmd::fleet::review::turn_cell::TurnCell;

/// `(mur_home, member, task_id)` → best-effort `tasks/cancel`. Injectable so
/// tests assert the id and the error path without a runtime.
pub type CancelFn = dyn Fn(PathBuf, String, String) -> Result<()> + Send + Sync;

/// One member turn as the UI sees it (from `DriverReq::TurnStarted`).
#[derive(Clone)]
pub struct TurnRef {
    pub member: String,
    pub cell: Arc<TurnCell>,
    pub task_id: Arc<OnceLock<String>>,
}

/// Esc state for the attached session.
#[derive(Default)]
pub struct LiveTurn {
    /// The turn in flight; `None` between turns.
    pub current: Option<TurnRef>,
    /// A won abort whose task id had not arrived yet (AC-P3b-19).
    pub pending_cancel: Option<TurnRef>,
    /// Esc×1 armed a pause after this turn (P3b-§6.2).
    pub pause_armed: bool,
    /// `tasks/cancel` failed (P3b-§6.3 step 5, AC-P3b-21).
    pub cancel_unsupported: bool,
}

/// Esc×1 during a turn: pause after it. The turn itself is untouched.
pub fn on_request_pause(app: &mut App) {
    let Some(s) = app.review.as_mut() else {
        return;
    };
    if let Some(h) = s.handle.as_ref() {
        h.flags.pause_requested.store(true, Ordering::Release);
    }
    if !s.turn.pause_armed {
        s.turn.pause_armed = true;
        app.push_system(REVIEW_FOOTER_WILL_PAUSE);
    }
}

/// Esc×2 during a turn. The CAS decides: lost → the reply already arrived,
/// nothing else happens; won → the reply is discarded and the task cancelled.
/// `last_sent` is never restored here (P3b-§6.3 step 3).
pub fn on_abort_turn(app: &mut App, cancel: &CancelFn) {
    let home = app.home.clone();
    let Some(s) = app.review.as_mut() else {
        return;
    };
    let Some(turn) = s.turn.current.clone() else {
        return;
    };
    if !turn.cell.abort() {
        app.push_system(REVIEW_REPLY_ALREADY_ARRIVED);
        return;
    }
    match turn.task_id.get() {
        Some(id) => fire(s, cancel, home, &turn.member, id),
        None => s.turn.pending_cancel = Some(turn),
    }
    app.push_system(REVIEW_DISCARDED_MARK);
}

/// Frame tick: a won abort whose task id has since arrived is cancelled once.
pub fn poll_pending_cancel(app: &mut App, cancel: &CancelFn) {
    let home = app.home.clone();
    let Some(s) = app.review.as_mut() else {
        return;
    };
    let Some(id) = s
        .turn
        .pending_cancel
        .as_ref()
        .and_then(|t| t.task_id.get().cloned())
    else {
        return;
    };
    if let Some(turn) = s.turn.pending_cancel.take() {
        fire(s, cancel, home, &turn.member, &id);
    }
}

/// Whether the frame loop must wake to poll a pending cancel.
pub fn cancel_pending(app: &App) -> bool {
    app.review
        .as_ref()
        .is_some_and(|s| s.turn.pending_cancel.is_some())
}

/// D9 / §4.4.1. `true` = quit now (no review, nothing running, or the
/// second Ctrl+D while closing). `false` = closing: the worker finishes its
/// turn and pauses as `detached` (AC-P3b-23a; D9's `pause_requested` would
/// write `user`); `Finished` sets `should_quit`. A pending prompt's reply is
/// dropped, which the worker also reads as `detached`.
pub fn on_request_quit(app: &mut App) -> bool {
    let Some(s) = app.review.as_mut() else {
        return true;
    };
    let Some(handle) = s.handle.as_ref() else {
        return true;
    };
    if s.closing {
        return true;
    }
    handle.flags.detach_requested.store(true, Ordering::Release);
    s.closing = true;
    s.awaiting = None;
    false
}

/// The worker's `TurnStarted`: Esc now acts on this turn.
pub fn on_turn_started(app: &mut App, turn: TurnRef) {
    if let Some(s) = app.review.as_mut() {
        s.turn.current = Some(turn);
        s.esc = ReviewEsc::TurnInFlight;
    }
}

/// The worker's `TurnEnded`: the cell is already settled, so this disarms
/// Esc but cannot undo a won abort (P3b-§6.3 step 6).
pub fn on_turn_ended(app: &mut App) {
    if let Some(s) = app.review.as_mut() {
        s.turn.current = None;
        s.turn.pending_cancel = None;
        s.esc = ReviewEsc::Detached;
    }
}

/// An async cancel failed (production path, see [`cancel_via_tokio`]). The
/// footer carries the user-facing text; the cause goes to the log.
pub fn on_cancel_failed(app: &mut App, err: &str) {
    tracing::warn!(error = %err, "review: tasks/cancel failed");
    if let Some(s) = app.review.as_mut() {
        s.turn.cancel_unsupported = true;
    }
}

/// The footer's review hint, most urgent first. `None` = the plain hint.
#[allow(dead_code)] // wired in T9: `ui/status.rs` right hint
pub fn footer_hint(app: &App) -> Option<&'static str> {
    let s = app.review.as_ref()?;
    if s.closing {
        Some(REVIEW_FOOTER_CLOSING)
    } else if s.turn.cancel_unsupported {
        Some(REVIEW_CANCEL_UNSUPPORTED)
    } else if s.turn.pause_armed {
        Some(REVIEW_FOOTER_PAUSE_ARMED)
    } else {
        None
    }
}

/// The real cancel: `tasks/cancel` off the UI thread. It returns at once; a
/// failure comes back as [`StreamMsg::ReviewCancelFailed`].
pub fn cancel_via_tokio(tx: Sender<StreamMsg>) -> impl Fn(PathBuf, String, String) -> Result<()> {
    move |home, member, task_id| {
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = cancel_task(home, member, task_id).await {
                let _ = tx
                    .send(StreamMsg::ReviewCancelFailed(format!("{e:#}")))
                    .await;
            }
        });
        Ok(())
    }
}

fn fire(s: &mut super::ReviewSession, cancel: &CancelFn, home: PathBuf, member: &str, id: &str) {
    if cancel(home, member.to_string(), id.to_string()).is_err() {
        s.turn.cancel_unsupported = true;
    }
}
