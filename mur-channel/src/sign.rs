//! Per-event Ed25519 signing (v3d). The WRITER signs the canonical sign-input —
//! `{v, channel_id, actor, kind, payload, idempotency_key}` — EXCLUDING the
//! store-assigned `seq` and `ts` (the signer does not know `seq`; the store
//! restamps `ts` under the append lock), so the caller can sign BEFORE append.
//! Verify-on-fold recomputes this input and checks `sig` against the writer's
//! pubkey at `key_version` (resolved via the rotation chain).

use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;
use std::path::Path;

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

/// Verify a single event against a known writer pubkey. A present sig must be
/// valid (always). A missing sig is tolerated only when `!require_sig`.
pub fn verify_one(
    channel_id: &str,
    ev: &ChannelEvent,
    writer_pubkey: &[u8; 32],
    require_sig: bool,
) -> bool {
    match ev.sig.as_deref() {
        Some(sig) => verify_event_sig(
            channel_id,
            &ev.actor,
            ev.kind,
            &ev.payload,
            ev.idempotency_key.as_deref(),
            sig,
            writer_pubkey,
        ),
        None => !require_sig,
    }
}

/// Resolve the writer's pubkey for a given `key_version` by folding the agent's
/// rotation chain (`<agent_home>/rotations.jsonl`). Falls back to the current
/// `identity.pub` when no chain / version match (single-host bootstrap).
pub fn resolve_writer_pubkey(agent_home: &Path, key_version: Option<u32>) -> Option<[u8; 32]> {
    use mur_common::identity::{ChainOptions, RotationAttestation, decode_pubkey, verify_chain};
    let chain_path = agent_home.join("rotations.jsonl");
    if let Ok(text) = std::fs::read_to_string(&chain_path) {
        let chain: Vec<RotationAttestation> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if let Ok(outcome) = verify_chain(
            &chain,
            ChainOptions {
                allow_emergency: false,
            },
        ) {
            if (key_version == Some(outcome.head_key_version) || key_version.is_none())
                && let Ok(b) = decode_pubkey(&outcome.head_pubkey)
            {
                return Some(b);
            }
            if let Some(kv) = key_version
                && let Some(att) = chain.iter().find(|a| a.new_key_version == kv)
                && let Ok(b) = decode_pubkey(&att.new_pubkey)
            {
                return Some(b);
            }
        }
    }
    let txt = std::fs::read_to_string(agent_home.join("identity.pub")).ok()?;
    decode_pubkey(txt.trim()).ok()
}

/// Verify-on-fold: filter a log to the events that pass verification against the
/// channel's writer. Forged (bad-sig) events are dropped + logged; unsigned
/// events pass only when `!require_sig`.
pub fn verify_log(
    channel_id: &str,
    events: Vec<ChannelEvent>,
    writer_pubkey: &[u8; 32],
    require_sig: bool,
) -> Vec<ChannelEvent> {
    events
        .into_iter()
        .filter(|ev| {
            let ok = verify_one(channel_id, ev, writer_pubkey, require_sig);
            if !ok {
                tracing::warn!(
                    channel = channel_id,
                    seq = ev.seq,
                    "dropping unverifiable channel event"
                );
            }
            ok
        })
        .collect()
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

    #[test]
    fn verify_one_keeps_valid_drops_forged_tolerates_legacy() {
        let writer = AgentIdentity::generate();
        let actor = ChannelActor::Agent { id: "mur".into() };
        let p = serde_json::json!({ "text": "ok" });
        let good_sig = sign_event(&writer, "c1", &actor, EventKind::Message, &p, None);
        let forged_sig = sign_event(
            &AgentIdentity::generate(),
            "c1",
            &actor,
            EventKind::Message,
            &p,
            None,
        );
        let mk = |sig: Option<String>| ChannelEvent {
            seq: 0,
            ts: chrono::Utc::now(),
            actor: actor.clone(),
            kind: EventKind::Message,
            payload: p.clone(),
            idempotency_key: None,
            sig,
            key_version: Some(0),
        };
        let pubkey = writer.verifying_key_bytes();
        // valid signed → kept (any require_sig)
        assert!(verify_one("c1", &mk(Some(good_sig)), &pubkey, false));
        // forged signed → rejected (present-but-bad sig always fails)
        assert!(!verify_one("c1", &mk(Some(forged_sig)), &pubkey, false));
        // legacy unsigned → tolerated iff !require_sig
        assert!(verify_one("c1", &mk(None), &pubkey, false));
        assert!(!verify_one("c1", &mk(None), &pubkey, true));
    }
}
