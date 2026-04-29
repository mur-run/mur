//! Companion outbox event schema (Spec §3.5).
//!
//! `OutboxEvent` is the frozen, append-only record written to the companion
//! ledger.  No append logic lives here — that is M5.3+.  No tracing
//! integration — that is M5.5.

use chrono::{DateTime, Utc};
use mur_common::companion::{Relationship, Signal, Situation};
use serde::{Deserialize, Serialize};

/// Every event that the companion subsystem records to the outbox ledger.
///
/// Schema is frozen; adding variants is a breaking change to the ledger
/// format.  The `#[serde(tag = "event")]` adjacent encoding means each
/// JSON object carries an `"event"` field whose value is the variant name
/// (PascalCase, matching the Rust identifier).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum OutboxEvent {
    CompanionInitialized {
        at: DateTime<Utc>,
        version: u32,
    },
    RelationshipChanged {
        old: Relationship,
        new: Relationship,
        at: DateTime<Utc>,
    },
    QuietRequested {
        until: DateTime<Utc>,
        reason: String,
        at: DateTime<Utc>,
    },
    MessageScheduled {
        id: String,
        situation: Situation,
        template_id: String,
        scheduled_for: DateTime<Utc>,
    },
    MessageGenerated {
        id: String,
        locale_used: String,
        body_sha256: String,
        linter_violations: u32,
        regen_count: u32,
    },
    MessagePaused {
        id: String,
        resume_at: DateTime<Utc>,
        reason: String,
    },
    MessageSent {
        id: String,
        channel: String,
        sent_at: DateTime<Utc>,
    },
    MessageDropped {
        id: String,
        reason: String,
    },
    UserSignal {
        id: String,
        signal: Signal,
        at: DateTime<Utc>,
    },
    PassiveDismissInferred {
        id: String,
        at: DateTime<Utc>,
    },
    LocaleMismatchUnresolved {
        id: String,
        attempts: u8,
        at: DateTime<Utc>,
    },
    VoiceMdComposed {
        relationship: Relationship,
        locale_used: String,
        fallback_from: Option<String>,
        at: DateTime<Utc>,
    },
    RhythmWiped {
        at: DateTime<Utc>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap()
    }

    fn roundtrip(event: OutboxEvent) -> OutboxEvent {
        let json = serde_json::to_string(&event).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn companion_initialized() {
        let ev = OutboxEvent::CompanionInitialized {
            at: fixed_ts(),
            version: 1,
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn relationship_changed() {
        let ev = OutboxEvent::RelationshipChanged {
            old: Relationship::Friend,
            new: Relationship::Coach,
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn quiet_requested() {
        let ev = OutboxEvent::QuietRequested {
            until: fixed_ts(),
            reason: "user asked".to_string(),
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn message_scheduled() {
        let ev = OutboxEvent::MessageScheduled {
            id: "msg-1".to_string(),
            situation: Situation::MorningGreeting,
            template_id: "tmpl-a".to_string(),
            scheduled_for: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn message_generated() {
        let ev = OutboxEvent::MessageGenerated {
            id: "msg-1".to_string(),
            locale_used: "en".to_string(),
            body_sha256: "abc123".to_string(),
            linter_violations: 0,
            regen_count: 1,
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn message_paused() {
        let ev = OutboxEvent::MessagePaused {
            id: "msg-1".to_string(),
            resume_at: fixed_ts(),
            reason: "rate limit".to_string(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn message_sent() {
        let ev = OutboxEvent::MessageSent {
            id: "msg-1".to_string(),
            channel: "push".to_string(),
            sent_at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn message_dropped() {
        let ev = OutboxEvent::MessageDropped {
            id: "msg-1".to_string(),
            reason: "permission denied".to_string(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn user_signal() {
        let ev = OutboxEvent::UserSignal {
            id: "msg-1".to_string(),
            signal: Signal::Positive,
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn passive_dismiss_inferred() {
        let ev = OutboxEvent::PassiveDismissInferred {
            id: "msg-1".to_string(),
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn locale_mismatch_unresolved() {
        let ev = OutboxEvent::LocaleMismatchUnresolved {
            id: "msg-1".to_string(),
            attempts: 3,
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn voice_md_composed_with_fallback() {
        let ev = OutboxEvent::VoiceMdComposed {
            relationship: Relationship::Mentor,
            locale_used: "zh-TW".to_string(),
            fallback_from: Some("zh-CN".to_string()),
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn voice_md_composed_no_fallback() {
        let ev = OutboxEvent::VoiceMdComposed {
            relationship: Relationship::AccountabilityBuddy,
            locale_used: "en".to_string(),
            fallback_from: None,
            at: fixed_ts(),
        };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    #[test]
    fn rhythm_wiped() {
        let ev = OutboxEvent::RhythmWiped { at: fixed_ts() };
        assert_eq!(roundtrip(ev.clone()), ev);
    }

    /// Verify the `"event"` tag field is present in every serialized variant.
    #[test]
    fn all_variants_have_event_tag() {
        let events: Vec<OutboxEvent> = vec![
            OutboxEvent::CompanionInitialized {
                at: fixed_ts(),
                version: 1,
            },
            OutboxEvent::RelationshipChanged {
                old: Relationship::Friend,
                new: Relationship::Coach,
                at: fixed_ts(),
            },
            OutboxEvent::QuietRequested {
                until: fixed_ts(),
                reason: "r".to_string(),
                at: fixed_ts(),
            },
            OutboxEvent::MessageScheduled {
                id: "x".to_string(),
                situation: Situation::GentleCheckIn,
                template_id: "t".to_string(),
                scheduled_for: fixed_ts(),
            },
            OutboxEvent::MessageGenerated {
                id: "x".to_string(),
                locale_used: "en".to_string(),
                body_sha256: "h".to_string(),
                linter_violations: 0,
                regen_count: 0,
            },
            OutboxEvent::MessagePaused {
                id: "x".to_string(),
                resume_at: fixed_ts(),
                reason: "r".to_string(),
            },
            OutboxEvent::MessageSent {
                id: "x".to_string(),
                channel: "c".to_string(),
                sent_at: fixed_ts(),
            },
            OutboxEvent::MessageDropped {
                id: "x".to_string(),
                reason: "r".to_string(),
            },
            OutboxEvent::UserSignal {
                id: "x".to_string(),
                signal: Signal::Dismiss,
                at: fixed_ts(),
            },
            OutboxEvent::PassiveDismissInferred {
                id: "x".to_string(),
                at: fixed_ts(),
            },
            OutboxEvent::LocaleMismatchUnresolved {
                id: "x".to_string(),
                attempts: 1,
                at: fixed_ts(),
            },
            OutboxEvent::VoiceMdComposed {
                relationship: Relationship::Friend,
                locale_used: "en".to_string(),
                fallback_from: None,
                at: fixed_ts(),
            },
            OutboxEvent::RhythmWiped { at: fixed_ts() },
        ];

        for ev in &events {
            let json = serde_json::to_value(ev).expect("serialize");
            assert!(
                json.get("event").is_some(),
                "missing 'event' tag in: {json}"
            );
        }
    }
}
