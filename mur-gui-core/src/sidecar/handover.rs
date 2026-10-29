//! Lending a supervised child to a blocking caller and taking it back
//! (Hub spec §Launchers). The startup-override restart has to signal and wait
//! for a runtime the Hub owns, and the actor must never block, so the actor
//! hands the `Child` over, the caller stops it on its own thread, and anything
//! it could not finish comes back here to be reaped on the poll tick.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::warn;

/// How often [`stop_child`] polls `try_wait` while it waits for an exit.
const STOP_POLL: Duration = Duration::from_millis(50);

/// What `Supervisor::start_watched` started.
#[derive(Debug, Clone)]
pub struct Watched {
    /// The child this call spawned or found; `None` when a live runtime the
    /// Hub does not own already holds the lock.
    pub pid: Option<u32>,
    /// Filled once, from the actor's poll, when that child exits.
    pub exit: watch::Receiver<Option<ExitStatus>>,
}

/// SIGTERM, poll `try_wait` for `kill_wait`, SIGKILL, poll for `post_kill`.
/// `Ok(status)` once reaped; `Err(())` if it still is not (the caller keeps
/// the child and gives it back). Blocking — never call it from the actor.
#[allow(clippy::result_unit_err)]
pub fn stop_child(
    child: &mut Child,
    kill_wait: Duration,
    post_kill: Duration,
) -> Result<ExitStatus, ()> {
    if let Ok(Some(st)) = child.try_wait() {
        return Ok(st);
    }
    terminate(child);
    if let Some(st) = wait_for(child, kill_wait) {
        return Ok(st);
    }
    let _ = child.kill();
    wait_for(child, post_kill).ok_or(())
}

#[cfg(unix)]
fn terminate(child: &Child) {
    // SAFETY: kill(2) on a pid we still own (not yet reaped, so not reused).
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate(_child: &Child) {
    // No polite signal to send; the SIGKILL-equivalent below does the work.
}

fn wait_for(child: &mut Child, within: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Some(st),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(STOP_POLL),
            _ => return None,
        }
    }
}

/// A child coming back from a caller. If the slot was refilled meanwhile (a
/// Start while it was lent out), the returned one is a stray: kill it and
/// reap it later, never `wait()` here.
pub(super) fn reinsert(
    children: &mut HashMap<String, Child>,
    reaping: &mut Vec<(String, Child)>,
    slug: String,
    mut child: Child,
) {
    match children.entry(slug) {
        Entry::Occupied(slot) => {
            let slug = slot.key().clone();
            warn!(agent = %slug, pid = child.id(), "returned runtime lost its slot; killing it");
            let _ = child.kill();
            reaping.push((slug, child));
        }
        Entry::Vacant(slot) => {
            slot.insert(child);
        }
    }
}

/// Keep only the strays that have not been reaped yet.
pub(super) fn drain_reaping(reaping: &mut Vec<(String, Child)>) {
    reaping.retain_mut(|(_, c)| matches!(c.try_wait(), Ok(None)));
}

/// Tell each watcher whose child has exited. `Child::try_wait` caches the
/// status once reaped, so a child already reaped by `emit_status` still
/// reports it.
pub(super) fn notify_exits(
    children: &mut HashMap<String, Child>,
    watchers: &mut HashMap<String, watch::Sender<Option<ExitStatus>>>,
) {
    watchers.retain(
        |slug, tx| match children.get_mut(slug).map(Child::try_wait) {
            Some(Ok(Some(st))) => {
                let _ = tx.send(Some(st));
                false
            }
            _ => true,
        },
    );
}

/// A watched child is leaving the map for good (Stop, Shutdown).
pub(super) fn notify_one(
    watchers: &mut HashMap<String, watch::Sender<Option<ExitStatus>>>,
    slug: &str,
    status: Option<ExitStatus>,
) {
    if let Some(tx) = watchers.remove(slug)
        && let Some(st) = status
    {
        let _ = tx.send(Some(st));
    }
}

#[cfg(all(test, unix))]
#[path = "handover_tests.rs"]
mod tests;
