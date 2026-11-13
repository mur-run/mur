//! The real [`Lifecycle`]: `mur agent stop`'s order (service out of the way
//! first, then SIGTERM, SIGKILL if needed) and `mur agent start`. Not the
//! restart `Launcher` as a whole: it relies on KeepAlive to respawn, which
//! would race the sentinel (spec "Open for planning").

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use mur_common::lock_file::pid_alive;

use super::super::super::restart::{CliLauncher, Launcher, kill_wait_secs, stop_timeout_for};
use super::super::super::service::stop_service;
use super::super::super::start::cmd_start;
use super::apply::Lifecycle;

/// Lines of a failed quiet start's stderr kept for the card.
const START_ERR_LINES: usize = 6;

/// How long to wait, after `mur agent start` returns, for a live
/// `running.lock`. `start` only confirms for a few seconds and treats a
/// longer start as "not confirmed yet", never "failed"; a heavy start (local
/// model load) legitimately takes far longer. Same window murmur's autostart
/// gives a runtime before it falls back to the manual hint.
const READY_WINDOW: Duration = Duration::from_secs(60);
const READY_POLL: Duration = Duration::from_millis(250);

/// Poll `ready` until it yields a value or `window` elapses.
fn wait_ready<T>(
    window: Duration,
    poll: Duration,
    mut ready: impl FnMut() -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + window;
    loop {
        if let Some(v) = ready() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(poll);
    }
}

pub struct AgentLifecycle {
    name: String,
    home: PathBuf,
    /// Set for a caller that owns the terminal (murmur): start through a
    /// `mur agent start` child with its output captured, because
    /// `cmd_start` prints and would tear a TUI's screen.
    quiet_exe: Option<PathBuf>,
}

impl AgentLifecycle {
    pub fn new(name: &str, home: &std::path::Path) -> Self {
        Self {
            name: name.to_owned(),
            home: home.to_owned(),
            quiet_exe: None,
        }
    }

    /// Start through `mur_exe agent start <name>` with output captured.
    pub fn quiet(name: &str, home: &std::path::Path, mur_exe: PathBuf) -> Self {
        Self {
            quiet_exe: Some(mur_exe),
            ..Self::new(name, home)
        }
    }

    fn start_child(&self, exe: &std::path::Path) -> Result<()> {
        let out = std::process::Command::new(exe)
            .args(["agent", "start", &self.name])
            .stdin(std::process::Stdio::null())
            .output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(START_ERR_LINES).collect();
            bail!(
                "{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n").trim()
            );
        }
        Ok(())
    }

    fn agent_home(&self) -> PathBuf {
        self.home.join("agents").join(&self.name)
    }
}

impl Lifecycle for AgentLifecycle {
    fn running_pid(&self) -> Option<u32> {
        mur_common::lock_file::read(&self.agent_home().join("running.lock"))
            .ok()
            .flatten()
            .map(|l| l.pid)
            .filter(|p| pid_alive(*p))
    }

    fn stop(&mut self, pid: u32) -> Result<()> {
        let had_service = stop_service(&self.name);
        let timeout = stop_timeout_for(&self.name, &self.agent_home(), &mut |_| {});
        let mut launcher = CliLauncher::new(&self.name, &self.home, had_service);
        launcher
            .stop(pid, Duration::from_secs(kill_wait_secs(timeout)))
            .map_err(|e| anyhow::anyhow!("pid {} is still alive", e.pid))
    }

    fn start(&mut self) -> Result<()> {
        match &self.quiet_exe {
            Some(exe) => self.start_child(exe)?,
            None => cmd_start(&self.name)?,
        }
        // `start` returning is not the verdict: it gives up confirming after
        // a short window and calls that a slow start. Wait the full ready
        // window before telling the card the runtime did not come up.
        if wait_ready(READY_WINDOW, READY_POLL, || self.running_pid()).is_none() {
            bail!(
                "it did not report ready within {}s; `mur agent status {}` shows the current state",
                READY_WINDOW.as_secs(),
                self.name
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn wait_ready_sees_a_runtime_that_comes_up_late() {
        let calls = Cell::new(0);
        let got = wait_ready(Duration::from_secs(5), Duration::from_millis(1), || {
            calls.set(calls.get() + 1);
            (calls.get() >= 3).then_some(42u32)
        });
        assert_eq!(got, Some(42));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn wait_ready_gives_up_after_the_window() {
        let got: Option<u32> =
            wait_ready(Duration::from_millis(20), Duration::from_millis(5), || None);
        assert_eq!(got, None);
    }
}
