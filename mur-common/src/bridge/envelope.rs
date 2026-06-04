use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};

/// Bridge-signed wrapper around an A2A payload. `payload` is the *already-
/// canonical* JSON-serialized A2A `JsonRpcRequest`; the bridge canonicalizes
/// (sorted keys, no whitespace) BEFORE construction. Verification re-uses
/// these exact bytes — never re-canonicalize on receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEnvelope {
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub sig: Vec<u8>,
    pub key_version: u32,
    pub bridge_pubkey_multibase: String,
}

impl SignedEnvelope {
    pub fn canonical_payload_for_signing(&self) -> &[u8] {
        &self.payload
    }
}

#[derive(thiserror::Error, Debug)]
pub enum EnvelopeError {
    #[error("multibase decode: {0}")]
    Multibase(#[from] crate::identity::IdentityError),
    #[error("bad sig length: expected 64, got {0}")]
    BadSigLen(usize),
    #[error("signature does not verify")]
    SignatureMismatch,
    #[error("untrusted peer")]
    UntrustedPeer,
}

/// Domain-separation tag for bridge envelope signatures.
///
/// The agent's identity key also signs other things (notably `.muragent` DSSE
/// statements), so a distinct, fixed-length prefix keeps a signature made in one
/// context from ever verifying in another. It also lets us fold `key_version`
/// into the signed bytes — `key_version` lives outside `payload` on the wire, so
/// without this an attacker could replay a captured envelope with `key_version`
/// flipped and defeat a `TrustedPeer` version pin.
const ENVELOPE_DOMAIN: &[u8] = b"mur-bridge-envelope-v1";

/// The exact bytes covered by an envelope signature: a fixed-length domain tag,
/// the little-endian `key_version`, then the canonical payload. All fields ahead
/// of `payload` are fixed-length, so the encoding is unambiguous.
fn signing_bytes(payload: &[u8], key_version: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_DOMAIN.len() + 4 + payload.len());
    out.extend_from_slice(ENVELOPE_DOMAIN);
    out.extend_from_slice(&key_version.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Sign a canonical-JSON payload with the bridge's identity key.
///
/// `payload` MUST already be canonicalized (sorted keys, no whitespace) —
/// this function does NOT re-canonicalize. The signature covers the domain tag,
/// `key_version`, and `payload` (see [`signing_bytes`]); verifiers reconstruct
/// those exact bytes and never re-canonicalize the payload on receive.
pub fn sign_payload(
    payload: Vec<u8>,
    identity: &crate::identity::AgentIdentity,
    key_version: u32,
) -> SignedEnvelope {
    let sig = identity
        .signing_key()
        .sign(&signing_bytes(&payload, key_version));
    SignedEnvelope {
        payload,
        sig: sig.to_bytes().to_vec(),
        key_version,
        bridge_pubkey_multibase: crate::identity::encode_pubkey(&identity.verifying_key()),
    }
}

/// Verify the envelope's signature against an expected pubkey (multibase).
///
/// This is the low-level check: it does not consult any trust list. Callers
/// that need authorization (peer-must-be-trusted) should layer
/// `verify_inbound_envelope` on top of this.
pub fn verify_envelope_with_pubkey(
    env: &SignedEnvelope,
    expected_pubkey: &str,
) -> Result<(), EnvelopeError> {
    use ed25519_dalek::{Signature, VerifyingKey};
    if env.sig.len() != 64 {
        return Err(EnvelopeError::BadSigLen(env.sig.len()));
    }
    let pub_bytes = crate::identity::decode_pubkey(expected_pubkey)?;
    let vk = VerifyingKey::from_bytes(&pub_bytes).map_err(|_| EnvelopeError::SignatureMismatch)?;
    let sig_arr: [u8; 64] = env.sig.as_slice().try_into().unwrap();
    let sig = Signature::from_bytes(&sig_arr);
    // Reconstruct the exact signed bytes (domain || key_version || payload).
    // Because key_version is folded in here, mutating it on the wire breaks the
    // signature — which is what makes a TrustedPeer version pin enforceable.
    let msg = signing_bytes(&env.payload, env.key_version);
    vk.verify_strict(&msg, &sig)
        .map_err(|_| EnvelopeError::SignatureMismatch)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_payload_is_passthrough() {
        let payload = serde_json::json!({"a": 1}).to_string().into_bytes();
        let e = SignedEnvelope {
            payload: payload.clone(),
            sig: vec![0u8; 64],
            key_version: 1,
            bridge_pubkey_multibase: "z".into(),
        };
        assert_eq!(e.canonical_payload_for_signing(), payload.as_slice());
    }

    #[test]
    fn sign_then_verify_round_trips() {
        use crate::identity::AgentIdentity;
        let id = AgentIdentity::generate();
        let env = sign_payload(b"hello".to_vec(), &id, 7);
        assert_eq!(env.key_version, 7);
        verify_envelope_with_pubkey(&env, &env.bridge_pubkey_multibase).unwrap();
    }

    #[test]
    fn verify_with_wrong_pubkey_fails() {
        use crate::identity::{AgentIdentity, encode_pubkey};
        let a = AgentIdentity::generate();
        let b = AgentIdentity::generate();
        let env = sign_payload(b"x".to_vec(), &a, 0);
        let pub_b = encode_pubkey(&b.verifying_key());
        assert!(matches!(
            verify_envelope_with_pubkey(&env, &pub_b).unwrap_err(),
            EnvelopeError::SignatureMismatch
        ));
    }

    #[test]
    fn tampered_payload_fails() {
        use crate::identity::AgentIdentity;
        let id = AgentIdentity::generate();
        let mut env = sign_payload(b"orig".to_vec(), &id, 0);
        env.payload = b"tamper".to_vec();
        let pub_ = env.bridge_pubkey_multibase.clone();
        assert!(matches!(
            verify_envelope_with_pubkey(&env, &pub_).unwrap_err(),
            EnvelopeError::SignatureMismatch
        ));
    }

    #[test]
    fn tampered_key_version_fails() {
        // key_version is now covered by the signature, so flipping it on the
        // wire (to slip past a TrustedPeer version pin) invalidates the envelope.
        use crate::identity::AgentIdentity;
        let id = AgentIdentity::generate();
        let mut env = sign_payload(b"orig".to_vec(), &id, 3);
        let pub_ = env.bridge_pubkey_multibase.clone();
        verify_envelope_with_pubkey(&env, &pub_).expect("untampered verifies");
        env.key_version = 4;
        assert!(matches!(
            verify_envelope_with_pubkey(&env, &pub_).unwrap_err(),
            EnvelopeError::SignatureMismatch
        ));
    }
}
