//! The Hub's [`Launcher`] for agents without an installed service (Hub spec
//! §Launchers, rows 2–3). The runtime may be a child of the Hub's own
//! supervisor, and the supervisor's actor must never block, so `stop` takes
//! the `Child` out of the actor, stops it on this (blocking) thread and gives
//! back anything it could not reap. `start` and `kick` always go through the
//! supervisor's `start_watched`, never the CLI's detached direct respawn, so
//! the new runtime is again a Hub-supervised child.
//!
//! A service-installed agent uses `CliLauncher` instead (row 1).

use std::path::Path;
use std::process::{Child, ExitStatus};
use std::time::Duration;

use anyhow::{Result, anyhow};
use mur_core::cmd::agent::{CliLauncher, Launcher, POST_KILL_WAIT, RestartNote, StopFailed};
use mur_gui_core::sidecar::{Supervisor, Watched, stop_child};
use tracing::warn;

/// How a taken child is stopped; a seam so tests can make it fail or panic.
type Stopper = fn(&mut Child, Duration, Duration) -> Result<ExitStatus, ()>;

pub struct HubLauncher {
    supervisor: Supervisor,
    rt: tokio::runtime::Handle,
    slug: String,
    cli: CliLauncher,
    /// A child taken from the supervisor and not yet reaped or returned.
    taken: Option<Child>,
    watched: Option<Watched>,
    /// The stop budget the orchestrator passed; also bounds the return wait.
    kill_wait: Duration,
    stopper: Stopper,
}

impl HubLauncher {
    pub fn new(
        supervisor: Supervisor,
        rt: tokio::runtime::Handle,
        name: &str,
        home: &Path,
        has_service: bool,
    ) -> Self {
        Self {
            supervisor,
            rt,
            slug: name.to_owned(),
            cli: CliLauncher::new(name, home, has_service),
            taken: None,
            watched: None,
            kill_wait: POST_KILL_WAIT,
            stopper: stop_child,
        }
    }

    #[cfg(test)]
    fn with_stopper(mut self, stopper: Stopper) -> Self {
        self.stopper = stopper;
        self
    }

    fn start_watched(&mut self) -> Result<Option<u32>> {
        let watched = self
            .rt
            .block_on(self.supervisor.start_watched(self.slug.clone()))
            .map_err(|e| anyhow!(e))?;
        let pid = watched.pid;
        self.watched = Some(watched);
        Ok(pid)
    }
}

impl Launcher for HubLauncher {
    fn stop(&mut self, old_pid: u32, kill_wait: Duration) -> Result<(), StopFailed> {
        self.kill_wait = kill_wait;
        let taken = self
            .rt
            .block_on(self.supervisor.take_child(self.slug.clone()));
        match taken {
            Some(child) if child.id() == old_pid => {
                // Held in `self` first, so a panicking stopper still leaves
                // the child for `Drop` to give back.
                let child = self.taken.insert(child);
                match (self.stopper)(child, kill_wait, POST_KILL_WAIT) {
                    Ok(_) => {
                        self.taken = None;
                        Ok(())
                    }
                    Err(()) => Err(StopFailed { pid: old_pid }),
                }
            }
            // The Hub owns some other process for this agent; that is not
            // the one the lock names. Put it back and signal the lock's pid.
            Some(other) => {
                if let Err(child) = self.supervisor.try_return_child(self.slug.clone(), other) {
                    self.taken = Some(child);
                }
                self.cli.stop(old_pid, kill_wait)
            }
            None => self.cli.stop(old_pid, kill_wait),
        }
    }

    fn start(&mut self) -> Result<Option<u32>> {
        self.start_watched()
    }

    fn kick(&mut self, note: &mut dyn FnMut(RestartNote)) -> Result<Option<u32>> {
        note(RestartNote::Info(format!(
            "agent '{}': no respawn seen; asked the Hub supervisor again",
            self.slug
        )));
        self.start_watched()
    }

    fn fresh_pid(&self, old_pid: Option<u32>) -> Option<u32> {
        let w = self.watched.as_ref()?;
        let pid = w.pid?;
        (Some(pid) != old_pid && w.exit.borrow().is_none() && mur_common::lock_file::pid_alive(pid))
            .then_some(pid)
    }

    fn exited_early(&self) -> Option<String> {
        let w = self.watched.as_ref()?;
        let status = (*w.exit.borrow())?;
        Some(format!(
            "the runtime exited ({status}) before it acknowledged"
        ))
    }
}

impl Drop for HubLauncher {
    fn drop(&mut self) {
        let Some(child) = self.taken.take() else {
            return;
        };
        let child = match self.supervisor.try_return_child(self.slug.clone(), child) {
            Ok(()) => return,
            Err(c) => c,
        };
        // Full or closed. A blocking wait for room is only allowed off the
        // runtime's threads; on one, skip straight to the kill.
        let child = if tokio::runtime::Handle::try_current().is_err() {
            match self.supervisor.return_child_within(
                &self.rt,
                self.slug.clone(),
                child,
                self.kill_wait,
            ) {
                Ok(()) => return,
                Err(c) => c,
            }
        } else {
            child
        };
        let mut child = child;
        let pid = child.id();
        // Kill now (no grace: this is the give-up path) and reap within the
        // post-kill bound so no zombie outlives the job.
        let _ = stop_child(&mut child, Duration::ZERO, POST_KILL_WAIT);
        warn!(
            pid,
            "override restart dropped a runtime it could not return"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "launcher_tests.rs"]
mod tests;
