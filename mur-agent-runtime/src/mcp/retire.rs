//! Background stop of retired MCP clients (#2360, spec "Mid-run eviction",
//! "Shared clients").
//!
//! A client is retired when its entry is hot-reloaded or the agent stops.
//! It may still be held by an in-flight tool call, so the pool cannot stop
//! it on the spot; leaving it to its last `Drop` would SIGKILL it and lose
//! whatever it saves on exit. Instead a task waits for the other holders,
//! then runs the graceful stop. Once the agent stops, every such task must
//! finish by the stop deadline.

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
}

impl Retiring {
    /// From now on, every retired client must be stopped by `deadline`.
    pub(super) fn begin_stop(&self, deadline: Instant) {
        let _ = self.deadline.set(deadline);
    }

    /// Stop `client` in the background once nobody else holds it.
    pub(super) fn retire(&self, server: String, client: Arc<Mutex<McpClient>>) {
        let deadline = self.deadline.clone();
        let task = tokio::spawn(stop_when_released(server, client, deadline));
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
