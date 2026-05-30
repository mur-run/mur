//! Skill CRUD handlers — `/api/v1/skills`.

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use mur_common::skill::{read_from_dir, write_to_dir, SkillManifest};

use super::{AppError, AppState, notify, wrap};

#[derive(Deserialize, Default)]
pub struct SkillFilter {
    pub category: Option<String>,
    pub tag: Option<String>,
}

pub(super) async fn list_skills(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<SkillFilter>,
) -> Result<impl IntoResponse, AppError> {
    let skills_dir = state.skills_dir();
    if !skills_dir.exists() {
        return Ok(wrap(Vec::<SkillManifest>::new(), 0));
    }

    let mut skills = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries {
            if let Ok(en) = entry {
                let path = en.path();
                if path.is_dir() {
                    if let Ok(skill) = read_from_dir(&path) {
                        skills.push(skill);
                    }
                }
            }
        }
    }

    // Apply filters
    if let Some(category) = &filter.category {
        let cat_lower = category.to_lowercase();
        skills.retain(|s| format!("{:?}", s.category).to_lowercase() == cat_lower);
    }
    if let Some(tag) = &filter.tag {
        let tag_lower = tag.to_lowercase();
        skills.retain(|s| s.tags.iter().any(|t| t.to_lowercase() == tag_lower));
    }

    let count = skills.len();
    Ok(wrap(skills, count))
}

pub(super) async fn get_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let skills_dir = state.skills_dir();
    let skill_dir = skills_dir.join(&name);
    let skill = read_from_dir(&skill_dir)
        .map_err(|_| AppError::NotFound(format!("Skill '{}' not found", name)))?;

    // Count total skills for response metadata
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries {
            if let Ok(en) = entry {
                if en.path().is_dir() {
                    count += 1;
                }
            }
        }
    }

    Ok(wrap(skill, count))
}

#[derive(Deserialize)]
pub struct CreateSkillRequest {
    pub manifest: SkillManifest,
}

pub(super) async fn create_skill(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSkillRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }

    let skills_dir = state.skills_dir();
    let skill_dir = skills_dir.join(&req.manifest.name);
    if skill_dir.exists() {
        return Err(AppError::BadRequest(format!(
            "Skill '{}' already exists",
            req.manifest.name
        )));
    }

    write_to_dir(&skill_dir, &req.manifest)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
    notify(&state, "skill:created", &req.manifest.name);
    Ok((StatusCode::CREATED, wrap(req.manifest, 1)))
}

pub(super) async fn update_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }

    let skills_dir = state.skills_dir();
    let skill_dir = skills_dir.join(&name);
    let mut skill = read_from_dir(&skill_dir)
        .map_err(|_| AppError::NotFound(format!("Skill '{}' not found", name)))?;

    // Apply partial updates
    if let Some(desc) = updates.get("description").and_then(|v| v.as_str()) {
        skill.description = desc.to_string();
    }
    if let Some(vers) = updates.get("version").and_then(|v| v.as_str()) {
        skill.version = vers.to_string();
    }
    if let Some(abstract_text) = updates.get("abstract").and_then(|v| v.as_str()) {
        skill.content.r#abstract = abstract_text.to_string();
    }
    if let Some(tags) = updates.get("tags").and_then(|v| v.as_array()) {
        skill.tags = tags
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }

    skill.updated_at = chrono::Utc::now();
    write_to_dir(&skill_dir, &skill)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
    notify(&state, "skill:updated", &name);
    Ok(wrap(skill, 1))
}

pub(super) async fn delete_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }

    let skills_dir = state.skills_dir();
    let skill_dir = skills_dir.join(&name);
    if !skill_dir.exists() {
        return Err(AppError::NotFound(format!("Skill '{}' not found", name)));
    }

    std::fs::remove_dir_all(&skill_dir).map_err(|_| {
        AppError::NotFound(format!("Failed to delete skill '{}'", name))
    })?;

    notify(&state, "skill:deleted", &name);
    Ok(StatusCode::NO_CONTENT)
}
