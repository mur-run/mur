//! Skill CRUD handlers — `/api/v1/skills`.

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use mur_common::skill::{SkillManifest, read_from_dir, write_to_dir};

use super::{AppError, AppState, notify, wrap};

/// Resolve `skills_dir/<name>`, rejecting any request-supplied name that would
/// escape the skills directory. The dashboard server binds a network interface
/// without auth, so an unvalidated name on these routes is a remote
/// path-traversal — most severely `delete_skill`, where a name of `..` would
/// `remove_dir_all` the entire mur home. A skill name is a single directory
/// component: separators, `.`/`..`, control chars, and empty/oversize are
/// refused.
fn safe_skill_dir(
    skills_dir: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, AppError> {
    if name.is_empty()
        || name.len() > 64
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|c| c == '\0' || c.is_control())
    {
        return Err(AppError::BadRequest(format!(
            "invalid skill name: {name:?}"
        )));
    }
    Ok(skills_dir.join(name))
}

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
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir()
                && let Ok(skill) = read_from_dir(&path)
            {
                skills.push(skill);
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
    let skill_dir = safe_skill_dir(&skills_dir, &name)?;
    let skill = read_from_dir(&skill_dir)
        .map_err(|_| AppError::NotFound(format!("Skill '{}' not found", name)))?;

    // Count total skills for response metadata
    let count = std::fs::read_dir(&skills_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0);

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
    let skill_dir = safe_skill_dir(&skills_dir, &req.manifest.name)?;
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
    let skill_dir = safe_skill_dir(&skills_dir, &name)?;
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
    write_to_dir(&skill_dir, &skill).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
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
    let skill_dir = safe_skill_dir(&skills_dir, &name)?;
    if !skill_dir.exists() {
        return Err(AppError::NotFound(format!("Skill '{}' not found", name)));
    }

    std::fs::remove_dir_all(&skill_dir)
        .map_err(|_| AppError::NotFound(format!("Failed to delete skill '{}'", name)))?;

    notify(&state, "skill:deleted", &name);
    Ok(StatusCode::NO_CONTENT)
}
