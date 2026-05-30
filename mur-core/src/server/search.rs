//! Search handler — `POST /api/v1/search`. The shared `SearchRequest` /
//! `SearchResult` types are also used by `workflows::search_workflows`.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::retrieve::scoring::score_and_rank_generic;
use crate::retrieve::skill_candidates::load_skill_candidates;

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
    let skills_dir = state.skills_dir();
    let mur_home = skills_dir.parent().unwrap_or(&skills_dir).to_path_buf();
    let candidates = load_skill_candidates(&skills_dir, &mur_home).map_err(AppError::Internal)?;
    let count = candidates.len();

    let results: Vec<SearchResult> = score_and_rank_generic(&req.query, candidates)
        .into_iter()
        .take(req.limit)
        .map(|s| SearchResult {
            name: s.item.manifest.name.clone(),
            description: s.item.manifest.description.clone(),
            score: s.score,
            relevance: s.relevance,
            tier: format!("{:?}", s.item.manifest.category).to_lowercase(),
            maturity: format!("{:?}", s.item.stats.lifecycle_state).to_lowercase(),
            confidence: s.item.stats.anchor_confidence,
        })
        .collect();

    Ok(wrap(results, count))
}
