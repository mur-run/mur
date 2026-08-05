//! HTTP server for receiving commander signal batches.
//!
//! Binds to `127.0.0.1:9421` (configurable via `MUR_SIGNAL_PORT`).
//! `POST /v1/signals/batch` — bearer-token auth, writes signals to `~/.mur/inbox/`.

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use mur_common::{Signal, SignalBatch, SignalBatchResponse};
use mur_core::sync::inbox::Inbox;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const DEFAULT_PORT: u16 = 9421;
const TOKEN_FILE: &str = "secrets/commander-token";

#[derive(Clone)]
struct AppState {
    token: String,
    seen_batch_ids: Arc<Mutex<HashSet<String>>>,
}

/// Generate or read the daemon bearer token at `~/.mur/secrets/commander-token`.
/// Creates the file (and parent dir) if it doesn't exist.
pub fn ensure_token() -> Result<String> {
    let path = token_path()?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    if path.exists() {
        let t =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        return Ok(t.trim().to_owned());
    }
    let token = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, &token)?;
    // Restrict permissions on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Spawn the HTTP signal server as a background tokio task.
pub fn spawn(token: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run_server(token).await {
            eprintln!("murmurd signal-server error: {e:#}");
        }
    })
}

async fn run_server(token: String) -> Result<()> {
    let port: u16 = std::env::var("MUR_SIGNAL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let state = AppState {
        token,
        seen_batch_ids: Arc::new(Mutex::new(HashSet::new())),
    };
    let app = Router::new()
        .route("/v1/signals/batch", post(handle_batch))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind signal server to {addr}"))?;
    eprintln!("murmurd signal-server listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(batch): Json<SignalBatch>,
) -> impl IntoResponse {
    if !check_bearer(&state.token, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    if batch.schema_version != mur_common::SIGNAL_SCHEMA_VERSION {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "schema_version_mismatch"})),
        )
            .into_response();
    }

    // Batch-level dedup.
    let batch_key = batch.batch_id.to_string();
    {
        let mut seen = state.seen_batch_ids.lock().unwrap();
        if seen.contains(&batch_key) {
            let resp = SignalBatchResponse {
                accepted: 0,
                deduplicated: batch.signals.len(),
            };
            return (StatusCode::OK, Json(serde_json::json!({"accepted": resp.accepted, "deduplicated": resp.deduplicated}))).into_response();
        }
        seen.insert(batch_key);
    }

    // Write signals to inbox.
    let (accepted, deduplicated) = write_to_inbox(&batch.signals);
    let resp = SignalBatchResponse {
        accepted,
        deduplicated,
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({"accepted": resp.accepted, "deduplicated": resp.deduplicated})),
    )
        .into_response()
}

fn write_to_inbox(signals: &[Signal]) -> (usize, usize) {
    let inbox = match Inbox::default_location() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("murmurd signal-server: inbox open error: {e:#}");
            return (0, 0);
        }
    };
    let mut accepted = 0usize;
    let mut deduplicated = 0usize;
    for signal in signals {
        // Wire provenance: the batch arrived bearer-authed, so drop it into
        // the inbox's wire/ subdirectory — apply_all exempts that dir from
        // MUR_SIGNAL_REQUIRE_SIG (frozen wire v1 signals are unsigned).
        let dest = inbox
            .wire_dir()
            .join(mur_core::sync::inbox::signal_file_name(signal));
        if dest.exists() {
            deduplicated += 1;
            continue;
        }
        if let Err(e) = inbox.receive_wire(signal) {
            eprintln!("murmurd signal-server: inbox receive error: {e:#}");
        } else {
            accepted += 1;
        }
    }
    (accepted, deduplicated)
}

fn check_bearer(expected: &str, headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == expected)
}

fn token_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(home.join(".mur").join(TOKEN_FILE))
}
