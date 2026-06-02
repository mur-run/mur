//! MLX inference sidecar — spawns the bundled `mlx-server` (frozen mlx-lm,
//! OpenAI-compatible) on an ephemeral port and publishes its base URL via the
//! shared file so launchd-managed agents can reach it.

use std::net::TcpListener;

/// Reserve a free localhost TCP port by binding to :0 and reading the assigned
/// port. The listener is dropped immediately; a tiny race window exists before
/// the sidecar binds, which is acceptable here.
pub fn pick_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// OpenAI-compatible base URL for the sidecar on `port`.
pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}

/// Readiness probe URL (returns 200 once the model is loaded).
pub fn health_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1/models")
}

use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::CommandEvent;
use tracing::{info, warn};

/// Start the bundled `mlx-server` sidecar against the bundled model, write its
/// base URL to the shared file, and stream its logs. Idempotent at the
/// application level (call once on setup). Errors are logged, not fatal: if MLX
/// can't start, agents fall back to echo/cloud providers.
pub fn start(app: &AppHandle) {
    let port = match pick_free_port() {
        Ok(p) => p,
        Err(e) => {
            warn!("mlx sidecar: no free port: {e}");
            return;
        }
    };

    // Resolve the bundled model directory from app resources.
    let model_dir = match app
        .path()
        .resolve("models/default", tauri::path::BaseDirectory::Resource)
    {
        Ok(p) => p,
        Err(e) => {
            warn!("mlx sidecar: cannot resolve model resource: {e}");
            return;
        }
    };

    // Publish base URL for launchd-managed agents (Task 1 helper).
    let mur_home = crate::mur_home_path();
    if let Err(e) = mur_common::local_llm::write_base_url(&mur_home, &base_url(port)) {
        warn!("mlx sidecar: failed to write base url: {e}");
    }

    let cmd = app.shell().sidecar("mlx-server").and_then(|c| {
        Ok(c.args([
            "--model",
            model_dir.to_str().unwrap_or_default(),
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ]))
    });
    let cmd = match cmd {
        Ok(c) => c,
        Err(e) => {
            warn!("mlx sidecar: cannot create command: {e}");
            return;
        }
    };

    match cmd.spawn() {
        Ok((mut rx, _child)) => {
            info!(port, "mlx sidecar spawned");
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    if let CommandEvent::Stderr(line) = ev {
                        info!("mlx-server: {}", String::from_utf8_lossy(&line).trim());
                    }
                }
            });
        }
        Err(e) => warn!("mlx sidecar: spawn failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_free_port_is_nonzero() {
        let p = pick_free_port().unwrap();
        assert!(p > 0);
    }

    #[test]
    fn url_helpers_format_correctly() {
        assert_eq!(base_url(50320), "http://127.0.0.1:50320/v1");
        assert_eq!(health_url(50320), "http://127.0.0.1:50320/v1/models");
    }
}
