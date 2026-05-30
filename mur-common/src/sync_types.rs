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

// ── Fleet sync DTOs (Pro) ──────────────────────────────────────────

/// Kinds of fleet entity synced per-user across devices (Phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetEntityType {
    AgentProfile,
    ModelBinding,
    Skill,
}

impl FleetEntityType {
    /// URL path segment used in `/api/v1/core/fleet/<segment>`.
    pub fn path_segment(self) -> &'static str {
        match self {
            Self::AgentProfile => "agent_profile",
            Self::ModelBinding => "model_binding",
            Self::Skill => "skill",
        }
    }
}

/// One create/update/delete of a fleet entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetChange {
    /// "upsert" | "delete"
    pub action: String,
    /// Stable logical id: agent UUIDv7 for profiles, model key for bindings.
    pub logical_id: String,
    /// SHA-256 of the canonical payload (empty for deletes).
    pub content_hash: String,
    /// Canonical YAML/JSON payload (None for deletes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

/// One entity returned by a fleet pull.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetEntity {
    pub logical_id: String,
    pub content_hash: String,
    pub version: i64,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPushRequest {
    pub base_version: i64,
    pub entity_type: FleetEntityType,
    pub changes: Vec<FleetChange>,
    #[serde(default)]
    pub force_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPushResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPullResponse {
    pub entities: Vec<FleetEntity>,
    pub version: i64,
}

/// Combined fleet payload for one skill directory.
/// - `manifest_yaml`: raw `skill.yaml` (synced via LWW on `content_sha256`).
/// - `events_jsonl`: raw `events.jsonl` content (set-union merge on conflict).
/// - `content_sha256`: sha-256 of `manifest_yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFleetPayload {
    pub manifest_yaml: String,
    pub events_jsonl: String,
    pub content_sha256: String,
}

#[cfg(test)]
mod fleet_tests {
    use super::*;

    #[test]
    fn fleet_push_request_roundtrips() {
        let req = FleetPushRequest {
            base_version: 7,
            entity_type: FleetEntityType::AgentProfile,
            changes: vec![FleetChange {
                action: "upsert".into(),
                logical_id: "agent-abc".into(),
                content_hash: "deadbeef".into(),
                payload: Some("name: scout\n".into()),
            }],
            force_local: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FleetPushRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.base_version, 7);
        assert_eq!(back.entity_type, FleetEntityType::AgentProfile);
        assert_eq!(back.changes[0].logical_id, "agent-abc");
    }

    #[test]
    fn fleet_entity_type_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&FleetEntityType::ModelBinding).unwrap(),
            "\"model_binding\""
        );
    }

    #[test]
    fn skill_entity_type_roundtrips() {
        let s = serde_json::to_string(&FleetEntityType::Skill).unwrap();
        assert_eq!(s, r#""skill""#);
        let back: FleetEntityType = serde_json::from_str(&s).unwrap();
        assert_eq!(back, FleetEntityType::Skill);
        assert_eq!(FleetEntityType::Skill.path_segment(), "skill");
    }

    #[test]
    fn skill_fleet_payload_roundtrips() {
        let p = SkillFleetPayload {
            manifest_yaml: "name: foo\n".into(),
            events_jsonl: "{\"kind\":\"retrieval\"}\n".into(),
            content_sha256: "abc123".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: SkillFleetPayload = serde_json::from_str(&s).unwrap();
        assert_eq!(back.content_sha256, "abc123");
    }
}
