//! Search handler — `POST /api/v1/search`. The shared `SearchRequest` /
//! `SearchResult` types are also used by `workflows::search_workflows`.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::retrieve::scoring::{ScoredPattern, score_and_rank};

use super::{AppError, AppState, wrap};

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Serialize)]
pub(super) struct SearchResult {
    pub name: String,
    pub description: String,
    pub score: f64,
    pub relevance: f64,
    pub tier: String,
    pub maturity: String,
    pub confidence: f64,
}

pub(super) async fn search_patterns(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let patterns = store.list_all().map_err(AppError::Internal)?;
    let count = patterns.len();

    let scored: Vec<ScoredPattern> = score_and_rank(&req.query, patterns);

    let results: Vec<SearchResult> = scored
        .into_iter()
        .take(req.limit)
        .map(|sp| SearchResult {
            name: sp.item.name.clone(),
            description: sp.item.description.clone(),
            score: sp.score,
            relevance: sp.relevance,
            tier: format!("{:?}", sp.item.tier).to_lowercase(),
            maturity: format!("{:?}", sp.item.maturity).to_lowercase(),
            confidence: sp.item.confidence,
        })
        .collect();

    Ok(wrap(results, count))
}
