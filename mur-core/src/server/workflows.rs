//! Workflow handlers — `/api/v1/workflows` plus the
//! `extract-from-session` draft generator and the `search` endpoint.

use std::sync::Arc;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::Content;
use mur_common::workflow::{Step, Variable, Workflow};

use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::vector::LanceDbStore as VectorStore;

use super::search::{SearchRequest, SearchResult};
use super::{AppError, AppState, notify, wrap};

/// Semantic + keyword search for workflows.
pub(super) async fn search_workflows(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.workflow_store()?;
    let all_workflows = store.list_all().map_err(AppError::Internal)?;
    let count = all_workflows.len();

    if all_workflows.is_empty() {
        return Ok(wrap(Vec::<SearchResult>::new(), 0));
    }

    // Try semantic search via LanceDB
    let mut results: Vec<SearchResult> = Vec::new();

    if state.index_dir.exists()
        && let Ok(cfg) = load_config()
    {
        let emb_cfg = EmbeddingConfig::from_config(&cfg);
        if let Ok(query_embedding) = embed(&req.query, &emb_cfg).await
            && let Ok(vector_store) =
                VectorStore::open(&state.index_dir, cfg.embedding.dimensions as i32).await
            && let Ok(vector_results) = vector_store
                .search(&query_embedding, req.limit, Some("workflow"))
                .await
        {
            for r in vector_results {
                if let Some(w) = all_workflows.iter().find(|w| w.name == r.name) {
                    results.push(SearchResult {
                        name: w.name.clone(),
                        description: w.description.clone(),
                        score: r.similarity as f64,
                        relevance: r.similarity as f64,
                        tier: "workflow".into(),
                        maturity: String::new(),
                        confidence: r.similarity as f64,
                    });
                }
            }
        }
    }

    // Fallback to keyword search if no semantic results
    if results.is_empty() {
        let q = req.query.to_lowercase();
        for w in &all_workflows {
            let text = format!("{} {} {}", w.name, w.description, w.tools.join(" ")).to_lowercase();
            if text.contains(&q) {
                results.push(SearchResult {
                    name: w.name.clone(),
                    description: w.description.clone(),
                    score: 0.5,
                    relevance: 0.5,
                    tier: "workflow".into(),
                    maturity: String::new(),
                    confidence: 0.5,
                });
            }
        }
    }

    results.truncate(req.limit);
    Ok(wrap(results, count))
}

pub(super) async fn list_workflows(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let wf_store = state.workflow_store()?;
    let workflows = wf_store.list_all().map_err(AppError::Internal)?;
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(workflows, count))
}

pub(super) async fn get_workflow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.workflow_store()?;
    let workflow = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Workflow '{}' not found", id)))?;
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(workflow, count))
}

#[derive(Deserialize)]
pub struct CreateWorkflowRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub steps: Option<Vec<String>>,
    #[serde(default)]
    pub variables: Option<Vec<Variable>>,
    #[serde(default)]
    pub source_sessions: Option<Vec<String>>,
}

pub(super) async fn create_workflow(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorkflowRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.workflow_store()?;
    if store.exists(&req.name) {
        return Err(AppError::BadRequest(format!(
            "Workflow '{}' already exists",
            req.name
        )));
    }

    // Convert step description strings → Step structs
    let steps: Vec<Step> = req
        .steps
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, desc)| Step {
            order: (i + 1) as u32,
            description: desc,
            ..Default::default()
        })
        .collect();

    let workflow = Workflow {
        base: KnowledgeBase {
            name: req.name.clone(),
            description: req.description,
            content: Content::Plain(req.content.unwrap_or_default()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        trigger: req.trigger.unwrap_or_default(),
        tools: req.tools.unwrap_or_default(),
        steps,
        variables: req.variables.unwrap_or_default(),
        source_sessions: req.source_sessions.unwrap_or_default(),
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };

    store.save(&workflow).map_err(AppError::Internal)?;
    notify(&state, "workflow:created", &req.name);
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(workflow, count)))
}

pub(super) async fn update_workflow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.workflow_store()?;
    let mut workflow = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Workflow '{}' not found", id)))?;

    // Handle rename: update name + delete old file
    let old_name = id.clone();
    if let Some(new_name) = updates.get("name").and_then(|v| v.as_str())
        && !new_name.is_empty()
        && new_name != id
    {
        workflow.name = new_name.to_string();
    }

    if let Some(desc) = updates.get("description").and_then(|v| v.as_str()) {
        workflow.description = desc.to_string();
    }
    if let Some(trigger) = updates.get("trigger").and_then(|v| v.as_str()) {
        workflow.trigger = trigger.to_string();
    }
    if let Some(tools) = updates.get("tools").and_then(|v| v.as_array()) {
        workflow.tools = tools
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(steps) = updates.get("steps").and_then(|v| v.as_array()) {
        workflow.steps = steps
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                v.as_str().map(|s| Step {
                    order: (i + 1) as u32,
                    description: s.to_string(),
                    ..Default::default()
                })
            })
            .collect();
    }
    if let Some(vars) = updates.get("variables")
        && let Ok(parsed) = serde_json::from_value::<Vec<Variable>>(vars.clone())
    {
        workflow.variables = parsed;
    }

    workflow.updated_at = chrono::Utc::now();
    store.save(&workflow).map_err(AppError::Internal)?;

    // If renamed, delete the old file
    if workflow.name != old_name {
        let _ = store.delete(&old_name);
    }

    notify(&state, "workflow:updated", &workflow.name);
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(workflow, count))
}

pub(super) async fn delete_workflow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.workflow_store()?;
    let deleted = store
        .delete(&id)
        .map_err(|_| AppError::NotFound(format!("Workflow '{}' not found", id)))?;
    if !deleted {
        return Err(AppError::NotFound(format!("Workflow '{}' not found", id)));
    }
    notify(&state, "workflow:deleted", &id);
    Ok(StatusCode::NO_CONTENT)
}

/// Extract a draft workflow from session events.
///
/// Reads the session recording, filters noise (mur commands, turn markers),
/// identifies tools used, detects variables, and generates a concise
/// title/description from the session context.
pub(super) async fn extract_workflow_from_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let events = crate::session::read_events(&session_id)
        .map_err(|_| AppError::NotFound(format!("Session '{}' not found", session_id)))?;

    let extracted: crate::extract::ExtractedWorkflow = if crate::extract::has_llm_config() {
        crate::extract::extract_workflow_llm(&session_id, &events)
            .await
            .unwrap_or_else(|_| crate::extract::extract_workflow(&session_id, &events))
    } else {
        crate::extract::extract_workflow(&session_id, &events)
    };

    let count = state
        .pattern_store()
        .ok()
        .and_then(|s| s.list_names().ok())
        .map(|n| n.len())
        .unwrap_or(0);

    Ok(wrap(extracted.workflow, count))
}
