//! OS-managed agent lifecycle for MuR Hub (spec §3.1).
//!
//! Hub no longer spawns agent processes directly. Instead, `start()` registers
//! the agent with the OS init system (launchd / systemd --user / Run registry)
//! and tells it to start immediately. `stop()` asks the OS to stop it.
//! A 5-second polling task reflects OS-reported status in the watch channel.
//!
//! Public API is unchanged from the child-process supervisor: callers see the
//! same `start`/`stop`/`status_receiver` surface.

use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::autostart;

// ─── Public types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuntimeState {
    /// Agent process is running (pid unknown in OS-managed mode).
    Running { pid: u32 },
    /// Agent is not running.
    Stopped,
    /// Waiting before next restart attempt (OS handles this; kept for API compat).
    Restarting { attempt: u32, backoff_secs: u64 },
    /// Too many crashes; OS has backed off (kept for API compat).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRuntimeStatus {
    pub name: String,
    pub state: RuntimeState,
}

// ─── Internal messages ──────────────────────────────────────────────────────

enum Msg {
    Start(String),
    Stop(String),
    Shutdown,
}

// ─── Supervisor handle ──────────────────────────────────────────────────────

/// OS-managed agent supervisor. Clone freely — all copies share the actor.
#[derive(Clone)]
pub struct Supervisor {
    msg_tx: tokio::sync::mpsc::Sender<Msg>,
    status_rx: watch::Receiver<Vec<AgentRuntimeStatus>>,
}

impl Supervisor {
    /// Create the supervisor and start its background actor + status poller.
    pub fn new(mur_home: PathBuf) -> Self {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<Msg>(64);
        let (status_tx, status_rx) = watch::channel::<Vec<AgentRuntimeStatus>>(Vec::new());
        tokio::spawn(os_actor(mur_home, msg_rx, status_tx));
        Supervisor { msg_tx, status_rx }
    }

    pub fn status_receiver(&self) -> watch::Receiver<Vec<AgentRuntimeStatus>> {
        self.status_rx.clone()
    }

    /// Register and start an agent via the OS init system.
    pub async fn start(&self, name: impl Into<String>) {
        let _ = self.msg_tx.send(Msg::Start(name.into())).await;
    }

    /// Stop an agent via the OS init system.
    pub async fn stop(&self, name: impl Into<String>) {
        let _ = self.msg_tx.send(Msg::Stop(name.into())).await;
    }

    /// Shut down the supervisor actor (does not stop running agents — OS owns them).
    pub async fn shutdown(self) {
        let _ = self.msg_tx.send(Msg::Shutdown).await;
    }
}

// ─── Background actor ───────────────────────────────────────────────────────

async fn os_actor(
    mur_home: PathBuf,
    mut rx: tokio::sync::mpsc::Receiver<Msg>,
    status_tx: watch::Sender<Vec<AgentRuntimeStatus>>,
) {
    let mut known: HashSet<String> = HashSet::new();
    let mut poll_interval = tokio::time::interval(Duration::from_secs(5));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                match msg {
                    Msg::Shutdown => break,

                    Msg::Start(slug) => {
                        known.insert(slug.clone());
                        match find_runtime_binary() {
                            Ok(runtime_bin) => {
                                let display_name = slug.clone(); // best-effort; profile not loaded here
                                if let Err(e) = autostart::register(&slug, &display_name, &runtime_bin, &mur_home) {
                                    warn!(agent = %slug, "autostart register failed: {e}");
                                }
                                if let Err(e) = autostart::start_service(&slug) {
                                    warn!(agent = %slug, "autostart start_service failed: {e}");
                                } else {
                                    info!(agent = %slug, "agent service started via OS");
                                }
                            }
                            Err(e) => warn!(agent = %slug, "runtime binary not found: {e}"),
                        }
                        emit_status(&known, &status_tx);
                    }

                    Msg::Stop(slug) => {
                        if let Err(e) = autostart::stop_service(&slug) {
                            warn!(agent = %slug, "autostart stop_service failed: {e}");
                        }
                        known.remove(&slug);
                        emit_status(&known, &status_tx);
                    }
                }
            }

            _ = poll_interval.tick() => {
                if !known.is_empty() {
                    emit_status(&known, &status_tx);
                }
            }
        }
    }
}

fn emit_status(known: &HashSet<String>, tx: &watch::Sender<Vec<AgentRuntimeStatus>>) {
    let mut snapshot: Vec<AgentRuntimeStatus> = known
        .iter()
        .map(|name| {
            let state = if autostart::is_running(name) {
                RuntimeState::Running { pid: 0 }
            } else {
                RuntimeState::Stopped
            };
            AgentRuntimeStatus { name: name.clone(), state }
        })
        .collect();
    snapshot.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = tx.send(snapshot);
}

/// Find the `mur-agent-runtime` binary alongside the current executable.
fn find_runtime_binary() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow::anyhow!("no parent dir for current exe"))?;

    #[cfg(target_os = "windows")]
    let name = "mur-agent-runtime.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "mur-agent-runtime";

    let candidate = dir.join(name);
    if candidate.exists() {
        return Ok(candidate);
    }

    anyhow::bail!("mur-agent-runtime not found alongside Hub binary at {}", dir.display())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_state_serializes_correctly() {
        let running = RuntimeState::Running { pid: 1234 };
        let j = serde_json::to_string(&running).unwrap();
        assert!(j.contains("\"state\":\"running\""), "got: {j}");
        assert!(j.contains("1234"));

        let restarting = RuntimeState::Restarting {
            attempt: 2,
            backoff_secs: 4,
        };
        let j2 = serde_json::to_string(&restarting).unwrap();
        assert!(j2.contains("\"state\":\"restarting\""), "got: {j2}");

        let j3 = serde_json::to_string(&RuntimeState::Failed).unwrap();
        assert!(j3.contains("\"state\":\"failed\""), "got: {j3}");
    }

    #[tokio::test]
    async fn supervisor_start_stop_does_not_panic() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sup = Supervisor::new(dir.path().to_path_buf());
        assert!(sup.status_receiver().borrow().is_empty());

        // With no runtime binary present, start logs a warning and continues.
        sup.start("ghost").await;
        sup.stop("ghost").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        sup.shutdown().await;
    }

    #[tokio::test]
    async fn multiple_agents_tracked_independently() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sup = Supervisor::new(dir.path().to_path_buf());
        sup.start("alpha").await;
        sup.start("beta").await;
        sup.stop("alpha").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        sup.shutdown().await;
    }
}
