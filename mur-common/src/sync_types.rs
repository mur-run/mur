//! Typed DTOs for cloud sync API (Go server).
//!
//! These replace the previous stringly-typed /api/sync/pull, /api/sync/push
//! endpoints with the current team-scoped, integer-versioned API.

use serde::{Deserialize, Serialize};

/// Response from GET /api/v1/core/teams/{id}/sync/pull?since={v}
#[derive(Debug, Deserialize)]
pub struct SyncPullResponse {
    pub patterns: Vec<RemotePattern>,
    pub version: i64,
}

/// A single remote pattern in a pull response.
#[derive(Debug, Deserialize)]
pub struct RemotePattern {
    pub id: String,
    pub name: String,
    pub content: String,
    pub version: i64,
    #[serde(default)]
    pub deleted: bool,
}

/// Request body for POST /api/v1/core/teams/{id}/sync/push
#[derive(Debug, Serialize)]
pub struct SyncPushRequest {
    pub base_version: i64,
    pub changes: Vec<PatternChange>,
    #[serde(default)]
    pub force_local: bool,
}

/// A single change in a push request.
#[derive(Debug, Serialize)]
pub struct PatternChange {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PatternPayload>,
}

#[derive(Debug, Serialize)]
pub struct PatternPayload {
    pub name: String,
    pub content: String,
}

/// Response from POST /api/v1/core/teams/{id}/sync/push
#[derive(Debug, Deserialize)]
pub struct SyncPushResponse {
    pub ok: bool,
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub conflict: Option<bool>,
}

/// Response from GET /api/v1/workflows
#[derive(Debug, Deserialize)]
pub struct WorkflowListResponse {
    pub data: Vec<serde_json::Value>,
}
