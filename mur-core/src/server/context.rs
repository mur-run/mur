//! Context API handlers — `/api/v1/{context,ingest,feedback}`.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::context_api;
use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::vector::LanceDbStore as VectorStore;

use super::{AppError, AppState, notify, wrap};

pub(super) async fn context_retrieve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::ContextRequest>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;

    // Attempt hybrid (vector + keyword) scoring when the LanceDB index exists.
    // Falls back to keyword-only when the index is absent or embedding fails.
    let vector_scores = if state.index_dir.exists() {
        match load_config() {
            Ok(cfg) => {
                let emb_cfg = EmbeddingConfig::from_config(&cfg);
                match embed(&req.query, &emb_cfg).await {
                    Ok(query_embedding) => {
                        match VectorStore::open(&state.index_dir, cfg.embedding.dimensions as i32)
                            .await
                        {
                            Ok(vs) => {
                                vs.search(&query_embedding, 20, None)
                                    .await
                                    .ok()
                                    .map(|results| {
                                        results
                                            .into_iter()
                                            .map(|r| (r.name, r.similarity as f64))
                                            .collect::<std::collections::HashMap<_, _>>()
                                    })
                            }
                            Err(_) => None,
                        }
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let resp =
        context_api::retrieve(&req, &store, vector_scores.as_ref()).map_err(AppError::Internal)?;
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(resp, count))
}

pub(super) async fn context_ingest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::IngestRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    let resp = context_api::ingest(&req, &store).map_err(AppError::Internal)?;
    notify(&state, "pattern:ingested", &resp.pattern_id);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(resp, count)))
}

pub(super) async fn context_feedback(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::FeedbackRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;

    // Check pattern exists first for a clear 404
    store
        .get(&req.pattern_id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", req.pattern_id)))?;

    context_api::submit_feedback(&req, &store).map_err(AppError::Internal)?;
    notify(&state, "pattern:feedback", &req.pattern_id);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(serde_json::json!({"ok": true}), count))
}
