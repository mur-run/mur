//! A dial with an absolute deadline (#2090).
//!
//! The per-peer idle timeout (90 s, or 600 s for a runtime that does not
//! beat) bounds each silence, not the call: a caller whose whole budget is a
//! wall-clock bound — recovering a failed delegate's spend before the run's
//! deadline — needs every socket read and write to stop at that instant.
//! Timing out the caller's `await` alone would leave the blocking dial
//! running on its thread.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::DialMode;

/// [`super::dial_method`] against a running agent, finished by `deadline`.
/// A deadline already past fails before connecting.
pub fn dial_method_until(
    home: &Path,
    agent_name: &str,
    method: &str,
    params: Value,
    deadline: Instant,
) -> Result<Value> {
    super::dial_method_inner(
        home,
        agent_name,
        method,
        params,
        DialMode::RequireRunning,
        Some(deadline),
    )
}

/// The socket timeout to arm now: the idle timeout, cut to what is left of
/// `deadline`. `None` once the deadline has passed. Never zero — a zero
/// `SO_RCVTIMEO` means "no timeout".
pub(super) fn bounded(idle: Duration, deadline: Option<Instant>) -> Option<Duration> {
    let Some(d) = deadline else {
        return Some(idle);
    };
    let left = d.saturating_duration_since(Instant::now());
    (!left.is_zero()).then(|| left.min(idle))
}

/// A unix stream whose every underlying read and write re-arms the socket
/// timeout to [`bounded`]: the idle timeout, cut to what is left of the
/// deadline. `SO_RCVTIMEO` bounds one blocking call, not a frame — a peer
/// trickling bytes without a newline would otherwise stretch one
/// `read_line` past the deadline, one short read at a time. Past the
/// deadline a call fails with `TimedOut` before touching the socket.
#[cfg(unix)]
pub(super) struct Bounded {
    pub(super) stream: std::os::unix::net::UnixStream,
    pub(super) idle: Duration,
    pub(super) deadline: Option<Instant>,
}

#[cfg(unix)]
impl Bounded {
    fn arm(&self, write: bool) -> std::io::Result<()> {
        let Some(t) = bounded(self.idle, self.deadline) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "deadline passed",
            ));
        };
        if write {
            self.stream.set_write_timeout(Some(t))
        } else {
            self.stream.set_read_timeout(Some(t))
        }
    }

    /// Whether the deadline, if any, has passed.
    pub(super) fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

#[cfg(unix)]
impl std::io::Read for Bounded {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.arm(false)?;
        self.stream.read(buf)
    }
}

#[cfg(unix)]
impl std::io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.arm(true)?;
        self.stream.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

pub(super) fn passed(agent_name: &str, request: &Value) -> anyhow::Error {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("?");
    anyhow!("agent '{agent_name}': {method} did not finish before its deadline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_timeout_never_outlasts_the_deadline() {
        let idle = Duration::from_secs(90);
        assert_eq!(bounded(idle, None), Some(idle));
        let far = Instant::now() + Duration::from_secs(600);
        assert_eq!(bounded(idle, Some(far)), Some(idle));
        let near = bounded(idle, Some(Instant::now() + Duration::from_secs(2))).unwrap();
        assert!(near <= Duration::from_secs(2) && !near.is_zero());
        assert_eq!(bounded(idle, Some(Instant::now())), None);
    }
}
