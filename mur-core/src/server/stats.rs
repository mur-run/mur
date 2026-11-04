//! Stats / tags / links handlers — `/api/v1/stats`, `/tags`, `/links/{id}`.
//!
//! These used to aggregate the local Pattern store. Patterns are gone
//! (Tier C3); the same routes now describe the installed skills under
//! `~/.mur/skills/`, using each skill's `stats.json` sidecar for lifecycle
//! and usage counts. Response field names keep their historical spelling
//! (`total_patterns`, `pattern_count`) so existing dashboard clients parse.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::Serialize;

use mur_common::skill::SkillManifest;
use mur_common::skill::stats::{LifecycleState, SkillStats};

use super::{AppError, AppState, wrap};

#[derive(Serialize)]
struct StatsResponse {
    /// Installed skill count. Historical name kept for client compatibility.
    total_patterns: usize,
    total_workflows: usize,
    by_category: std::collections::BTreeMap<String, usize>,
    by_lifecycle: LifecycleCounts,
    total_usage: u64,
    total_success: u64,
    total_failure: u64,
}

#[derive(Serialize, Default)]
struct LifecycleCounts {
    draft: usize,
    emerging: usize,
    stable: usize,
    canonical: usize,
    deprecated: usize,
    archived: usize,
    /// Any lifecycle state added after this struct was written, plus
    /// `Swept` — kept out of the named buckets so the sum still adds up.
    other: usize,
}

/// Read every installed skill manifest under `skills_dir`. Unreadable
/// entries are skipped — the stats routes are advisory, never fail-closed.
pub(super) fn load_installed_skills(skills_dir: &std::path::Path) -> Vec<SkillManifest> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir()
            && let Ok(m) = mur_common::skill::read_from_dir(&path)
        {
            out.push(m);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub(super) async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let skills = load_installed_skills(&state.skills_dir());
    let wf_store = state.workflow_store()?;
    let workflows = wf_store.list_all().map_err(AppError::Internal)?;
    let mur_home = state.mur_home();

    let mut by_category = std::collections::BTreeMap::new();
    let mut lifecycle = LifecycleCounts::default();
    let mut total_usage = 0u64;
    let mut total_success = 0u64;
    let mut total_failure = 0u64;

    for s in &skills {
        *by_category
            .entry(format!("{:?}", s.category).to_lowercase())
            .or_insert(0) += 1;
        let stats = SkillStats::load(&SkillStats::path(&mur_home, &s.name))
            .ok()
            .flatten();
        match stats.as_ref().map(|st| st.lifecycle_state) {
            None | Some(LifecycleState::Draft) => lifecycle.draft += 1,
            Some(LifecycleState::Emerging) => lifecycle.emerging += 1,
            Some(LifecycleState::Stable) => lifecycle.stable += 1,
            Some(LifecycleState::Canonical) => lifecycle.canonical += 1,
            Some(LifecycleState::Deprecated) => lifecycle.deprecated += 1,
            Some(LifecycleState::Archived) => lifecycle.archived += 1,
            Some(_) => lifecycle.other += 1,
        }
        if let Some(st) = stats {
            total_usage += st.usage_count;
            total_success += st.success_count;
            total_failure += st.failure_count;
        }
    }

    let stats = StatsResponse {
        total_patterns: skills.len(),
        total_workflows: workflows.len(),
        by_category,
        by_lifecycle: lifecycle,
        total_usage,
        total_success,
        total_failure,
    };

    Ok(wrap(stats, skills.len()))
}

#[derive(Serialize)]
struct TagInfo {
    name: String,
    count: usize,
}

pub(super) async fn get_tags(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let skills = load_installed_skills(&state.skills_dir());

    let mut tag_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for s in &skills {
        for t in &s.tags {
            *tag_counts.entry(t.clone()).or_default() += 1;
        }
    }

    let tags: Vec<TagInfo> = tag_counts
        .into_iter()
        .map(|(name, count)| TagInfo { name, count })
        .collect();

    Ok(wrap(tags, skills.len()))
}

#[derive(Serialize)]
struct LinksResponse {
    /// Skills this one declares as dependencies.
    related: Vec<String>,
}

pub(super) async fn get_links(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let skills = load_installed_skills(&state.skills_dir());
    let skill = skills
        .iter()
        .find(|s| s.name == id)
        .ok_or_else(|| AppError::NotFound(format!("Skill '{}' not found", id)))?;
    let related = skill.requires.iter().map(|r| r.name.clone()).collect();
    Ok(wrap(LinksResponse { related }, skills.len()))
}
