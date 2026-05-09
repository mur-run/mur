//! Pattern CRUD handlers — `/api/v1/patterns`.

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use super::{AppError, AppState, notify, wrap};

#[derive(Deserialize, Default)]
pub struct PatternFilter {
    pub tier: Option<String>,
    pub maturity: Option<String>,
    pub tag: Option<String>,
    pub status: Option<String>,
}

pub(super) async fn list_patterns(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<PatternFilter>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let mut patterns = store.list_all().map_err(AppError::Internal)?;

    // Apply filters
    if let Some(tier) = &filter.tier {
        let tier_lower = tier.to_lowercase();
        patterns.retain(|p| format!("{:?}", p.tier).to_lowercase() == tier_lower);
    }
    if let Some(maturity) = &filter.maturity {
        let mat_lower = maturity.to_lowercase();
        patterns.retain(|p| format!("{:?}", p.maturity).to_lowercase() == mat_lower);
    }
    if let Some(tag) = &filter.tag {
        let tag_lower = tag.to_lowercase();
        patterns.retain(|p| {
            p.tags
                .topics
                .iter()
                .chain(p.tags.languages.iter())
                .any(|t| t.to_lowercase() == tag_lower)
        });
    }
    if let Some(status) = &filter.status {
        let status_lower = status.to_lowercase();
        patterns.retain(|p| format!("{:?}", p.lifecycle.status).to_lowercase() == status_lower);
    }

    let count = patterns.len();
    Ok(wrap(patterns, count))
}

pub(super) async fn get_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let pattern = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pattern, count))
}

#[derive(Deserialize)]
pub struct CreatePatternRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub technical: Option<String>,
    #[serde(default)]
    pub principle: Option<String>,
    #[serde(default)]
    pub plain_content: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

pub(super) async fn create_pattern(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePatternRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    if store.exists(&req.name) {
        return Err(AppError::BadRequest(format!(
            "Pattern '{}' already exists",
            req.name
        )));
    }

    let content = if let Some(tech) = req.technical {
        Content::DualLayer {
            technical: tech,
            principle: req.principle,
        }
    } else if let Some(plain) = req.plain_content {
        Content::Plain(plain)
    } else {
        return Err(AppError::BadRequest(
            "Must provide 'technical' or 'plain_content'".to_string(),
        ));
    };

    let tier = match req.tier.as_deref() {
        Some("project") => Tier::Project,
        Some("core") => Tier::Core,
        _ => Tier::Session,
    };

    let pattern = Pattern {
        base: KnowledgeBase {
            schema: SCHEMA_VERSION,
            name: req.name.clone(),
            description: req.description,
            content,
            tier,
            confidence: req.confidence.unwrap_or(0.7),
            tags: Tags {
                topics: req.tags.unwrap_or_default(),
                ..Tags::default()
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    };

    store.save(&pattern).map_err(AppError::Internal)?;
    notify(&state, "pattern:created", &req.name);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(pattern, count)))
}

pub(super) async fn update_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    let mut pattern = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;

    // Apply partial updates
    if let Some(desc) = updates.get("description").and_then(|v| v.as_str()) {
        pattern.description = desc.to_string();
    }
    if let Some(tech) = updates.get("technical").and_then(|v| v.as_str()) {
        pattern.content = Content::DualLayer {
            technical: tech.to_string(),
            principle: updates
                .get("principle")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| match &pattern.content {
                    Content::DualLayer { principle, .. } => principle.clone(),
                    Content::Plain(_) => None,
                }),
        };
    }
    if let Some(conf) = updates.get("confidence").and_then(|v| v.as_f64()) {
        pattern.confidence = conf.clamp(0.0, 1.0);
    }
    if let Some(imp) = updates.get("importance").and_then(|v| v.as_f64()) {
        pattern.importance = imp.clamp(0.0, 1.0);
    }
    if let Some(tier_str) = updates.get("tier").and_then(|v| v.as_str()) {
        pattern.tier = match tier_str {
            "project" => Tier::Project,
            "core" => Tier::Core,
            _ => Tier::Session,
        };
    }
    if let Some(tags) = updates.get("tags").and_then(|v| v.as_array()) {
        pattern.tags.topics = tags
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }

    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern).map_err(AppError::Internal)?;
    notify(&state, "pattern:updated", &id);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pattern, count))
}

pub(super) async fn delete_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    let deleted = store
        .delete(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;
    if !deleted {
        return Err(AppError::NotFound(format!("Pattern '{}' not found", id)));
    }
    notify(&state, "pattern:deleted", &id);
    Ok(StatusCode::NO_CONTENT)
}
