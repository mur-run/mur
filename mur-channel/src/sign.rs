//! Per-event Ed25519 signing (v3d). The WRITER signs the canonical sign-input —
//! `{v, channel_id, actor, kind, payload, idempotency_key}` — EXCLUDING the
//! store-assigned `seq` and `ts` (the signer does not know `seq`; the store
//! restamps `ts` under the append lock), so the caller can sign BEFORE append.
//! Verify-on-fold recomputes this input and checks `sig` against the writer's
//! pubkey at `key_version` (resolved via the rotation chain).

use mur_common::channel::{ChannelActor, EventKind};

/// Canonicalization version — bump if the sign-input shape changes so an old
/// signature is never silently checked against a new canonicalization.
pub const SIG_INPUT_VERSION: u32 = 1;

/// Canonical bytes signed for an event. `serde_json` sorts object keys (no
/// preserve_order), so this is deterministic for a given input.
pub fn sign_input(
    channel_id: &str,
    actor: &ChannelActor,
    kind: EventKind,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
) -> Vec<u8> {
    let canon = serde_json::json!({
        "v": SIG_INPUT_VERSION,
        "channel_id": channel_id,
        "actor": actor,
        "kind": kind,
        "payload": payload,
        "idempotency_key": idempotency_key,
    });
    serde_json::to_vec(&canon).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_input_excludes_seq_ts_and_is_stable() {
        let actor = ChannelActor::Agent { id: "qa".into() };
        let p = serde_json::json!({ "text": "hi" });
        let a = sign_input("c1", &actor, EventKind::Message, &p, Some("k1"));
        let b = sign_input("c1", &actor, EventKind::Message, &p, Some("k1"));
        assert_eq!(a, b, "deterministic");
        assert_ne!(
            a,
            sign_input("c2", &actor, EventKind::Message, &p, Some("k1"))
        );
        assert_ne!(
            a,
            sign_input(
                "c1",
                &actor,
                EventKind::Message,
                &serde_json::json!({"text":"yo"}),
                Some("k1")
            )
        );
        assert_ne!(a, sign_input("c1", &actor, EventKind::Message, &p, None));
        let s = String::from_utf8_lossy(&a);
        assert!(!s.contains("\"seq\""));
        assert!(!s.contains("\"ts\""));
    }
}
