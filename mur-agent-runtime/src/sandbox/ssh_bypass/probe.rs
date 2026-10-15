//! Live probe: confirm a [`Classification::Candidate`] by actually dialing the
//! upstream from a child under the agent's own sandbox profile. Connect only —
//! no ssh-agent protocol bytes are ever sent.
//!
//! The parent never trusts anything but a clean `EPERM`/`EACCES` from a child
//! that reports it applied the profile: every other result is inconclusive,
//! and inconclusive is audit-only.

use super::{AuditReason, Classification};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Hidden runtime subcommand that runs the probe child.
pub const PROBE_SUBCOMMAND: &str = "ssh-bypass-probe";

/// How long the parent waits for the probe child before calling it
/// inconclusive. Bounds the (non-blocking) delay the probe adds to start.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How often the parent polls the child for exit while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Child → parent exit-code protocol. Any other code, or death by signal,
/// is inconclusive.
pub const EXIT_CONNECTED: i32 = 0;
pub const EXIT_DENIED: i32 = 10;
pub const EXIT_OTHER_ERRNO: i32 = 11;
pub const EXIT_SANDBOX_FAILED: i32 = 12;

/// What the probe observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// `connect` failed with EPERM/EACCES under the profile.
    Denied,
    /// `connect` succeeded under the profile.
    Connected,
    /// Anything else: spawn failure, timeout, sandbox apply failure, other
    /// errno, unknown exit code, killed by signal.
    Inconclusive(String),
}

impl ProbeOutcome {
    /// Only a clean denial claims a boundary.
    pub fn classification(&self) -> Classification {
        match self {
            Self::Denied => Classification::Boundary,
            Self::Connected => Classification::AuditOnly(AuditReason::ProbeReachable),
            Self::Inconclusive(_) => Classification::AuditOnly(AuditReason::ProbeInconclusive),
        }
    }
}

/// Spawn `exe PROBE_SUBCOMMAND <upstream>`, feed it `profile` on stdin, and
/// map its exit to an outcome, killing it after `timeout`.
pub fn run_probe(exe: &Path, profile: &str, upstream: &Path, timeout: Duration) -> ProbeOutcome {
    let mut child = match Command::new(exe)
        .arg(PROBE_SUBCOMMAND)
        .arg(upstream)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::Inconclusive(format!("spawn failed: {e}")),
    };

    // Write on a separate thread so a child that never drains stdin cannot
    // stall the parent past `timeout`. A write error (the child exited first)
    // surfaces through the exit status, so it is ignored here.
    if let Some(mut stdin) = child.stdin.take() {
        let profile = profile.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(profile.as_bytes());
        });
    }

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return outcome_for_exit(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return ProbeOutcome::Inconclusive(format!("timed out after {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return ProbeOutcome::Inconclusive(format!("wait failed: {e}"));
            }
        }
    }
}

fn outcome_for_exit(status: ExitStatus) -> ProbeOutcome {
    match status.code() {
        Some(EXIT_CONNECTED) => ProbeOutcome::Connected,
        Some(EXIT_DENIED) => ProbeOutcome::Denied,
        Some(EXIT_OTHER_ERRNO) => {
            ProbeOutcome::Inconclusive("connect failed with another errno".into())
        }
        Some(EXIT_SANDBOX_FAILED) => {
            ProbeOutcome::Inconclusive("child could not apply the sandbox".into())
        }
        Some(code) => ProbeOutcome::Inconclusive(format!("unexpected exit code {code}")),
        None => ProbeOutcome::Inconclusive("child killed by signal".into()),
    }
}

/// Child side, after the profile is applied: dial `upstream`, close, and
/// return the exit code for the parent. Never writes to the socket.
pub fn connect_only(upstream: &Path) -> i32 {
    exit_code_for_connect(&UnixStream::connect(upstream).map(drop))
}

/// Map the child's `connect` result to the exit-code protocol.
pub fn exit_code_for_connect(result: &std::io::Result<()>) -> i32 {
    match result {
        Ok(()) => EXIT_CONNECTED,
        Err(e) if matches!(e.raw_os_error(), Some(libc::EPERM | libc::EACCES)) => EXIT_DENIED,
        Err(_) => EXIT_OTHER_ERRNO,
    }
}

#[cfg(test)]
mod tests;
