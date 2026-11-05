//! R1 · The kind cannot be forged from a Message: `kind` is inside the signed
//! input, so rewriting a signed `message` line to `ask` breaks verification.

use chrono::Utc;
use mur_channel::sign::{sign_event, sign_event_v2, verify_one};
use mur_common::channel::{AskPayload, ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

const CHANNEL: &str = "c1";
const PREV: &str = "00";

fn event(
    kind: EventKind,
    payload: serde_json::Value,
    sig: String,
    prev: Option<&str>,
) -> ChannelEvent {
    ChannelEvent {
        seq: 0,
        ts: Utc::now(),
        actor: ChannelActor::Agent { id: "pm".into() },
        kind,
        payload,
        idempotency_key: Some("n1".into()),
        sig: Some(sig),
        key_version: None,
        prev_hash: prev.map(str::to_string),
    }
}

/// Re-read the line with `kind` rewritten, the way an attacker editing
/// `events.jsonl` would produce it.
fn rewrite_kind(ev: &ChannelEvent, kind: &str) -> ChannelEvent {
    let mut v = serde_json::to_value(ev).unwrap();
    v["kind"] = serde_json::Value::String(kind.into());
    serde_json::from_value(v).unwrap()
}

fn ask_shaped_payload() -> serde_json::Value {
    serde_json::to_value(AskPayload {
        to: "tech-writer".into(),
        question: "q".into(),
        expires_at_ms: i64::MAX,
        nonce: "n1".into(),
    })
    .unwrap()
}

#[test]
fn message_rewritten_to_ask_fails_verification_v1_and_v2() {
    let id = AgentIdentity::generate();
    let key = id.verifying_key_bytes();
    let actor = ChannelActor::Agent { id: "pm".into() };
    let payload = ask_shaped_payload();

    let v1 = sign_event(
        &id,
        CHANNEL,
        &actor,
        EventKind::Message,
        &payload,
        Some("n1"),
    );
    let v2 = sign_event_v2(
        &id,
        CHANNEL,
        &actor,
        EventKind::Message,
        &payload,
        Some("n1"),
        PREV,
    );

    for msg in [
        event(EventKind::Message, payload.clone(), v1, None),
        event(EventKind::Message, payload.clone(), v2, Some(PREV)),
    ] {
        assert!(
            verify_one(CHANNEL, &msg, &key, true),
            "genuine message verifies"
        );
        let forged = rewrite_kind(&msg, "ask");
        assert_eq!(forged.kind, EventKind::Ask);
        assert!(
            !verify_one(CHANNEL, &forged, &key, true),
            "rewritten kind must fail"
        );
    }
}

#[test]
fn genuinely_signed_ask_verifies_and_cannot_become_ask_reply() {
    let id = AgentIdentity::generate();
    let key = id.verifying_key_bytes();
    let actor = ChannelActor::Agent { id: "pm".into() };
    let payload = ask_shaped_payload();
    let sig = sign_event_v2(
        &id,
        CHANNEL,
        &actor,
        EventKind::Ask,
        &payload,
        Some("n1"),
        PREV,
    );
    let ask = event(EventKind::Ask, payload, sig, Some(PREV));
    assert!(verify_one(CHANNEL, &ask, &key, true));
    assert!(!verify_one(
        CHANNEL,
        &rewrite_kind(&ask, "ask-reply"),
        &key,
        true
    ));
}
