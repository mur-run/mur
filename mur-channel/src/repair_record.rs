//! The record that opens a repaired chain (`mur channel repair`, #1965).
//! Lives here, below `mur-core`, so the agent runtime can recognise it
//! without depending on the CLI crate (§3.4, revision 5 review).
use mur_common::channel::{ChannelEvent, EventKind};

/// `payload.type` of the event that opens a repaired chain.
pub const REPAIR_EVENT_TYPE: &str = "channel-repair";

/// Is `ev` the record `restart_chain` writes at `seq 0` of a fresh chain?
/// Shape only — the caller verifies the signature.
pub fn is_repair_record(ev: &ChannelEvent) -> bool {
    ev.kind == EventKind::Note
        && ev.payload.get("type").and_then(|v| v.as_str()) == Some(REPAIR_EVENT_TYPE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::ChannelActor;

    fn ev(kind: EventKind, payload: serde_json::Value) -> ChannelEvent {
        ChannelEvent {
            seq: 0,
            ts: Utc::now(),
            actor: ChannelActor::System,
            kind,
            payload,
            idempotency_key: None,
            sig: None,
            key_version: None,
            prev_hash: None,
        }
    }

    #[test]
    fn note_with_repair_type_is_a_repair_record() {
        assert!(is_repair_record(&ev(
            EventKind::Note,
            serde_json::json!({"type": "channel-repair"})
        )));
    }

    #[test]
    fn message_with_repair_type_is_not() {
        assert!(!is_repair_record(&ev(
            EventKind::Message,
            serde_json::json!({"type": "channel-repair"})
        )));
    }

    #[test]
    fn note_without_type_is_not() {
        assert!(!is_repair_record(&ev(
            EventKind::Note,
            serde_json::json!({"text": "x"})
        )));
    }
}
