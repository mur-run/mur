//! Starting a review with nothing attached: `/review --main … --reviewer …`
//! (P3b-§3.1, §3.3, §4.3) and `/review resume <n>` (§8). Every check and the
//! run lock happen here on the UI thread; only then does a worker exist, its
//! requests forwarded onto the UI stream.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use mur_channel::ChannelService;
use tokio::sync::mpsc::Sender;

use super::state::{Awaiting, ReviewSession};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::fleet::review::constants::{
    REVIEW_LEFT_PAUSED_NOTICE, REVIEW_PAUSED_CONTINUE_PROMPT, RULING_RECORDED_CONTINUE_PROMPT,
};
use crate::cmd::fleet::review::murmur::bridge::{DriverEvent, DriverReq};
use crate::cmd::fleet::review::murmur::worker::{StartKind, WorkerHandle, spawn_review_worker};
use crate::cmd::fleet::review::note::is_murmur_send;
use crate::cmd::fleet::review::resume::prepare_resume;
use crate::cmd::fleet::review::run_lock::try_acquire;
use crate::cmd::fleet::review::session::{
    ReviewArgs, prepare_session, remove_session_fleet, render_resume_summary, require_running,
};

/// Start a fresh session. `Ok` carries the attached session and the banner;
/// `Err` is the text to show (§3.3: `{:#}`, nothing left behind).
pub(super) fn start(
    home: &Path,
    args: &ReviewArgs,
    tx: &Sender<StreamMsg>,
) -> Result<(ReviewSession, String)> {
    let (fleet, limits, banner) = prepare_session(home, args)?;
    let (name, channel_id) = (fleet.name.clone(), fleet.channel_id.clone());
    // From here the fleet exists; any refusal removes it again.
    let undo = |e: anyhow::Error| {
        let _ = remove_session_fleet(home, &name);
        e
    };
    let svc = ChannelService::open(home).map_err(undo)?;
    // §4.3: the lock is taken here, synchronously, so a refusal shows before
    // any thread exists. `LockDenied`'s `Display` is the text (AC-P3b-7).
    let lock = try_acquire(&svc, &channel_id)
        .map_err(|e| undo(anyhow!("review session '{name}': {e}")))?;
    let kind = StartKind::Fresh {
        fleet: Box::new(fleet),
        task: args.task.clone(),
        limits,
        lock,
    };
    let handle = spawn(home, kind, tx).map_err(undo)?;
    Ok((attach(name, channel_id, Some(handle), None), banner))
}

/// §8 steps 2–5: `prepare_resume` (it takes the lock) and the members check,
/// as `mur fleet review-resume` does. `Ok` carries the session and the text
/// to show: the summary, then either the continue question (the session
/// waits holding the lock) or, when a ruling is owed, nothing more — the
/// worker starts straight into the ruling prompt. On `Err` the `Resumable`
/// is dropped, so the lock is released.
pub(super) fn resume(
    home: &Path,
    name: &str,
    tx: &Sender<StreamMsg>,
) -> Result<(ReviewSession, String)> {
    let r = prepare_resume(home, name)?;
    require_running(home, &[&r.fleet.members[0], &r.fleet.members[1]])?;
    let mut text = render_resume_summary(&r);
    let (name, channel_id) = (r.fleet.name.clone(), r.fleet.channel_id.clone());
    if r.ledger.pending_ruling().is_empty() {
        text.push_str(
            if r.ruling_recorded {
                RULING_RECORDED_CONTINUE_PROMPT
            } else {
                REVIEW_PAUSED_CONTINUE_PROMPT
            }
            .trim_end(),
        );
        let waiting = Awaiting::ResumeConfirm(Box::new(r));
        return Ok((attach(name, channel_id, None, Some(waiting)), text));
    }
    let handle = spawn(home, StartKind::Resume(Box::new(r)), tx)?;
    let text = text.trim_end().to_string();
    Ok((attach(name, channel_id, Some(handle), None), text))
}

/// The line typed at `Paused — continue?` (§8 step 4). Enter / `y` / `yes`
/// hand the held lock to a worker; anything else drops the `Resumable`,
/// which releases the lock, and leaves the session paused.
pub fn answer_resume(app: &mut App, line: &str, tx: &Sender<StreamMsg>) {
    let Some(Awaiting::ResumeConfirm(r)) = app.review.as_mut().and_then(|s| s.awaiting.take())
    else {
        return;
    };
    if !is_murmur_send(line) {
        app.review = None;
        app.push_system(REVIEW_LEFT_PAUSED_NOTICE);
        return;
    }
    match spawn(&app.home, StartKind::Resume(r), tx) {
        Ok(h) => {
            if let Some(s) = app.review.as_mut() {
                s.handle = Some(h);
            }
        }
        Err(e) => {
            app.review = None;
            app.push_error(format!("{e:#}"));
        }
    }
}

fn attach(
    name: String,
    channel_id: String,
    handle: Option<WorkerHandle>,
    awaiting: Option<Awaiting>,
) -> ReviewSession {
    ReviewSession {
        name,
        channel_id,
        handle,
        esc: ReviewEsc::Detached,
        awaiting,
        closing: false,
        hint: Default::default(),
    }
}

/// The worker for `kind`, its requests forwarded onto `tx`.
fn spawn(home: &Path, kind: StartKind, tx: &Sender<StreamMsg>) -> Result<WorkerHandle> {
    let (req, done) = forward(tx.clone());
    spawn_review_worker(PathBuf::from(home), kind, req, done)
        .map_err(|e| anyhow!("cannot start the review worker: {e}"))
}

/// The worker talks std mpsc; the UI loop reads tokio mpsc. One plain thread
/// per channel bridges them and ends when the worker drops its sender.
/// `Finished` waits for the request thread to drain, so it can never overtake
/// the last `Show` (the worker drops its request sender as its thread ends).
fn forward(
    tx: Sender<StreamMsg>,
) -> (
    std::sync::mpsc::Sender<DriverReq>,
    std::sync::mpsc::Sender<DriverEvent>,
) {
    let (req, req_rx) = std::sync::mpsc::channel::<DriverReq>();
    let (done, done_rx) = std::sync::mpsc::channel::<DriverEvent>();
    let reqs = tx.clone();
    let drained = std::thread::spawn(move || {
        for r in req_rx {
            if reqs.blocking_send(StreamMsg::ReviewReq(r)).is_err() {
                break; // UI gone: dropping `r` drops its reply, so the worker pauses
            }
        }
    });
    std::thread::spawn(move || {
        let finished = done_rx.recv();
        let _ = drained.join();
        if let Ok(DriverEvent::Finished(o)) = finished {
            let _ = tx.blocking_send(StreamMsg::ReviewFinished(o));
        }
    });
    (req, done)
}
