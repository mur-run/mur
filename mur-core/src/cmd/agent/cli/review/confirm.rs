//! The attached worker's requests and its end, on the UI thread (spec §4.1,
//! §5, §7.2): the send gate answered from the composer, transcript `Show`s,
//! and `Finished`.

use super::render::finished_block;
use super::state::Awaiting;
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::fleet::review::constants::REVIEW_LEFT_PAUSED_NOTICE;
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, Outcome};
use crate::cmd::fleet::review::note::{LineMode, send_answer_for};
use crate::cmd::fleet::review::wire::message_text;

/// One worker request. A request whose reply is dropped reads as "the human
/// is gone" on the worker side (`MurmurTransport::ask`).
pub fn on_request(app: &mut App, req: DriverReq) {
    match req {
        DriverReq::Confirm {
            member,
            params,
            open,
            reply,
        } => {
            let Some(s) = app.review.as_mut() else {
                return;
            };
            s.awaiting = Some(Awaiting::Confirm { open, reply });
            s.esc = ReviewEsc::AwaitingConfirm;
            let text = message_text(&params).unwrap_or_default();
            app.push_system(format!("--- next message to {member} ---\n{text}"));
        }
        DriverReq::Show(text) => app.push_system(text),
        DriverReq::TurnStarted {
            member,
            task_id,
            turn,
        } => super::keys::on_turn_started(
            app,
            super::keys::TurnRef {
                member,
                cell: turn,
                task_id,
            },
        ),
        DriverReq::TurnEnded { .. } => super::keys::on_turn_ended(app),
        DriverReq::Ruling { text, open, reply } => {
            let Some(s) = app.review.as_mut() else {
                return;
            };
            s.awaiting = Some(Awaiting::Ruling { open, reply });
            app.push_system(text.trim_end().to_string());
        }
        // wired in PR 4 (Task 11): the review HITL modal. Until then the
        // dropped reply denies.
        DriverReq::Hitl { .. } => {}
    }
}

/// The line typed at the ruling prompt (§7.1), returned verbatim: the
/// driver parses it as it parses stdin, re-asking with a hint on anything
/// it does not take. A bare Enter is `"\n"` there, never the EOF `""`.
pub fn answer_ruling(app: &mut App, line: &str) {
    let Some(Awaiting::Ruling { reply, .. }) = app.review.as_mut().and_then(|s| s.awaiting.take())
    else {
        return;
    };
    let line = if line.is_empty() { "\n" } else { line };
    // A send error means the worker already ended; `Finished` follows.
    let _ = reply.send(line.to_string());
}

/// Esc ×2 at the ruling prompt or `Paused — continue?` (AC-P3b-25, 28): the
/// EOF answer. Ruling → `""`, which the driver reads as leave paused; resume
/// → drop the `Resumable`, releasing the lock.
pub fn dismiss_prompt(app: &mut App) {
    match app.review.as_mut().and_then(|s| s.awaiting.take()) {
        Some(Awaiting::Ruling { reply, .. }) => {
            let _ = reply.send(String::new());
        }
        Some(Awaiting::ResumeConfirm(_)) => {
            app.review = None;
            app.push_system(REVIEW_LEFT_PAUSED_NOTICE);
        }
        // §6.1: Esc never answers the send gate; put it back.
        other => {
            if let Some(s) = app.review.as_mut() {
                s.awaiting = other;
            }
        }
    }
}

/// The line typed while `Awaiting::Confirm` (§5.2). A hint is shown and the
/// gate stays open; any answer is sent and the gate closes.
pub(super) fn answer_confirm(app: &mut App, line: &str) {
    let Some(s) = app.review.as_mut() else {
        return;
    };
    let Some(Awaiting::Confirm { open, reply }) = s.awaiting.take() else {
        return;
    };
    let Some(members) = s.handle.as_ref().map(|h| h.members.clone()) else {
        return;
    };
    let home = app.home.clone();
    let canon = |n: &str| crate::a2a_dial::canonicalize_agent_name(&home, n);
    match send_answer_for(line, &members, &open, LineMode::Murmur, canon) {
        Err(hint) => {
            s.awaiting = Some(Awaiting::Confirm { open, reply });
            app.push_system(hint);
        }
        Ok(answer) => {
            s.esc = ReviewEsc::Detached;
            // A send error means the worker already ended; `Finished` follows.
            let _ = reply.send(answer);
        }
    }
}

/// §7.2: render the outcome, join the worker, release the session. A
/// graceful close (§4.4.1) quits only now, after the turn was ledgered.
pub fn on_finished(app: &mut App, outcome: Outcome) {
    app.push_system(finished_block(&outcome));
    let session = app.review.take();
    if session.as_ref().is_some_and(|s| s.closing) {
        app.should_quit = true;
    }
    if let Some(handle) = session.and_then(|s| s.handle) {
        // `Finished` is the worker's last statement, so this returns at once;
        // a panic in the loop was already caught into `Outcome::Err`.
        let _ = handle.join.join();
    }
}
