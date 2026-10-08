//! `/review --main … --reviewer …` with nothing attached (P3b-§3.1, §3.3,
//! §4.3): the same pre-flight as `mur fleet review`, the run lock taken on
//! the UI thread, then the worker — its requests forwarded onto the UI stream.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use mur_channel::ChannelService;
use tokio::sync::mpsc::Sender;

use super::state::ReviewSession;
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::fleet::review::murmur::bridge::{DriverEvent, DriverReq};
use crate::cmd::fleet::review::murmur::worker::{StartKind, spawn_review_worker};
use crate::cmd::fleet::review::run_lock::try_acquire;
use crate::cmd::fleet::review::session::{ReviewArgs, prepare_session, remove_session_fleet};

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
    let (req, done) = forward(tx.clone());
    let handle = spawn_review_worker(PathBuf::from(home), kind, req, done)
        .map_err(|e| undo(anyhow!("cannot start the review worker: {e}")))?;
    let session = ReviewSession {
        name,
        channel_id,
        handle: Some(handle),
        esc: ReviewEsc::Detached,
        closing: false,
    };
    Ok((session, banner))
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
