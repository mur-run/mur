//! Session handlers — `/api/v1/sessions`.

use std::sync::Arc;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use super::{ApiResponse, AppError, AppState, wrap};

#[derive(Serialize)]
pub(super) struct SessionInfo {
    id: String,
    event_count: usize,
    file_size: u64,
    modified_at: String,
    source: Option<String>,
    started_at: Option<String>,
    stopped_at: Option<String>,
    title: Option<String>,
    tools_used: Option<Vec<String>>,
    user_turns: Option<usize>,
    assistant_turns: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct SessionDetail {
    id: String,
    event_count: usize,
    file_size: u64,
    modified_at: String,
    source: Option<String>,
    started_at: Option<String>,
    stopped_at: Option<String>,
    title: Option<String>,
    tools_used: Option<Vec<String>>,
    user_turns: Option<usize>,
    assistant_turns: Option<usize>,
    events: Vec<crate::session::SessionEvent>,
    #[serde(default)]
    fingerprints: Vec<mur_common::event::BehaviorFingerprint>,
}

#[derive(Deserialize)]
pub(super) struct BulkDeleteRequest {
    ids: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct SessionPatchRequest {
    title: Option<String>,
}

pub(super) async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ApiResponse<Vec<SessionInfo>>>, AppError> {
    let recordings = crate::session::list_recordings().map_err(AppError::Internal)?;
    let count = state
        .pattern_store()
        .ok()
        .and_then(|s| s.list_names().ok())
        .map(|n| n.len())
        .unwrap_or(0);

    let sessions: Vec<SessionInfo> = recordings
        .into_iter()
        .map(|r| {
            let modified_at: chrono::DateTime<chrono::Utc> = r.modified.into();
            let (source, started_at, stopped_at, title, tools_used, user_turns, assistant_turns) =
                if let Some(ref m) = r.meta {
                    (
                        Some(m.source.clone()),
                        Some(m.started_at.clone()),
                        m.stopped_at.clone(),
                        m.title.clone(),
                        Some(m.tools_used.clone()),
                        Some(m.user_turns),
                        Some(m.assistant_turns),
                    )
                } else {
                    (None, None, None, None, None, None, None)
                };
            SessionInfo {
                id: r.id,
                event_count: r.event_count,
                file_size: r.file_size,
                modified_at: modified_at.to_rfc3339(),
                source,
                started_at,
                stopped_at,
                title,
                tools_used,
                user_turns,
                assistant_turns,
            }
        })
        .collect();

    Ok(wrap(sessions, count))
}

pub(super) async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<SessionDetail>>, AppError> {
    let recordings = crate::session::list_recordings().map_err(AppError::Internal)?;
    let rec = recordings
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| AppError::NotFound(format!("Session '{}' not found", id)))?;

    let events = crate::session::read_events(&id).map_err(AppError::Internal)?;
    let modified_at: chrono::DateTime<chrono::Utc> = rec.modified.into();
    let count = state
        .pattern_store()
        .ok()
        .and_then(|s| s.list_names().ok())
        .map(|n| n.len())
        .unwrap_or(0);

    let (source, started_at, stopped_at, title, tools_used, user_turns, assistant_turns) =
        if let Some(ref m) = rec.meta {
            (
                Some(m.source.clone()),
                Some(m.started_at.clone()),
                m.stopped_at.clone(),
                m.title.clone(),
                Some(m.tools_used.clone()),
                Some(m.user_turns),
                Some(m.assistant_turns),
            )
        } else {
            (None, None, None, None, None, None, None)
        };

    // Load fingerprints for this session
    let fingerprints = crate::capture::emergence::load_fingerprints()
        .unwrap_or_default()
        .into_iter()
        .filter(|fp| fp.session_id == id)
        .collect();

    Ok(wrap(
        SessionDetail {
            id: rec.id,
            event_count: rec.event_count,
            file_size: rec.file_size,
            modified_at: modified_at.to_rfc3339(),
            source,
            started_at,
            stopped_at,
            title,
            tools_used,
            user_turns,
            assistant_turns,
            events,
            fingerprints,
        },
        count,
    ))
}

pub(super) async fn get_session_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<crate::session::SessionEvent>>>, AppError> {
    let events = crate::session::read_events(&id)
        .map_err(|_| AppError::NotFound(format!("Session '{}' not found", id)))?;
    let count = state
        .pattern_store()
        .ok()
        .and_then(|s| s.list_names().ok())
        .map(|n| n.len())
        .unwrap_or(0);
    Ok(wrap(events, count))
}

pub(super) async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    crate::session::delete_recording(&id).map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn bulk_delete_sessions(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BulkDeleteRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    for id in &body.ids {
        crate::session::delete_recording(id).map_err(AppError::Internal)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn patch_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SessionPatchRequest>,
) -> Result<Json<ApiResponse<crate::session::SessionMeta>>, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let meta = crate::session::update_meta(&id, body.title)
        .map_err(|_| AppError::NotFound(format!("Session '{}' meta not found", id)))?;
    let count = state
        .pattern_store()
        .ok()
        .and_then(|s| s.list_names().ok())
        .map(|n| n.len())
        .unwrap_or(0);
    Ok(wrap(meta, count))
}
