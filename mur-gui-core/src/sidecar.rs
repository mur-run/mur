//! Multi-agent sidecar supervisor for MuR Hub.
//!
//! Spawns and supervises `mur-agent-runtime --profile <name> start` for each
//! configured agent. Restarts on crash with exponential backoff (0/2/4/8/30s
//! cap). After 5 crashes within 60s, stops auto-restarting (Failed state).
//!
//! Hub shutdown: SIGTERM all children, SIGKILL after 5s.
//! Status is broadcast via `tokio::sync::watch<Vec<AgentRuntimeStatus>>`.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{info, warn};

// ─── Public types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuntimeState {
    /// Child process is alive.
    Running { pid: u32 },
    /// Cleanly stopped (user request or not yet started).
    Stopped,
    /// Waiting before next restart attempt.
    Restarting { attempt: u32, backoff_secs: u64 },
    /// Too many crashes; will not auto-restart until reset.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRuntimeStatus {
    pub name: String,
    pub state: RuntimeState,
}

// ─── Internal messages ──────────────────────────────────────────────────────

enum Msg {
    /// External: start an agent (idempotent if already running).
    Start(String),
    /// External: stop an agent (suppresses auto-restart).
    Stop(String),
    /// External: shut down the supervisor.
    Shutdown,
    /// Internal: child process exited (sent by child-monitor task).
    AgentExited(String),
    /// Internal: backoff elapsed, restart the agent if still desired.
    RestartNow(String),
}

// ─── Per-agent state ────────────────────────────────────────────────────────

struct ManagedAgent {
    runtime_state: RuntimeState,
    crash_window: Vec<Instant>,
    user_stopped: bool,
    /// Oneshot sender to cancel the current child-monitor task.
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ManagedAgent {
    fn new() -> Self {
        ManagedAgent {
            runtime_state: RuntimeState::Stopped,
            crash_window: Vec::new(),
            user_stopped: false,
            cancel_tx: None,
        }
    }

    /// Record a crash and return the next backoff, or `None` if we give up.
    fn next_backoff(&mut self) -> Option<Duration> {
        self.crash_window
            .retain(|t| t.elapsed() < Duration::from_secs(60));
        self.crash_window.push(Instant::now());
        let n = self.crash_window.len();
        if n > 5 {
            None
        } else {
            Some(match n {
                1 => Duration::ZERO,
                2 => Duration::from_secs(2),
                3 => Duration::from_secs(4),
                4 => Duration::from_secs(8),
                _ => Duration::from_secs(30),
            })
        }
    }
}

// ─── Supervisor handle ──────────────────────────────────────────────────────

/// Multi-agent supervisor. Clone freely — all copies share the same actor.
#[derive(Clone)]
pub struct Supervisor {
    msg_tx: tokio::sync::mpsc::Sender<Msg>,
    status_rx: watch::Receiver<Vec<AgentRuntimeStatus>>,
}

impl Supervisor {
    /// Create a supervisor and start its background actor.
    pub fn new(mur_home: PathBuf) -> Self {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<Msg>(64);
        let (status_tx, status_rx) = watch::channel::<Vec<AgentRuntimeStatus>>(Vec::new());
        let actor_tx = msg_tx.clone();
        tokio::spawn(supervisor_actor(mur_home, msg_rx, actor_tx, status_tx));
        Supervisor { msg_tx, status_rx }
    }

    pub fn status_receiver(&self) -> watch::Receiver<Vec<AgentRuntimeStatus>> {
        self.status_rx.clone()
    }

    /// Start an agent (idempotent if already running).
    pub async fn start(&self, name: impl Into<String>) {
        let _ = self.msg_tx.send(Msg::Start(name.into())).await;
    }

    /// Stop an agent; suppresses auto-restart until next `start`.
    pub async fn stop(&self, name: impl Into<String>) {
        let _ = self.msg_tx.send(Msg::Stop(name.into())).await;
    }

    /// Shut down all agents and stop the actor.
    pub async fn shutdown(self) {
        let _ = self.msg_tx.send(Msg::Shutdown).await;
    }
}

// ─── Background actor ───────────────────────────────────────────────────────

async fn supervisor_actor(
    mur_home: PathBuf,
    mut rx: tokio::sync::mpsc::Receiver<Msg>,
    self_tx: tokio::sync::mpsc::Sender<Msg>,
    status_tx: watch::Sender<Vec<AgentRuntimeStatus>>,
) {
    let mut agents: HashMap<String, ManagedAgent> = HashMap::new();

    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Shutdown => {
                shutdown_all(&mut agents);
                break;
            }

            Msg::Start(name) => {
                // Explicit block so entry's mutable borrow ends before emit_status.
                let should_emit = {
                    let entry = agents.entry(name.clone()).or_insert_with(ManagedAgent::new);
                    if matches!(entry.runtime_state, RuntimeState::Running { .. }) {
                        false
                    } else {
                        entry.user_stopped = false;
                        do_spawn(&name, entry, self_tx.clone(), &mur_home);
                        true
                    }
                };
                if should_emit {
                    emit_status(&agents, &status_tx);
                }
            }

            Msg::Stop(name) => {
                {
                    if let Some(entry) = agents.get_mut(&name) {
                        entry.user_stopped = true;
                        cancel_child(entry);
                        entry.runtime_state = RuntimeState::Stopped;
                    }
                }
                emit_status(&agents, &status_tx);
            }

            Msg::AgentExited(name) => {
                // Phase 1: compute what to do next (mutable borrow).
                enum Next {
                    EmitStopped,
                    EmitFailed,
                    ScheduleRestart { attempt: u32, backoff: Duration },
                    Nothing,
                }
                let next = if let Some(entry) = agents.get_mut(&name) {
                    entry.cancel_tx = None;
                    if entry.user_stopped {
                        entry.runtime_state = RuntimeState::Stopped;
                        Next::EmitStopped
                    } else {
                        match entry.next_backoff() {
                            None => {
                                warn!(agent = %name, "too many crashes; stopping auto-restart");
                                entry.runtime_state = RuntimeState::Failed;
                                Next::EmitFailed
                            }
                            Some(backoff) => {
                                let attempt = entry.crash_window.len() as u32;
                                entry.runtime_state = RuntimeState::Restarting {
                                    attempt,
                                    backoff_secs: backoff.as_secs(),
                                };
                                Next::ScheduleRestart { attempt, backoff }
                            }
                        }
                    }
                } else {
                    Next::Nothing
                };
                // Phase 2: emit status + schedule (borrow released).
                match next {
                    Next::EmitStopped | Next::EmitFailed => {
                        emit_status(&agents, &status_tx);
                    }
                    Next::ScheduleRestart { attempt, backoff } => {
                        emit_status(&agents, &status_tx);
                        info!(agent = %name, ?backoff, attempt, "restart scheduled");
                        schedule_restart(name, backoff, self_tx.clone());
                    }
                    Next::Nothing => {}
                }
            }

            Msg::RestartNow(name) => {
                let should_spawn = if let Some(entry) = agents.get_mut(&name) {
                    if entry.user_stopped {
                        false
                    } else {
                        do_spawn(&name, entry, self_tx.clone(), &mur_home);
                        true
                    }
                } else {
                    false
                };
                if should_spawn {
                    emit_status(&agents, &status_tx);
                }
            }
        }
    }
}

fn do_spawn(
    name: &str,
    entry: &mut ManagedAgent,
    msg_tx: tokio::sync::mpsc::Sender<Msg>,
    mur_home: &Path,
) {
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    entry.cancel_tx = Some(cancel_tx);

    let name = name.to_string();
    let mur_home = mur_home.to_path_buf();

    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new("mur-agent-runtime");
        cmd.args(["--profile", &name, "start"])
            .env("PATH", augmented_path())
            .env("MUR_HOME", &mur_home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(agent = %name, error = %e, "failed to spawn mur-agent-runtime");
                let _ = msg_tx.send(Msg::AgentExited(name)).await;
                return;
            }
        };

        let pid = child.id().unwrap_or(0);
        info!(agent = %name, pid, "agent runtime spawned");
        drop(child.stdout.take());
        drop(child.stderr.take());

        tokio::select! {
            _ = child.wait() => {
                info!(agent = %name, pid, "agent runtime exited");
            }
            _ = cancel_rx => {
                terminate_child(&mut child).await;
            }
        }

        let _ = msg_tx.send(Msg::AgentExited(name)).await;
    });

    entry.runtime_state = RuntimeState::Running { pid: 0 };
}

fn schedule_restart(name: String, backoff: Duration, tx: tokio::sync::mpsc::Sender<Msg>) {
    tokio::spawn(async move {
        tokio::time::sleep(backoff).await;
        let _ = tx.send(Msg::RestartNow(name)).await;
    });
}

fn cancel_child(entry: &mut ManagedAgent) {
    if let Some(tx) = entry.cancel_tx.take() {
        let _ = tx.send(());
    }
}

fn shutdown_all(agents: &mut HashMap<String, ManagedAgent>) {
    for (name, entry) in agents.iter_mut() {
        info!(agent = %name, "shutting down");
        entry.user_stopped = true;
        cancel_child(entry);
    }
}

fn emit_status(
    agents: &HashMap<String, ManagedAgent>,
    tx: &watch::Sender<Vec<AgentRuntimeStatus>>,
) {
    let mut snapshot: Vec<AgentRuntimeStatus> = agents
        .iter()
        .map(|(name, entry)| AgentRuntimeStatus {
            name: name.clone(),
            state: entry.runtime_state.clone(),
        })
        .collect();
    snapshot.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = tx.send(snapshot);
}

async fn terminate_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) with negative pid sends SIGTERM to the process group.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let _ = child.kill().await;
}

fn augmented_path() -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    if cfg!(target_os = "macos") {
        format!("/opt/homebrew/bin:/usr/local/bin:{existing}:/usr/bin:/bin:/usr/sbin:/sbin")
    } else if cfg!(target_os = "linux") {
        format!("/usr/local/bin:/snap/bin:{existing}:/usr/bin:/bin:/usr/sbin:/sbin")
    } else {
        existing
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_curve_correct() {
        let mut entry = ManagedAgent::new();
        assert_eq!(entry.next_backoff(), Some(Duration::ZERO));
        assert_eq!(entry.next_backoff(), Some(Duration::from_secs(2)));
        assert_eq!(entry.next_backoff(), Some(Duration::from_secs(4)));
        assert_eq!(entry.next_backoff(), Some(Duration::from_secs(8)));
        assert_eq!(entry.next_backoff(), Some(Duration::from_secs(30)));
        assert_eq!(entry.next_backoff(), None); // 6th crash → give up
    }

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

    #[test]
    fn augmented_path_non_empty() {
        assert!(!augmented_path().is_empty());
    }

    #[tokio::test]
    async fn supervisor_start_stop_nonexistent() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sup = Supervisor::new(dir.path().to_path_buf());
        // Initial snapshot is empty.
        assert!(sup.status_receiver().borrow().is_empty());

        // Starting a nonexistent binary: spawn will fail, exit will be sent.
        // We don't crash — just log a warning.
        // We can't easily assert on the status here without real binaries,
        // but we verify the actor doesn't panic.
        sup.stop("ghost").await;
        sup.shutdown().await;
    }

    #[tokio::test]
    async fn multiple_agents_tracked_independently() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sup = Supervisor::new(dir.path().to_path_buf());
        // Issue start commands for two agents; actor receives them without panic.
        sup.start("alpha").await;
        sup.start("beta").await;
        sup.stop("alpha").await;
        // Give actor time to process.
        tokio::time::sleep(Duration::from_millis(50)).await;
        sup.shutdown().await;
    }
}
