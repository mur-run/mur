//! Graceful stop of a stdio MCP server (#2360, spec "Graceful stop").
//!
//! The MCP specification's stdio shutdown (lifecycle → shutdown → stdio):
//! close the server's stdin, wait for it to exit, then SIGTERM, then
//! SIGKILL. A server that saves state on exit (the browser proxy exports
//! its login on stdin EOF) only gets to do so on the first step; the old
//! `kill()` skipped straight to the last one.

use std::time::Duration;
use tokio::time::Instant;

use super::{McpClient, StdioMcpClient};

/// Time a server gets between SIGTERM and SIGKILL. Taken from the end of
/// the stop's deadline, so the stdin-EOF wait gets everything before it.
pub const TERM_GRACE: Duration = Duration::from_millis(500);

/// Poll interval while waiting for the child to exit. `std::process::Child`
/// has no async wait; the pool's children are few and a stop is rare.
const EXIT_POLL: Duration = Duration::from_millis(25);

/// How a stop ended. Logged; nothing branches on it outside tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The server exited after its stdin closed (the graceful path).
    Exited,
    /// It ignored stdin EOF and exited on SIGTERM.
    Terminated,
    /// It needed SIGKILL.
    Killed,
}

impl McpClient {
    /// Stop by `deadline`: the MCP stdio sequence for a stdio server (see
    /// [`StdioMcpClient::stop`]), a best-effort session DELETE for HTTP.
    pub async fn stop_by(self, deadline: tokio::time::Instant) -> StopOutcome {
        match self {
            McpClient::Stdio(c) => c.stop(deadline).await,
            McpClient::Http(c) => {
                let _ = tokio::time::timeout_at(deadline, c.shutdown()).await;
                StopOutcome::Exited
            }
        }
    }
}

impl StdioMcpClient {
    /// Stop this server by `deadline`, following the MCP stdio sequence.
    ///
    /// The stdin-EOF wait ends at `deadline - TERM_GRACE`; SIGTERM gets the
    /// rest; SIGKILL is the floor, so the child is always reaped on return
    /// and `Drop` finds nothing left to do.
    pub async fn stop(self: Box<Self>, deadline: Instant) -> StopOutcome {
        // Step 1: close stdin. Taking it out of the slot drops the pipe's
        // write end; a request racing this fails as a closed stream.
        drop(self.stdin.lock().await.take());

        let term_at = deadline.checked_sub(TERM_GRACE).unwrap_or(deadline);
        if self.exited_by(term_at).await {
            return StopOutcome::Exited;
        }
        #[cfg(unix)]
        {
            let pid = self.child.lock().await.id();
            // SAFETY: plain kill(2) on our own unreaped child; the pid
            // cannot be reused until we wait on it.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if self.exited_by(deadline).await {
                return StopOutcome::Terminated;
            }
        }
        let mut child = self.child.lock().await;
        let _ = child.kill();
        let _ = child.wait();
        StopOutcome::Killed
    }

    /// Whether the child has exited (and been reaped) by `at`.
    async fn exited_by(&self, at: Instant) -> bool {
        loop {
            if let Ok(Some(_)) = self.child.lock().await.try_wait() {
                return true;
            }
            if Instant::now() >= at {
                return false;
            }
            tokio::time::sleep(EXIT_POLL.min(at.saturating_duration_since(Instant::now()))).await;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;
    use mur_common::agent::McpServerEntry;

    fn entry(script: &str) -> McpServerEntry {
        serde_yaml_ng::from_str(&format!(
            "name: fake\ncommand: /bin/sh\nargs: [\"-c\", {script:?}]\n"
        ))
        .expect("entry")
    }

    async fn spawn(script: &str) -> Box<StdioMcpClient> {
        Box::new(
            StdioMcpClient::spawn(&entry(script), &SandboxPolicy::default(), None, None)
                .await
                .expect("spawn"),
        )
    }

    #[tokio::test]
    async fn a_server_that_exits_on_stdin_eof_is_never_signalled() {
        // `cat` exits on EOF; a marker proves it ran its own exit path.
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("eof");
        let c = spawn(&format!("cat >/dev/null; touch {}", mark.display())).await;
        let out = c.stop(Instant::now() + Duration::from_secs(5)).await;
        assert_eq!(out, StopOutcome::Exited);
        assert!(mark.exists(), "the server must reach its own exit path");
    }

    #[tokio::test]
    async fn a_server_ignoring_eof_gets_sigterm_then_sigkill() {
        // Honours SIGTERM: exits on it.
        let c = spawn("exec sleep 30").await;
        let t0 = Instant::now();
        let out = c.stop(t0 + Duration::from_secs(1)).await;
        assert_eq!(out, StopOutcome::Terminated);

        // Ignores SIGTERM too: only SIGKILL ends it, still inside the deadline.
        let c = spawn("trap '' TERM; while :; do sleep 1; done").await;
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(1);
        let out = c.stop(deadline).await;
        assert_eq!(out, StopOutcome::Killed);
        assert!(Instant::now() < deadline + Duration::from_millis(250));
    }
}
