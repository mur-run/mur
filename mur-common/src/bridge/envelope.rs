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

/// Sign a canonical-JSON payload with the bridge's identity key.
///
/// `payload` MUST already be canonicalized (sorted keys, no whitespace) —
/// this function does NOT re-canonicalize. Verifiers re-use the exact bytes
/// stored in the resulting `SignedEnvelope.payload`; never re-canonicalize on
/// receive.
pub fn sign_payload(
    payload: Vec<u8>,
    identity: &crate::identity::AgentIdentity,
    key_version: u32,
) -> SignedEnvelope {
    let sig = identity.signing_key().sign(&payload);
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
    vk.verify_strict(&env.payload, &sig)
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
}
