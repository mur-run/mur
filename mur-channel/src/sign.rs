//! Per-event Ed25519 signing (v3d). The WRITER signs the canonical sign-input —
//! `{v, channel_id, actor, kind, payload, idempotency_key}` — EXCLUDING the
//! store-assigned `seq` and `ts` (the signer does not know `seq`; the store
//! restamps `ts` under the append lock), so the caller can sign BEFORE append.
//! Verify-on-fold recomputes this input and checks `sig` against the writer's
//! pubkey at `key_version` (resolved via the rotation chain).

use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;

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

/// Sign an event's canonical input with `identity`; returns the multibase sig.
pub fn sign_event(
    identity: &AgentIdentity,
    channel_id: &str,
    actor: &ChannelActor,
    kind: EventKind,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
) -> String {
    let input = sign_input(channel_id, actor, kind, payload, idempotency_key);
    let sig = identity.sign_bytes(&input);
    multibase::encode(multibase::Base::Base58Btc, sig)
}

/// Verify a multibase signature over an event's canonical input against a raw
/// Ed25519 pubkey. Returns false on any decode/verify failure (fail-closed).
pub fn verify_event_sig(
    channel_id: &str,
    actor: &ChannelActor,
    kind: EventKind,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
    sig_multibase: &str,
    pubkey: &[u8; 32],
) -> bool {
    let Ok((_b, sig_bytes)) = multibase::decode(sig_multibase) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.as_slice().try_into() else {
        return false;
    };
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let input = sign_input(channel_id, actor, kind, payload, idempotency_key);
    use ed25519_dalek::Verifier;
    vk.verify(&input, &sig).is_ok()
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

    use mur_common::identity::AgentIdentity;
    use tempfile::TempDir;

    #[test]
    fn sign_then_verify_roundtrips_and_rejects_tamper() {
        let tmp = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        id.save(tmp.path()).unwrap();
        let actor = ChannelActor::Agent { id: "mur".into() };
        let payload = serde_json::json!({ "text": "approved" });

        let sig = sign_event(
            &id,
            "c1",
            &actor,
            EventKind::HitlResponse,
            &payload,
            Some("k1"),
        );
        let pub_bytes = id.verifying_key_bytes();
        assert!(verify_event_sig(
            "c1",
            &actor,
            EventKind::HitlResponse,
            &payload,
            Some("k1"),
            &sig,
            &pub_bytes
        ));
        // Tampered payload → fails.
        let tampered = serde_json::json!({ "text": "DENIED" });
        assert!(!verify_event_sig(
            "c1",
            &actor,
            EventKind::HitlResponse,
            &tampered,
            Some("k1"),
            &sig,
            &pub_bytes
        ));
        // Wrong key → fails.
        let other = AgentIdentity::generate();
        assert!(!verify_event_sig(
            "c1",
            &actor,
            EventKind::HitlResponse,
            &payload,
            Some("k1"),
            &sig,
            &other.verifying_key_bytes()
        ));
    }
}
