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
    token: String,
    agent: String,
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

    let token = crate::mobile::ensure_pair_token(&home).map_err(AppError::Internal)?;
    let port = crate::mobile::mobile_port();
    let host = crate::mobile::lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let uri = crate::mobile::pairing_uri(&host, port, &token, &q.agent);

    Ok(Json(PairUriResponse {
        uri,
        host,
        port,
        token,
        agent: q.agent,
    }))
}
