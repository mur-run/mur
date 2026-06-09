//! MLX inference sidecar — spawns the bundled `mlx-server` (frozen mlx-vlm,
//! OpenAI-compatible; serves both text and vision) on an ephemeral port and
//! publishes its base URL via the shared file so launchd-managed agents can
//! reach it.

use std::net::TcpListener;
use std::time::Duration;

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

/// Whether the bundled MLX model directory is present, i.e. local in-process
/// inference is possible. Used to decide whether the built-in concierge needs a
/// fallback model so it can actually respond on machines without the bundle.
pub fn model_available(app: &AppHandle) -> bool {
    ["resources/models/default", "models/default"]
        .iter()
        .filter_map(|rel| {
            app.path()
                .resolve(rel, tauri::path::BaseDirectory::Resource)
                .ok()
        })
        .any(|p| p.is_dir())
}

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

    // Resolve the bundled model directory from app resources. Tauri stages the
    // `resources/**` bundle glob under `<Resource>/resources/...`, so resolve that
    // path first and fall back to the bare name for dev / alternate layouts.
    let model_dir = match ["resources/models/default", "models/default"]
        .iter()
        .filter_map(|rel| {
            app.path()
                .resolve(rel, tauri::path::BaseDirectory::Resource)
                .ok()
        })
        .find(|p| p.is_dir())
    {
        Some(p) => p,
        None => {
            warn!(
                "mlx sidecar: bundled model resource not found (resources/models/default); \
                 skipping local inference"
            );
            return;
        }
    };

    let mur_home = crate::mur_home_path();

    // The bundled model dir must be valid UTF-8 to pass as a CLI arg. Fail soft
    // (matching the is_dir() guard above) rather than spawning with `--model ""`,
    // which would be a silent misconfiguration that's hard to diagnose downstream.
    let model_arg = match model_dir.to_str() {
        Some(s) => s.to_string(),
        None => {
            warn!(
                "mlx sidecar: model path is not valid UTF-8 ({}); skipping local inference",
                model_dir.display()
            );
            return;
        }
    };

    let cmd = app.shell().sidecar("mlx-server").map(|c| {
        c.args([
            "--model",
            &model_arg,
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
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
            info!(port, "mlx sidecar spawned; waiting for readiness");
            // Stream the sidecar's stderr to our logs.
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    if let CommandEvent::Stderr(line) = ev {
                        info!("mlx-server: {}", String::from_utf8_lossy(&line).trim());
                    }
                }
            });
            // Publish the base URL only once the server actually accepts
            // connections. uvicorn binds/accepts only AFTER its lifespan startup
            // (which loads the model) succeeds, so a successful connect is a true
            // readiness signal — and if the frozen binary crashes during model
            // load it never accepts, so we never publish a dead URL that agents
            // would fall against with no fallback.
            tauri::async_runtime::spawn(async move {
                if wait_until_ready(port, MLX_READINESS_TIMEOUT).await {
                    match mur_common::local_llm::write_base_url(&mur_home, &base_url(port)) {
                        Ok(()) => info!(port, "mlx sidecar ready; base url published"),
                        Err(e) => warn!("mlx sidecar: failed to write base url: {e}"),
                    }
                } else {
                    warn!(
                        port,
                        "mlx sidecar did not accept connections within timeout; \
                         not publishing base url (agents fall back to echo/cloud)"
                    );
                }
            });
        }
        Err(e) => warn!("mlx sidecar: spawn failed: {e}"),
    }
}

/// How long to wait for the sidecar to start accepting connections before
/// giving up and leaving the base URL unpublished.
const MLX_READINESS_TIMEOUT: Duration = Duration::from_secs(180);
/// Interval between readiness probes.
const MLX_READINESS_POLL: Duration = Duration::from_millis(500);

/// Poll until the sidecar accepts a TCP connection on `port`, or `timeout`
/// elapses (returns false). uvicorn only begins accepting connections after its
/// lifespan startup — which loads the model — completes, so a successful connect
/// doubles as a model-ready signal.
async fn wait_until_ready(port: u16, timeout: Duration) -> bool {
    use tokio::net::TcpStream;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_ok()
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(MLX_READINESS_POLL).await;
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
