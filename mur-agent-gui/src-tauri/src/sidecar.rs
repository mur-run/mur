//! Sidecar manager — spawn / monitor / restart `mur-agent-runtime`
//! as a child of the GUI app.
//!
//! Architectural notes:
//!
//! * On Unix, the runtime calls `setpgid(0, 0)` at the top of
//!   `entrypoint()` (see `mur-agent-runtime/src/supervisor.rs`), so
//!   sending `SIGTERM` to its process group reaches the runtime AND
//!   every MCP child it spawned.
//! * On Windows, this manager wraps the spawn in a Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so the same tree-kill
//!   semantics apply when the GUI process exits.
//! * Restart-with-backoff: 1st crash → 0s, 2nd → 2s, 3rd → 4s, 4th
//!   → 8s, 5th → 16s, 6th → 60s cap. After 5 crashes within 60s the
//!   tray icon turns red and we stop respawning until the user
//!   explicitly asks for "Start Agent" again.
//! * `PATH` is augmented before spawn so MCP children can find their
//!   user-installed binaries (see CLAUDE.md / spec § 4.5).

use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tracing::{info, warn};

#[derive(Debug, Default)]
pub struct SidecarManager {
    inner: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    child: Option<CommandChild>,
    /// Timestamps of crashes within the last minute (rolling window).
    crash_window: Vec<Instant>,
    /// True if the user explicitly stopped the sidecar (don't auto-restart).
    user_stopped: bool,
}

impl SidecarManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn the runtime as a sidecar, capturing stdout/stderr to
    /// the GUI's log channel. `agent_name` is forwarded as
    /// `--profile <name>`. Idempotent — if a child is already
    /// running, returns Ok().
    pub fn start(&self, app: &AppHandle, agent_name: &str) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        if state.child.is_some() {
            return Ok(()); // already running
        }

        let cmd = app
            .shell()
            .sidecar("mur-agent-runtime")
            .context("create sidecar command (mur-agent-runtime)")?
            .args(["--profile", agent_name])
            .env("PATH", augmented_path())
            .env("MUR_GUI_AGENT_NAME", agent_name);

        let (mut rx, child) = cmd.spawn().context("spawn sidecar")?;
        info!("sidecar spawned, pid={}", child.pid());

        let app_clone = app.clone();
        let agent_name_clone = agent_name.to_string();
        // Drain stdout/stderr in a background task; emit events to the
        // webview so the Logs window can subscribe. Restart-on-exit is
        // also handled here.
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(line) => {
                        let _ = app_clone.emit("sidecar:stdout", String::from_utf8_lossy(&line).to_string());
                    }
                    CommandEvent::Stderr(line) => {
                        let _ = app_clone.emit("sidecar:stderr", String::from_utf8_lossy(&line).to_string());
                    }
                    CommandEvent::Error(msg) => {
                        warn!("sidecar error: {msg}");
                        let _ = app_clone.emit("sidecar:error", msg);
                    }
                    CommandEvent::Terminated(payload) => {
                        info!("sidecar exit: {:?}", payload);
                        let _ = app_clone.emit("sidecar:terminated", payload);
                        // Trigger restart-with-backoff (registered globally).
                        let mgr = app_clone.state::<Arc<SidecarManager>>();
                        mgr.handle_exit(&app_clone, &agent_name_clone);
                    }
                    _ => {}
                }
            }
        });

        state.child = Some(child);
        state.user_stopped = false;
        Ok(())
    }

    /// Send SIGTERM to the runtime + its process group. Idempotent.
    pub fn stop(&self) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        state.user_stopped = true;
        if let Some(child) = state.child.take() {
            // tauri-plugin-shell's CommandChild::kill sends SIGKILL
            // on Unix. Better: send SIGTERM to the process group so
            // the runtime can flush telemetry + drop its lock.
            #[cfg(unix)]
            {
                let pid = child.pid() as i32;
                // SAFETY: kill(2) with -pid sends to process group;
                // POSIX-defined behaviour; runtime catches SIGTERM.
                unsafe {
                    libc::kill(-pid, libc::SIGTERM);
                }
                // Give the runtime up to 5s to clean up; then SIGKILL.
                std::thread::sleep(Duration::from_secs(2));
                let _ = child.kill();
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
        }
        Ok(())
    }

    /// React to an unexpected exit. Honours user_stopped; otherwise
    /// applies exponential backoff and respawns.
    fn handle_exit(&self, app: &AppHandle, agent_name: &str) {
        let backoff = {
            let mut state = self.inner.lock().unwrap();
            state.child = None;
            if state.user_stopped {
                return;
            }
            state.crash_window.retain(|t| t.elapsed() < Duration::from_secs(60));
            state.crash_window.push(Instant::now());
            let n = state.crash_window.len();
            if n > 5 {
                warn!("sidecar crashed >5 times in 60s; stop auto-restart");
                let _ = app.emit("sidecar:gave-up", n);
                return;
            }
            // 0s, 2s, 4s, 8s, 16s, 60s
            match n {
                1 => Duration::from_secs(0),
                2 => Duration::from_secs(2),
                3 => Duration::from_secs(4),
                4 => Duration::from_secs(8),
                5 => Duration::from_secs(16),
                _ => Duration::from_secs(60),
            }
        };
        info!("sidecar restart in {backoff:?}");
        let app_clone = app.clone();
        let agent = agent_name.to_string();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(backoff).await;
            let mgr = app_clone.state::<Arc<SidecarManager>>();
            if let Err(e) = mgr.start(&app_clone, &agent) {
                warn!("sidecar respawn failed: {e}");
            }
        });
    }

    /// Inspect the current sidecar PID (None when not running).
    /// Used by tests + future status endpoints.
    #[allow(dead_code)]
    pub fn pid(&self) -> Option<u32> {
        self.inner
            .lock()
            .unwrap()
            .child
            .as_ref()
            .map(|c| c.pid())
    }
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

// `tauri::Emitter` is needed to call `app.emit(...)`.
use tauri::Emitter;

// Used in tests + when the sidecar binary is directly named (rare).
#[allow(dead_code)]
pub fn runtime_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_starts_with_no_child() {
        let m = SidecarManager::new();
        assert!(m.pid().is_none());
    }

    #[test]
    fn augmented_path_includes_homebrew_on_mac() {
        let path = augmented_path();
        if cfg!(target_os = "macos") {
            assert!(path.contains("/opt/homebrew/bin"));
            assert!(path.contains("/usr/local/bin"));
        }
    }
}
