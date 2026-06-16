//! GET /api/v1/mobile/pair-uri — pairing URI for the Hub QR screen.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
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

pub(super) async fn get_pair_uri(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PairUriQuery>,
) -> Result<impl IntoResponse, AppError> {
    let home = state
        .patterns_dir
        .parent()
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("cannot derive mur home from patterns_dir"))
        })?
        .to_path_buf();

    // Mint a fresh single-use enrollment window (writes only the token hash to a
    // 0600 file); the plaintext token rides the QR/URI this auth-gated endpoint
    // returns — never to mDNS, never to disk.
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
