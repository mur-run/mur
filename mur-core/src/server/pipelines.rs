//! Pipeline handlers — `/api/v1/pipelines` (CRUD + run + validate).

use std::sync::Arc;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::executor::pipeline::PipelineExecutor;
use crate::store::pipeline_yaml::PipelineDef;

use super::{AppError, AppState, notify, wrap};

pub(super) async fn list_pipelines(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pipeline_store()?;
    let pipelines = store.list_all().map_err(AppError::Internal)?;
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pipelines, count))
}

pub(super) async fn get_pipeline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pipeline_store()?;
    let pipeline = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pipeline '{}' not found", id)))?;
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pipeline, count))
}

#[derive(Deserialize)]
pub struct CreatePipelineRequest {
    pub id: String,
    pub expression: String,
    #[serde(default)]
    pub description: String,
}

pub(super) async fn create_pipeline(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pipeline_store()?;
    if store.exists(&req.id) {
        return Err(AppError::BadRequest(format!(
            "Pipeline '{}' already exists",
            req.id
        )));
    }

    // Validate expression syntax
    mur_common::pipeline::parse_pipeline_expr(&req.expression)
        .map_err(|e| AppError::BadRequest(format!("Invalid pipeline expression: {}", e)))?;

    let pipeline = PipelineDef {
        id: req.id.clone(),
        expression: req.expression,
        description: req.description,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    store.save(&pipeline).map_err(AppError::Internal)?;
    notify(&state, "pipeline:created", &req.id);
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(pipeline, count)))
}

pub(super) async fn update_pipeline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pipeline_store()?;
    let mut pipeline = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pipeline '{}' not found", id)))?;

    if let Some(expr) = updates.get("expression").and_then(|v| v.as_str()) {
        // Validate new expression
        mur_common::pipeline::parse_pipeline_expr(expr)
            .map_err(|e| AppError::BadRequest(format!("Invalid pipeline expression: {}", e)))?;
        pipeline.expression = expr.to_string();
    }
    if let Some(desc) = updates.get("description").and_then(|v| v.as_str()) {
        pipeline.description = desc.to_string();
    }

    pipeline.updated_at = chrono::Utc::now();
    store.save(&pipeline).map_err(AppError::Internal)?;
    notify(&state, "pipeline:updated", &id);
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pipeline, count))
}

pub(super) async fn delete_pipeline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pipeline_store()?;
    let deleted = store
        .delete(&id)
        .map_err(|_| AppError::NotFound(format!("Pipeline '{}' not found", id)))?;
    if !deleted {
        return Err(AppError::NotFound(format!("Pipeline '{}' not found", id)));
    }
    notify(&state, "pipeline:deleted", &id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct RunPipelineRequest {
    #[serde(default)]
    pub fail_fast: bool,
}

pub(super) async fn run_pipeline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<RunPipelineRequest>,
) -> Result<impl IntoResponse, AppError> {
    let pl_store = state.pipeline_store()?;
    let pipeline = pl_store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pipeline '{}' not found", id)))?;

    let expr = mur_common::pipeline::parse_pipeline_expr(&pipeline.expression)
        .map_err(|e| AppError::BadRequest(format!("Invalid pipeline expression: {}", e)))?;

    let wf_store = state.workflow_store()?;
    let executor = PipelineExecutor::new(wf_store).with_fail_fast(req.fail_fast);

    let output = executor
        .execute(&expr, None)
        .await
        .map_err(AppError::Internal)?;

    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(output, count))
}

#[derive(Deserialize)]
pub struct ValidatePipelineRequest {
    pub expression: String,
}

#[derive(Serialize)]
struct ValidatePipelineResponse {
    valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ast: Option<mur_common::pipeline::PipelineExpr>,
}

pub(super) async fn validate_pipeline(
    Json(req): Json<ValidatePipelineRequest>,
) -> Result<impl IntoResponse, AppError> {
    match mur_common::pipeline::parse_pipeline_expr(&req.expression) {
        Ok(ast) => Ok(Json(ValidatePipelineResponse {
            valid: true,
            error: None,
            ast: Some(ast),
        })),
        Err(e) => Ok(Json(ValidatePipelineResponse {
            valid: false,
            error: Some(e.to_string()),
            ast: None,
        })),
    }
}

#[derive(Deserialize)]
pub(super) struct RunPipelineExprRequest {
    expression: String,
}

pub(super) async fn run_pipeline_expr(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunPipelineExprRequest>,
) -> Result<impl IntoResponse, AppError> {
    let expr = mur_common::pipeline::parse_pipeline_expr(&req.expression)
        .map_err(|e| AppError::BadRequest(format!("Invalid pipeline expression: {}", e)))?;

    let wf_store = state.workflow_store()?;
    let executor = PipelineExecutor::new(wf_store);

    let output = executor
        .execute(&expr, None)
        .await
        .map_err(AppError::Internal)?;

    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(output, count))
}
