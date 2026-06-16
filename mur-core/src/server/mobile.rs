//! GET /api/v1/mobile/pair-uri — pairing URI for the Hub QR screen.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use super::{AppError, AppState};

#[derive(Deserialize)]
pub(super) struct PairUriQuery {
    #[serde(default = "default_agent")]
    agent: String,
}

fn default_agent() -> String {
    crate::mobile::DEFAULT_MOBILE_AGENT.to_string()
}

#[derive(Serialize)]
struct PairUriResponse {
    uri: String,
    host: String,
    port: u16,
    window_id: String,
    token: String,
    agent: String,
    /// Seconds until this single-use window expires.
    expires_in: u64,
}

/// Derive `~/.mur` from the server's patterns dir.
fn mur_home(state: &AppState) -> Result<std::path::PathBuf, AppError> {
    state
        .patterns_dir
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("cannot derive mur home from patterns_dir"))
        })
}

pub(super) async fn get_pair_uri(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PairUriQuery>,
) -> Result<impl IntoResponse, AppError> {
    let home = mur_home(&state)?;

    // Mint a fresh single-use enrollment window: the 0600 file holds the token
    // (the daemon recomputes the HMAC proof against it); the plaintext token rides
    // the QR/URI this auth-gated endpoint returns — never on the wire, never mDNS.
    // Canonicalize so the QR's agent + did match what the daemon binds into the
    // proof transcript (it canonicalizes too).
    let agent = crate::a2a_dial::canonicalize_agent_name(&home, &q.agent);
    let did = crate::mobile::daemon_id(&home, &agent).ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "agent \"{agent}\" has no identity yet — start it once before pairing"
        ))
    })?;
    let (window_id, token) =
        crate::mobile::mint_pair_window(&home, &agent).map_err(AppError::Internal)?;
    let port = crate::mobile::mobile_port();
    let host = crate::mobile::lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let uri = crate::mobile::pairing_uri(&host, port, &window_id, &token, &did, &agent);

    Ok(Json(PairUriResponse {
        uri,
        host,
        port,
        window_id,
        token,
        agent,
        expires_in: crate::mobile::pair_window_ttl_secs(),
    }))
}

#[derive(Serialize)]
struct DeviceEntry {
    fingerprint: String,
    pubkey: String,
}

#[derive(Serialize)]
struct DevicesResponse {
    devices: Vec<DeviceEntry>,
}

/// GET /api/v1/mobile/devices — list paired phones (for the Hub "Linked devices"
/// screen). Mirrors `mur agent devices`.
pub(super) async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let home = mur_home(&state)?;
    let devices = crate::mobile::list_paired_devices(&home)
        .into_iter()
        .map(|(pubkey, fingerprint)| DeviceEntry {
            fingerprint,
            pubkey,
        })
        .collect();
    Ok(Json(DevicesResponse { devices }))
}

/// DELETE /api/v1/mobile/devices/{fingerprint} — revoke a paired phone by
/// fingerprint prefix (or full pubkey). Mirrors `mur agent unpair`.
pub(super) async fn unpair_device(
    State(state): State<Arc<AppState>>,
    Path(fingerprint): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let home = mur_home(&state)?;
    match crate::mobile::remove_paired_device(&home, &fingerprint).map_err(AppError::Internal)? {
        Some(pubkey) => Ok(Json(serde_json::json!({ "removed": pubkey }))),
        None => Err(AppError::NotFound(format!(
            "no paired device matches '{fingerprint}'"
        ))),
    }
}
