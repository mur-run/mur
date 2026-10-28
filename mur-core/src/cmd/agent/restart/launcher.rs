//! The stop and launch half of a restart, behind a trait so the startup
//! override orchestrator (and later the Hub) drives the same steps as
//! `mur agent restart`. `CliLauncher` is the code `restart_one_with` and
//! `restart_confirm` used to inline, moved without changing what it does.
//!
//! Spec: `2026-10-25-startup-overrides-hub-design.md` §The seam in
//! `restart_one_with`, §Launchers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::super::pid_alive;
use super::super::restart_confirm::{managed_pid, pid_runnable};
use super::{RestartNote, direct_respawn, kickstart_service};

/// How long to wait for the pid to vanish after SIGKILL before calling the
/// stop failed. SIGKILL cannot be caught; this only covers the kernel
/// tearing the process down.
const POST_KILL_WAIT: Duration = Duration::from_secs(2);
const STOP_POLL: Duration = Duration::from_millis(100);

/// The old runtime is still alive after SIGTERM, the drain window, SIGKILL
/// and [`POST_KILL_WAIT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cmd::agent) struct StopFailed {
    pub pid: u32,
}

impl std::fmt::Display for StopFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pid {} is still alive after SIGTERM and SIGKILL",
            self.pid
        )
    }
}

pub(in crate::cmd::agent) trait Launcher {
    /// Stop the runtime whose lock file names `old_pid`. `Ok` once the pid
    /// is gone (exiting on its own counts).
    fn stop(&mut self, old_pid: u32, kill_wait: Duration) -> Result<(), StopFailed>;
    /// Bring a fresh runtime up. `Ok(None)`: a service manager will start it
    /// (KeepAlive, or a unit this launcher already asked to start).
    /// `Ok(Some(pid))`: this launcher spawned `pid`.
    fn start(&mut self) -> Result<Option<u32>>;
    /// No fresh lock within the passive window: kick the service or respawn.
    /// Returns the pid to liveness-gate the next window on, when known.
    fn kick(&mut self, note: &mut dyn FnMut(RestartNote)) -> Result<Option<u32>>;
    /// A runnable process that may still write the lock: not `old_pid`, not
    /// a zombie. `None` when nothing is coming up on its own.
    fn fresh_pid(&self, old_pid: Option<u32>) -> Option<u32>;
}

/// `mur agent restart`'s launcher: signals for stop, launchd / systemd or a
/// detached direct spawn for start and kick.
pub(in crate::cmd::agent) struct CliLauncher {
    name: String,
    agent_home: PathBuf,
    home: PathBuf,
    has_service: bool,
    direct_pid: Option<u32>,
}

impl CliLauncher {
    pub(in crate::cmd::agent) fn new(name: &str, home: &Path, has_service: bool) -> Self {
        Self {
            name: name.to_owned(),
            agent_home: home.join("agents").join(name),
            home: home.to_owned(),
            has_service,
            direct_pid: None,
        }
    }

    fn respawn(&mut self) -> Result<u32> {
        let pid = direct_respawn(&self.name, &self.agent_home, &self.home)?;
        self.direct_pid = Some(pid);
        Ok(pid)
    }
}

impl Launcher for CliLauncher {
    fn stop(&mut self, old_pid: u32, kill_wait: Duration) -> Result<(), StopFailed> {
        signal(old_pid, Signal::Term);
        // The SIGKILL fallback never fires while the runtime is still
        // draining: `kill_wait` is the profile's stop timeout plus a grace.
        wait_gone(old_pid, kill_wait);
        if pid_alive(old_pid) {
            signal(old_pid, Signal::Kill);
            wait_gone(old_pid, POST_KILL_WAIT);
        }
        if pid_alive(old_pid) {
            return Err(StopFailed { pid: old_pid });
        }
        Ok(())
    }

    fn start(&mut self) -> Result<Option<u32>> {
        if self.has_service {
            // KeepAlive respawns on exit; the wait finds the fresh lock.
            return Ok(None);
        }
        self.respawn().map(Some)
    }

    fn kick(&mut self, note: &mut dyn FnMut(RestartNote)) -> Result<Option<u32>> {
        let name = self.name.clone();
        if !self.has_service {
            note(RestartNote::Info(format!(
                "agent '{name}': no respawn seen; retrying direct respawn"
            )));
            return self.respawn().map(Some);
        }
        match kickstart_service(&name) {
            Ok(true) => {
                note(RestartNote::Info(format!(
                    "agent '{name}': no respawn seen; kicked the service unit"
                )));
                // The kick restarts the clock from zero, so gate the
                // post-kick window on the service manager's new pid.
                Ok(managed_pid(&name))
            }
            Ok(false) => {
                note(RestartNote::Info(format!(
                    "agent '{name}': service kickstart failed; falling back to direct respawn"
                )));
                self.respawn().map(Some)
            }
            // Attestation failed — fail closed: no kick and no direct spawn
            // (which would verify and fail too). The runtime needs repair.
            Err(e) => bail!("{e:#}"),
        }
    }

    fn fresh_pid(&self, old_pid: Option<u32>) -> Option<u32> {
        [managed_pid(&self.name), self.direct_pid]
            .into_iter()
            .flatten()
            .find(|pid| Some(*pid) != old_pid && pid_runnable(*pid))
    }
}

enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal(pid: u32, sig: Signal) {
    let sig = match sig {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` takes plain integers; a stale pid only yields ESRCH.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn signal(_pid: u32, _sig: Signal) {}

fn wait_gone(pid: u32, within: Duration) {
    let until = Instant::now() + within;
    while Instant::now() < until && pid_alive(pid) {
        std::thread::sleep(STOP_POLL);
    }
}
