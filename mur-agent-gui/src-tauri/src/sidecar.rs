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

        // Resolve the active model's secret synchronously (we're already
        // inside Tauri's runtime; block_on on the helper is fine).
        let secret_envs =
            tauri::async_runtime::block_on(resolve_secrets_for_agent(agent_name));
        for (k, _) in &secret_envs {
            info!(env_key = %k, "injecting resolved secret into sidecar env");
        }

        let mut cmd = app
            .shell()
            .sidecar("mur-agent-runtime")
            .context("create sidecar command (mur-agent-runtime)")?
            .args(["--profile", agent_name])
            .env("PATH", augmented_path())
            .env("MUR_GUI_AGENT_NAME", agent_name);
        for (k, v) in secret_envs {
            cmd = cmd.env(k, v);
        }

        let (mut rx, child) = cmd.spawn().context("spawn sidecar")?;
        info!(pid = child.pid(), agent = %agent_name, "sidecar spawned");

        let app_clone = app.clone();
        let agent_name_clone = agent_name.to_string();
        // Drain stdout/stderr in a background task; emit events to the
        // webview so the Logs window can subscribe. Restart-on-exit is
        // also handled here.
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(line) => {
                        let _ = app_clone
                            .emit("sidecar:stdout", String::from_utf8_lossy(&line).to_string());
                    }
                    CommandEvent::Stderr(line) => {
                        let _ = app_clone
                            .emit("sidecar:stderr", String::from_utf8_lossy(&line).to_string());
                    }
                    CommandEvent::Error(msg) => {
                        warn!("sidecar error: {msg}");
                        let _ = app_clone.emit("sidecar:error", msg);
                    }
                    CommandEvent::Terminated(payload) => {
                        info!(?payload, "sidecar terminated");
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
    ///
    /// Releases the inner Mutex before the 2s drain sleep so concurrent
    /// `start`/`pid`/`handle_exit` callers don't block. The taken
    /// `CommandChild` handles its own SIGKILL on drop if we never get
    /// to it.
    pub fn stop(&self) -> Result<()> {
        // Take the child (and set user_stopped) under the lock, then
        // release before the long-running drain sleep.
        let child = {
            let mut state = self.inner.lock().unwrap();
            state.user_stopped = true;
            state.child.take()
        };

        let Some(child) = child else { return Ok(()) };

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
            // Give the runtime up to 2s to clean up; then SIGKILL.
            std::thread::sleep(Duration::from_secs(2));
            let _ = child.kill();
        }
        #[cfg(not(unix))]
        {
            let _ = child.kill();
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
            state
                .crash_window
                .retain(|t| t.elapsed() < Duration::from_secs(60));
            state.crash_window.push(Instant::now());
            let n = state.crash_window.len();
            if n > 5 {
                warn!(
                    crashes = n,
                    window_secs = 60,
                    "sidecar crashed too often; stop auto-restart"
                );
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
        info!(?backoff, "sidecar restart scheduled");
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
        self.inner.lock().unwrap().child.as_ref().map(|c| c.pid())
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

/// Look up the agent's `model_ref`, find the matching registry entry, resolve
/// its `SecretRef` if any, and return `(env_var_name, value)` pairs to inject
/// into the sidecar's environment. Errors are swallowed at every step — the
/// sidecar still launches without secrets, falling back to its own `from_env`
/// path or an echo runner.
async fn resolve_secrets_for_agent(agent_name: &str) -> Vec<(String, String)> {
    use mur_common::agent::AgentProfile;
    use mur_common::model::ModelRegistry;
    use secrecy::ExposeSecret;

    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    let pyaml = home.join(format!(".mur/agents/{agent_name}/profile.yaml"));
    let Ok(body) = std::fs::read_to_string(&pyaml) else {
        return vec![];
    };
    let profile: AgentProfile = match serde_yaml_ng::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, agent = %agent_name, "parse profile.yaml");
            return vec![];
        }
    };
    let Some(model_ref) = profile.model_ref else {
        return vec![];
    };
    let reg_path = match ModelRegistry::default_path() {
        Ok(p) => p,
        Err(_) => return vec![],
    };
    let reg = match ModelRegistry::load_from(&reg_path) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "load model registry");
            return vec![];
        }
    };
    let Some(entry) = reg.models.get(&model_ref) else {
        warn!(model_ref = %model_ref, "model_ref not in registry");
        return vec![];
    };
    let Some(secret) = entry.secret.as_ref() else {
        return vec![];
    };
    let resolved = match secret.resolve().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "resolve secret for sidecar");
            return vec![];
        }
    };
    let env_var = match entry.provider.as_str() {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        _ => return vec![],
    };
    vec![(env_var.to_string(), resolved.expose_secret().to_string())]
}
