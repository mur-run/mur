//! Context API handler — `/api/v1/context`.
//!
//! `/api/v1/ingest` and `/api/v1/feedback` wrote to the local Pattern store
//! and were retired with it (Tier C3).

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::response::IntoResponse;

use crate::context_api;

use super::{AppError, AppState, wrap};

pub(super) async fn context_retrieve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::ContextRequest>,
) -> Result<impl IntoResponse, AppError> {
    let resp = context_api::retrieve(&req, &state.skills_dir(), &state.mur_home())
        .map_err(AppError::Internal)?;
    let count = resp.patterns.len();
    Ok(wrap(resp, count))
}
