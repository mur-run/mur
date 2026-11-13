//! The real [`Lifecycle`]: `mur agent stop`'s order (service out of the way
//! first, then SIGTERM, SIGKILL if needed) and `mur agent start`. Not the
//! restart `Launcher` as a whole: it relies on KeepAlive to respawn, which
//! would race the sentinel (spec "Open for planning").

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use mur_common::lock_file::pid_alive;

use super::super::super::restart::{CliLauncher, Launcher, kill_wait_secs, stop_timeout_for};
use super::super::super::service::stop_service;
use super::super::super::start::cmd_start;
use super::apply::Lifecycle;

pub struct AgentLifecycle {
    name: String,
    home: PathBuf,
}

impl AgentLifecycle {
    pub fn new(name: &str, home: &std::path::Path) -> Self {
        Self {
            name: name.to_owned(),
            home: home.to_owned(),
        }
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
        cmd_start(&self.name)?;
        if self.running_pid().is_none() {
            bail!(
                "it did not report ready in time; `mur agent status {}` shows the current state",
                self.name
            );
        }
        Ok(())
    }
}
