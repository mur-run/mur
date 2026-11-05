//! Pure Channel types — no I/O; store logic lives in the `mur-channel` crate.
//!
//! Durable on-disk formats:
//!   - `~/.mur/channels/<id>/events.jsonl` — append-only event log
//!   - `~/.mur/channels/<id>/channel.yaml` — manifest (cached view of log state)
//!
//! # Schema versioning
//!
//! [`CHANNEL_SCHEMA_VERSION`] guards both the event log rows and the manifest.
//! Bump it ONLY when:
//!   1. A required field is renamed or removed, OR
//!   2. A field's semantic meaning changes, OR
//!   3. A new `EventKind` variant carries semantics that older readers must not
//!      silently skip (readers skip unknown/unparseable lines for robustness, so
//!      bump only when a silent skip would corrupt state rather than merely omit
//!      optional data).
//!
//! Adding a new optional field with `#[serde(default)]` does NOT require a bump.
//!
//! ## Backward reads
//! The event log must always remain fold-able from the beginning. When adding new
//! optional fields, annotate them with `#[serde(default)]` so older rows written
//! before the field existed still deserialize cleanly. Manifests follow the same
//! rule: older `channel.yaml` files must load without error.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version for the manifest + event log; breaking changes bump this.
/// v2: `HitlResponse` events carry approval authority — a reader that silently
/// skips one could re-apply a gated effect (v3c).
pub const CHANNEL_SCHEMA_VERSION: u32 = 2;

/// A2A v0.3 lifecycle vocabulary, serialized on the wire as kebab-case
/// (`input-required`, `canceled`, etc.).
///
/// ## Spelling note
/// `Canceled` (→ `"canceled"`) intentionally follows the A2A v0.3 spec spelling
/// and is a DISTINCT type from [`crate::a2a::TaskState`], which spells the
/// equivalent variant `Cancelled` (two l's). The two enums are bridged by an
/// explicit boundary mapping — not string equality — so the spelling difference
/// is deliberate, not a bug.
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
    /// MUR extension (not in A2A v0.3): the channel has had no activity for an
    /// extended period and was system-marked stale.
    Stale,
}

/// Why a channel exists. Deliberately smaller than the ways a UI may render a
/// channel: Direct vs Group is derived from participants, and Companion/HITL
/// are event-derived states — none of them are purposes.
///
/// `Option<ChannelPurpose>` on `Channel`: `None` means "written before this
/// field existed" and MUST NOT be treated as an explicit `Conversation`.
/// Resolve it for display with `mur_channel::purpose::effective_purpose`;
/// correct it on disk only with `mur channel backfill-purpose`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelPurpose {
    Conversation,
    FleetRun,
    WorkflowRun,
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

/// The role a participant plays within a channel's lifecycle.
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

/// The intent a channel is working toward, with optional acceptance criteria.
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
    /// Why this channel exists. `None` = legacy manifest; see `ChannelPurpose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<ChannelPurpose>,
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
    /// One agent asks another a question (channel consult). Signed by the
    /// asker; the payload is an [`AskPayload`] and `idempotency_key` equals
    /// its `nonce`. Not agent progress: readers that count agent output skip it.
    Ask,
    /// The answerer's reply to an [`EventKind::Ask`]; the payload is an
    /// [`AskReplyPayload`]. Not agent progress, a fleet reply, or a final answer.
    AskReply,
}

impl EventKind {
    /// True for the consult kinds, which an agent writes beside its work
    /// rather than as part of it. Readers that treat any agent-authored event
    /// as progress, a reply, or a final answer must exclude these.
    pub fn is_consult(self) -> bool {
        matches!(self, EventKind::Ask | EventKind::AskReply)
    }
}

/// Payload of an [`EventKind::Ask`] event. Every field is inside the signed
/// input, so the answerer, the deadline and the nonce cannot be rewritten
/// without breaking the asker's signature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskPayload {
    /// The answering agent's name. Only that agent may answer.
    pub to: String,
    /// The question text the answerer sees.
    pub question: String,
    /// Unix-epoch milliseconds after which the answerer refuses the `Ask`.
    pub expires_at_ms: i64,
    /// Single-use id; the event's `idempotency_key` carries the same value.
    pub nonce: String,
}

/// Payload of an [`EventKind::AskReply`] event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskReplyPayload {
    /// The `nonce` of the [`AskPayload`] this answers.
    pub nonce: String,
    /// The answer text.
    pub text: String,
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
    /// Detached Ed25519 signature (multibase) by the channel's WRITER over the
    /// canonical sign-input `{v, channel_id, actor, kind, payload,
    /// idempotency_key}` — EXCLUDING the store-assigned `seq`/`ts` (see
    /// `mur-channel` `sign::sign_input`). `None` for legacy/unsigned events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// RESERVED — key version for the signing key (v3d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_version: Option<u32>,
    /// Hex SHA-256 of the previous raw line in `events.jsonl` (or the genesis
    /// constant for the first line). Set only on sig-input v2 events, which
    /// sign over it; `None` for v1 (externally signed) and unsigned events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
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
            sig: None,
            key_version: None,
            prev_hash: None,
        };
        let line = serde_json::to_string(&ev).unwrap();
        let back: ChannelEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.seq, 3);
        assert_eq!(back.actor, ChannelActor::Agent { id: "qa".into() });
        assert_eq!(back.payload["text"], "hello");
    }

    #[test]
    fn consult_kinds_round_trip_in_kebab_case() {
        for (kind, wire) in [
            (EventKind::Ask, "\"ask\""),
            (EventKind::AskReply, "\"ask-reply\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
            assert_eq!(serde_json::from_str::<EventKind>(wire).unwrap(), kind);
            assert!(kind.is_consult());
        }
        assert!(!EventKind::Message.is_consult());
    }

    #[test]
    fn ask_event_round_trips_with_typed_payload() {
        let ask = AskPayload {
            to: "tech-writer".into(),
            question: "what does R7 cover?".into(),
            expires_at_ms: 1_700_000_000_000,
            nonce: "n1".into(),
        };
        let ev = ChannelEvent {
            seq: 4,
            ts: Utc::now(),
            actor: ChannelActor::Agent { id: "pm".into() },
            kind: EventKind::Ask,
            payload: serde_json::to_value(&ask).unwrap(),
            idempotency_key: Some(ask.nonce.clone()),
            sig: None,
            key_version: None,
            prev_hash: None,
        };
        let back: ChannelEvent =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(back.kind, EventKind::Ask);
        let got: AskPayload = serde_json::from_value(back.payload).unwrap();
        assert_eq!(got, ask);

        let reply = AskReplyPayload {
            nonce: "n1".into(),
            text: "seq reuse".into(),
        };
        let v = serde_json::to_value(&reply).unwrap();
        assert_eq!(serde_json::from_value::<AskReplyPayload>(v).unwrap(), reply);
    }

    #[test]
    fn system_actor_round_trips() {
        let a = ChannelActor::System;
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "{\"kind\":\"system\"}");
        let back: ChannelActor = serde_json::from_str(&j).unwrap();
        assert_eq!(back, ChannelActor::System);
    }

    #[test]
    fn event_omits_sig_fields_when_absent_and_reads_old_rows() {
        // New events with sig=None must omit the field from JSON (no schema churn).
        let ev = ChannelEvent {
            seq: 0,
            ts: Utc::now(),
            actor: ChannelActor::System,
            kind: EventKind::Note,
            payload: serde_json::json!({}),
            idempotency_key: None,
            sig: None,
            key_version: None,
            prev_hash: None,
        };
        let line = serde_json::to_string(&ev).unwrap();
        assert!(!line.contains("sig"), "sig must be omitted when None");
        assert!(
            !line.contains("key_version"),
            "key_version must be omitted when None"
        );

        // Old rows without the fields must deserialize cleanly.
        let old = r#"{"seq":0,"ts":"2026-06-16T00:00:00Z","actor":{"kind":"system"},"kind":"note","payload":{}}"#;
        let back: ChannelEvent = serde_json::from_str(old).unwrap();
        assert_eq!(back.sig, None);
        assert_eq!(back.key_version, None);
    }

    #[test]
    fn legacy_manifest_without_purpose_deserializes_as_none() {
        // A manifest written before this field existed. Must still load.
        let json = r#"{
            "v": 2,
            "id": "019ed0af-5e38-7912-b554-dc335a8fc2db",
            "title": "chat with mur",
            "state": "working",
            "owner": {"kind": "human", "name": "david"},
            "participants": [],
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-01T10:00:00Z"
        }"#;
        let ch: Channel = serde_json::from_str(json).expect("legacy manifest must deserialize");
        assert_eq!(
            ch.purpose, None,
            "absent purpose must be None, not a default"
        );
    }

    #[test]
    fn purpose_round_trips_in_kebab_case() {
        let json = r#"{
            "v": 2,
            "id": "x",
            "title": "t",
            "state": "working",
            "owner": {"kind": "system"},
            "participants": [],
            "purpose": "fleet-run",
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-01T10:00:00Z"
        }"#;
        let ch: Channel = serde_json::from_str(json).unwrap();
        assert_eq!(ch.purpose, Some(ChannelPurpose::FleetRun));

        let back = serde_json::to_string(&ch).unwrap();
        assert!(back.contains(r#""purpose":"fleet-run""#), "got: {back}");
    }

    #[test]
    fn purpose_is_omitted_when_none() {
        let json = r#"{
            "v": 2, "id": "x", "title": "t", "state": "working",
            "owner": {"kind": "system"}, "participants": [],
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-01T10:00:00Z"
        }"#;
        let ch: Channel = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&ch).unwrap();
        assert!(
            !back.contains("purpose"),
            "a None purpose must not be written back as null: {back}"
        );
    }
}
