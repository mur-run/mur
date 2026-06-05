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
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::oneshot;
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
    /// Start an agent and report back whether it actually launched, so the
    /// caller (and ultimately the Hub UI) can surface failures instead of
    /// swallowing them into the log.
    Start(String, oneshot::Sender<Result<(), String>>),
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
    ///
    /// Returns `Err` with a human-readable reason if the agent could not be
    /// launched (e.g. the runtime binary is missing or the service refused to
    /// start), so callers can surface it to the user instead of failing silently.
    pub async fn start(&self, name: impl Into<String>) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(Msg::Start(name.into(), reply_tx))
            .await
            .map_err(|_| "supervisor is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "supervisor dropped the start request".to_string())?
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

                    Msg::Start(slug, reply) => {
                        known.insert(slug.clone());
                        let result = start_agent_service(&slug, &mur_home);
                        match &result {
                            Ok(()) => info!(agent = %slug, "agent service started via OS"),
                            Err(e) => warn!(agent = %slug, "start failed: {e}"),
                        }
                        emit_status(&known, &status_tx);
                        let _ = reply.send(result);
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
            AgentRuntimeStatus {
                name: name.clone(),
                state,
            }
        })
        .collect();
    snapshot.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = tx.send(snapshot);
}

/// Register and start an agent's OS service, returning a human-readable error
/// on the first failing step so the Hub UI can surface it to the user (C3).
fn start_agent_service(slug: &str, mur_home: &Path) -> Result<(), String> {
    let runtime_bin = find_runtime_binary().map_err(|e| {
        format!(
            "agent runtime not found ({e}). Reinstall MUR so mur-agent-runtime \
             is available, or run build.sh to install it."
        )
    })?;
    let display_name = slug.to_string(); // best-effort; profile not loaded here
    autostart::register(slug, &display_name, &runtime_bin, mur_home)
        .map_err(|e| format!("could not register the agent service: {e}"))?;
    autostart::start_service(slug)
        .map_err(|e| format!("the agent service failed to start: {e}"))?;
    Ok(())
}

/// A non-empty regular file. The `binaries/` directory ships zero-byte
/// `externalBin` placeholders in git that are only filled in at release time;
/// spawning one fails, so a candidate must have real bytes to count.
fn is_real_binary(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Locate the `mur-agent-runtime` binary across the locations it can live in:
/// an explicit override, alongside the Hub binary (dev builds), inside the
/// macOS `.app` bundle's Resources, or on `PATH` (installed next to `mur`).
/// Zero-byte placeholders are skipped so we never spawn a broken stub (C2).
fn find_runtime_binary() -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let name = "mur-agent-runtime.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "mur-agent-runtime";

    // Explicit override wins, matching mur-core's resolver.
    if let Some(v) = std::env::var_os("MUR_AGENT_RUNTIME_BIN") {
        let p = PathBuf::from(v);
        if is_real_binary(&p) {
            return Ok(p);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(name)); // dev build / simple install
        // macOS .app: Contents/MacOS/<exe> → Contents/Resources/binaries/<name>
        candidates.push(dir.join("../Resources/binaries").join(name));
        candidates.push(dir.join("../Resources").join(name));
    }
    // Anywhere on PATH (e.g. installed next to the `mur` CLI).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            candidates.push(dir.join(name));
        }
    }

    for c in &candidates {
        if is_real_binary(c) {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "mur-agent-runtime not found (looked alongside the Hub, in the app bundle, and on PATH)"
    )
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

        // With no runtime binary present, start returns Err but the agent is
        // still tracked (it was inserted before the launch attempt).
        let _ = sup.start("ghost").await;
        sup.stop("ghost").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        sup.shutdown().await;
    }

    #[tokio::test]
    async fn multiple_agents_tracked_independently() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sup = Supervisor::new(dir.path().to_path_buf());
        let _ = sup.start("alpha").await;
        let _ = sup.start("beta").await;
        sup.stop("alpha").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        sup.shutdown().await;
    }
}
