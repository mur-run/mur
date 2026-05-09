//! Stats / tags / links handlers — `/api/v1/stats`, `/tags`, `/links/{id}`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::Serialize;

use mur_common::knowledge::Maturity;
use mur_common::pattern::{LifecycleStatus, Tier};

use super::{AppError, AppState, wrap};

#[derive(Serialize)]
struct StatsResponse {
    total_patterns: usize,
    total_workflows: usize,
    by_tier: TierCounts,
    by_maturity: MaturityCounts,
    by_status: StatusCounts,
    total_injections: u64,
    avg_confidence: f64,
    avg_importance: f64,
}

#[derive(Serialize, Default)]
struct TierCounts {
    session: usize,
    project: usize,
    core: usize,
}

#[derive(Serialize, Default)]
struct MaturityCounts {
    draft: usize,
    emerging: usize,
    stable: usize,
    canonical: usize,
}

#[derive(Serialize, Default)]
struct StatusCounts {
    active: usize,
    deprecated: usize,
    archived: usize,
}

pub(super) async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let patterns = store.list_all().map_err(AppError::Internal)?;
    let wf_store = state.workflow_store()?;
    let workflows = wf_store.list_all().map_err(AppError::Internal)?;

    let mut tier = TierCounts::default();
    let mut maturity = MaturityCounts::default();
    let mut status = StatusCounts::default();
    let mut total_injections = 0u64;
    let mut total_confidence = 0.0f64;
    let mut total_importance = 0.0f64;

    for p in &patterns {
        match p.tier {
            Tier::Session => tier.session += 1,
            Tier::Project => tier.project += 1,
            Tier::Core => tier.core += 1,
        }
        match p.maturity {
            Maturity::Draft => maturity.draft += 1,
            Maturity::Emerging => maturity.emerging += 1,
            Maturity::Stable => maturity.stable += 1,
            Maturity::Canonical => maturity.canonical += 1,
        }
        match p.lifecycle.status {
            LifecycleStatus::Active => status.active += 1,
            LifecycleStatus::Deprecated => status.deprecated += 1,
            LifecycleStatus::Archived => status.archived += 1,
        }
        total_injections += p.evidence.injection_count;
        total_confidence += p.confidence;
        total_importance += p.importance;
    }

    let n = patterns.len().max(1) as f64;
    let stats = StatsResponse {
        total_patterns: patterns.len(),
        total_workflows: workflows.len(),
        by_tier: tier,
        by_maturity: maturity,
        by_status: status,
        total_injections,
        avg_confidence: total_confidence / n,
        avg_importance: total_importance / n,
    };

    Ok(wrap(stats, patterns.len()))
}

#[derive(Serialize)]
struct TagInfo {
    name: String,
    count: usize,
}

pub(super) async fn get_tags(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let patterns = store.list_all().map_err(AppError::Internal)?;

    let mut tag_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for p in &patterns {
        for t in p.tags.topics.iter().chain(p.tags.languages.iter()) {
            *tag_counts.entry(t.clone()).or_default() += 1;
        }
    }

    let tags: Vec<TagInfo> = tag_counts
        .into_iter()
        .map(|(name, count)| TagInfo { name, count })
        .collect();

    Ok(wrap(tags, patterns.len()))
}

pub(super) async fn get_links(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let pattern = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pattern.links.clone(), count))
}
