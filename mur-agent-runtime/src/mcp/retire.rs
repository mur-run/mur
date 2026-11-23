//! Background stop of retired MCP clients (#2360, spec "Mid-run eviction",
//! "Shared clients").
//!
//! A client is retired when its entry is hot-reloaded or the agent stops.
//! It may still be held by an in-flight tool call, so the pool cannot stop
//! it on the spot; leaving it to its last `Drop` would SIGKILL it and lose
//! whatever it saves on exit. Instead a task waits for the other holders,
//! then runs the graceful stop. Once the agent stops, every such task must
//! finish by the stop deadline.
//!
//! A hot-reloaded stdio client that nobody holds is different (#2543): its
//! replacement waits for it to exit ([`Retiring::retire_before_restart`],
//! [`Retiring::exited`]). A server may save state on its way out that the
//! next generation reads as it starts; the browser server writes its login
//! back to the profile, and a new browser that loaded the profile first
//! would later have its own write-back refused as a foreign change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::protocol::mcp_client::McpClient;

/// Bound on one retired client's graceful stop (and the slice an agent stop
/// reserves for MCP servers). A browser server spends it, after stdin EOF,
/// on its login export and write-back; SIGTERM comes `TERM_GRACE` before the
/// end. `mur-core` asserts that `mur-browser`'s `EXPORT_TIMEOUT` plus
/// `WRITEBACK_BUDGET` plus `TERM_GRACE` fits inside it, so an export that
/// finishes in time is always written back before the signal.
pub const MCP_STOP_SLICE: Duration = Duration::from_secs(10);

/// How often a retiring task checks whether the other holders let go.
const HOLDER_POLL: Duration = Duration::from_millis(25);

#[derive(Default)]
pub(super) struct Retiring {
    /// Set once, at agent stop: every retiring task's hard end.
    deadline: Arc<OnceLock<Instant>>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
    /// Per server: held by the stop of an idle generation until its process
    /// has exited, so the next generation can wait for it.
    exiting: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Retiring {
    /// From now on, every retired client must be stopped by `deadline`.
    pub(super) fn begin_stop(&self, deadline: Instant) {
        let _ = self.deadline.set(deadline);
    }

    /// Stop `client` in the background once nobody else holds it.
    pub(super) fn retire(&self, server: String, client: Arc<Mutex<McpClient>>) {
        let deadline = self.deadline.clone();
        self.track(tokio::spawn(stop_when_released(server, client, deadline)));
    }

    /// Retire a hot-reloaded `client` whose replacement is about to start.
    ///
    /// An idle stdio client (the caller holds the only reference) is stopped
    /// under the server's exit lock, which [`exited`](Self::exited) waits
    /// for. A client an in-flight call still holds is retired in the
    /// background as before: that call may run for minutes, and the turn
    /// does not wait for it (spec "Mid-run eviction").
    ///
    /// Not async on purpose: the client is handed to its stop task before
    /// the caller can be cancelled, so a caller that gives up waiting never
    /// drops (and so SIGKILLs) it. Call [`exited`](Self::exited) first, so
    /// the exit lock is free; if it is not, the stop is not waited for.
    pub(super) fn retire_before_restart(&self, server: String, client: Arc<Mutex<McpClient>>) {
        // Sole owner: the pool already dropped its reference, so the count
        // cannot rise again and the lock is free.
        let idle_stdio = Arc::strong_count(&client) == 1
            && client
                .try_lock()
                .is_ok_and(|c| matches!(*c, McpClient::Stdio(_)));
        let exit = match idle_stdio {
            true => self.exit_lock(&server).try_lock_owned().ok(),
            false => None,
        };
        let deadline = self.deadline.clone();
        self.track(tokio::spawn(async move {
            stop_when_released(server, client, deadline).await;
            drop(exit);
        }));
    }

    /// Wait until no idle generation of `server` is still exiting. Bounded
    /// by that stop's own deadline (`MCP_STOP_SLICE`, or the agent's stop).
    pub(super) async fn exited(&self, server: &str) {
        drop(self.exit_lock(server).lock().await);
    }

    fn exit_lock(&self, server: &str) -> Arc<Mutex<()>> {
        let mut exiting = self.exiting.lock().unwrap_or_else(|e| e.into_inner());
        exiting.entry(server.to_owned()).or_default().clone()
    }

    fn track(&self, task: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }

    /// Wait for every retiring task, bounded by `deadline`.
    pub(super) async fn join(&self, deadline: Instant) {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        let all = futures::future::join_all(tasks);
        if tokio::time::timeout_at(deadline, all).await.is_err() {
            tracing::warn!("MCP servers still stopping at the stop deadline; leaving them to exit");
        }
    }
}

async fn stop_when_released(
    server: String,
    mut client: Arc<Mutex<McpClient>>,
    stop_deadline: Arc<OnceLock<Instant>>,
) {
    // Holders are in-flight calls; outside a stop they always finish, so
    // only the stop deadline bounds this wait (the holder sub-deadline).
    loop {
        match Arc::try_unwrap(client) {
            Ok(m) => {
                let end = stop_end(&stop_deadline);
                let out = m.into_inner().stop_by(end).await;
                tracing::info!(server = %server, outcome = ?out, "MCP server stopped");
                return;
            }
            Err(shared) => client = shared,
        }
        if stop_deadline.get().is_some_and(|d| Instant::now() >= *d) {
            // Falls back to `Drop` (SIGKILL) when the holder lets go.
            tracing::warn!(
                server = %server,
                "MCP server still in use at the stop deadline; skipped its graceful stop"
            );
            return;
        }
        tokio::time::sleep(HOLDER_POLL).await;
    }
}

/// When a stop starting now must end: the agent's stop deadline if one is
/// set, else `MCP_STOP_SLICE` from now.
fn stop_end(stop_deadline: &OnceLock<Instant>) -> Instant {
    let own = Instant::now() + MCP_STOP_SLICE;
    stop_deadline.get().map_or(own, |d| (*d).min(own))
}
