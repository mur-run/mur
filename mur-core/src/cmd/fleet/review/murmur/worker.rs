//! The review worker thread and its lock hand-off (P3b-§4.3, §4.4, §7.2, §8).
//!
//! The UI thread takes the run lock and, for a resume, folds the channel
//! (`prepare_resume`) — both synchronously, so a refusal shows before any
//! thread exists. Everything then moves into the worker: it drives the loop
//! through a [`MurmurTransport`], reports one [`DriverEvent::Finished`], and
//! drops the lock when the thread ends. The UI joins the handle before it
//! tries the lock again.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Instant;

use mur_common::fleet::Fleet;

use super::super::constants::{REVIEW_AUTO_DEGRADED_NOTICE, TRANSPORT_RETRY_DELAY};
use super::super::driver::ReviewTransport;
use super::super::loop_driver::run_review_loop;
use super::super::resume::{Resumable, ResumeEnd, settle_then_resume};
use super::super::run_lock::DriverLock;
use super::super::schema::{Mode, SessionLimits};
use super::super::session::{apply_requested_pause, end_session};
use super::bridge::{DriverEvent, DriverReq, Outcome, ReviewFlags};
use super::transport::{DialFn, MurmurTransport, RespondFn, real_io};

/// What the worker starts from. The lock is already held in both cases:
/// a fresh session carries it here, a resume carries it inside [`Resumable`].
pub enum StartKind {
    /// `fleet` is boxed: both variants are large (`large_enum_variant`).
    Fresh {
        fleet: Box<Fleet>,
        task: String,
        limits: SessionLimits,
        lock: DriverLock,
    },
    Resume(Box<Resumable>),
}

/// The UI thread's grip on a running worker.
pub struct WorkerHandle {
    /// Join before taking the run lock again: the lock drops as the thread ends.
    pub join: JoinHandle<()>,
    pub flags: ReviewFlags,
    // No reader: `ReviewSession::name` is what the UI shows. Remove in T12
    // unless a task gives it one.
    #[allow(dead_code)]
    pub name: String,
    pub members: [String; 2],
}

/// Start the review worker over the real A2A edges.
pub fn spawn_review_worker(
    mur_home: PathBuf,
    kind: StartKind,
    req: Sender<DriverReq>,
    done: Sender<DriverEvent>,
) -> std::io::Result<WorkerHandle> {
    let (dial, respond) = real_io(&mur_home);
    spawn_with_io(mur_home, kind, req, done, dial, respond)
}

/// Same, with the network edges injected.
pub fn spawn_with_io(
    mur_home: PathBuf,
    kind: StartKind,
    req: Sender<DriverReq>,
    done: Sender<DriverEvent>,
    dial: DialFn,
    respond: RespondFn,
) -> std::io::Result<WorkerHandle> {
    let fleet = match &kind {
        StartKind::Fresh { fleet, .. } => fleet,
        StartKind::Resume(r) => &r.fleet,
    };
    let (name, members) = (
        fleet.name.clone(),
        [fleet.members[0].clone(), fleet.members[1].clone()],
    );
    let flags = ReviewFlags::default();
    let transport = (flags.clone(), members.clone());
    let join = std::thread::Builder::new()
        .name(format!("review-{name}"))
        .spawn(move || {
            let (flags, members) = transport;
            let transport =
                MurmurTransport::with_io(mur_home.clone(), req, flags, members, dial, respond);
            // A panic in the loop must still end the session on the UI side.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&transport, &mur_home, kind)
            }))
            .unwrap_or_else(|_| Outcome::Err("the review worker panicked".into()));
            let _ = done.send(DriverEvent::Finished(outcome));
        })?;
    Ok(WorkerHandle {
        join,
        flags,
        name,
        members,
    })
}

fn run(transport: &MurmurTransport, mur_home: &Path, kind: StartKind) -> Outcome {
    match kind {
        StartKind::Fresh {
            fleet,
            task,
            limits,
            lock,
        } => {
            let _lock = lock;
            let fleet = *fleet;
            let [main, reviewer] = [&fleet.members[0], &fleet.members[1]];
            let run = run_review_loop(
                transport,
                mur_home,
                &fleet.name,
                &fleet.channel_id,
                main,
                reviewer,
                &task,
                Mode::SemiAuto,
                TRANSPORT_RETRY_DELAY,
                limits,
                &Instant::now,
            );
            let run = apply_requested_pause(transport, mur_home, &fleet.channel_id, run);
            match end_session(mur_home, &fleet, run) {
                Ok((ledger, stop)) => {
                    Outcome::Ran(stop, Box::new(ledger), fleet.channel_id.clone())
                }
                Err(e) => Outcome::Err(format!("{e:#}")),
            }
        }
        StartKind::Resume(r) => {
            // 3b runs semi-auto only (Q7): say so before anything is asked.
            if r.ledger.mode == Mode::Auto {
                let _ = transport.show(REVIEW_AUTO_DEGRADED_NOTICE);
            }
            let channel_id = r.fleet.channel_id.clone();
            match settle_then_resume(transport, mur_home, *r, TRANSPORT_RETRY_DELAY) {
                Ok(ResumeEnd::LeftPaused) => Outcome::LeftPaused,
                Ok(ResumeEnd::Ran(ledger, stop)) => Outcome::Ran(stop, ledger, channel_id),
                Err(e) => Outcome::Err(format!("{e:#}")),
            }
        }
    }
}
