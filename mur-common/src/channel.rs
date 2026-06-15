//! Unified Channel — the single durable work object shared across surfaces.
//! Pure types only (no I/O); store logic lives in the `mur-channel` crate.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version for the manifest + event log; breaking changes bump this.
pub const CHANNEL_SCHEMA_VERSION: u32 = 1;

/// A2A v0.3 lifecycle, serialized on the wire as kebab-case (`input-required`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Failed,
    Canceled,
    Rejected,
    Stale,
}

/// Who produced an event / is a participant. Named `ChannelActor` to avoid
/// colliding with the pre-existing `mur_common::actor::Actor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChannelActor {
    Human { name: String },
    Agent { id: String },
    System,
}

impl ChannelActor {
    /// The local human owner, from `$USER`/`$USERNAME`, falling back to `you`.
    pub fn local_human() -> Self {
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "you".to_string());
        ChannelActor::Human { name }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    Owner,
    Router,
    Delegate,
    Observer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub actor: ChannelActor,
    pub role: ParticipantRole,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Goal {
    #[serde(default)]
    pub statement: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

/// The durable manifest (a cache of state derivable from the event log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub v: u32,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub goal: Goal,
    pub state: ChannelState,
    pub owner: ChannelActor,
    #[serde(default)]
    pub participants: Vec<Participant>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Message,
    Delegation,
    Handoff,
    ToolCall,
    ToolResult,
    StateChange,
    Artifact,
    HitlRequest,
    HitlResponse,
    Note,
}

/// One append-only line in `~/.mur/channels/<id>/events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub actor: ChannelActor,
    pub kind: EventKind,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_state_serializes_kebab() {
        let j = serde_json::to_string(&ChannelState::InputRequired).unwrap();
        assert_eq!(j, "\"input-required\"");
    }

    #[test]
    fn event_round_trips() {
        let ev = ChannelEvent {
            seq: 3,
            ts: Utc::now(),
            actor: ChannelActor::Agent { id: "qa".into() },
            kind: EventKind::Message,
            payload: serde_json::json!({ "text": "hello", "task_id": "t-1" }),
            idempotency_key: None,
        };
        let line = serde_json::to_string(&ev).unwrap();
        let back: ChannelEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.seq, 3);
        assert_eq!(back.actor, ChannelActor::Agent { id: "qa".into() });
        assert_eq!(back.payload["text"], "hello");
    }
}
