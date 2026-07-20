//! DSSE (Dead Simple Signing Envelope) over in-toto v1 Statement.
//!
//! Spec §6.3: signature envelope format for `.muragent`.

use crate::identity::AgentIdentity;
use crate::muragent::MuragentError;
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use serde::{Deserialize, Serialize};

/// DSSE PAE: `"DSSEv1 " || len(payloadType) || " " || payloadType || " " || len(payload) || " " || payload`
///
/// All len() calls count UTF-8 bytes (not character count).
pub fn pae(payload_type: &str, payload: &str) -> Vec<u8> {
    let mut out = b"DSSEv1 ".to_vec();
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.as_bytes());
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsseEnvelope {
    #[serde(rename = "payloadType")]
    pub payload_type: String,
    pub payload: String,
    pub signatures: Vec<DsseSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsseSignature {
    pub keyid: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub sig: String,
}

/// Sign a payload with the agent's Ed25519 identity, returning a DSSE envelope.
pub fn sign(
    payload_type: &str,
    payload_json: &str,
    identity: &AgentIdentity,
) -> Result<DsseEnvelope, MuragentError> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};

    let pae_bytes = pae(payload_type, payload_json);
    let signing_key = identity.signing_key();
    let signature: Signature = signing_key.sign(&pae_bytes);

    let verifying_key = signing_key.verifying_key();
    let pubkey_bytes = verifying_key.as_bytes();
    let keyid = keyid_from_pubkey(pubkey_bytes);

    let envelope = DsseEnvelope {
        payload_type: payload_type.to_string(),
        payload: B64.encode(payload_json.as_bytes()),
        signatures: vec![DsseSignature {
            keyid,
            public_key: B64.encode(pubkey_bytes),
            sig: B64.encode(signature.to_bytes()),
        }],
    };

    Ok(envelope)
}

/// Verify a DSSE envelope's first signature.
/// Uses `verify_strict` (rejects non-canonical encodings and small-order points).
pub fn verify(envelope: &DsseEnvelope, expected_payload_type: &str) -> Result<(), MuragentError> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};

    if envelope.payload_type != expected_payload_type {
        return Err(MuragentError::DsseError(format!(
            "payload type mismatch: expected '{}', got '{}'",
            expected_payload_type, envelope.payload_type
        )));
    }

    let payload_bytes = B64
        .decode(&envelope.payload)
        .map_err(|e| MuragentError::DsseError(format!("payload base64: {e}")))?;
    let payload_str = String::from_utf8(payload_bytes)
        .map_err(|e| MuragentError::DsseError(format!("payload utf-8: {e}")))?;

    if envelope.signatures.is_empty() {
        return Err(MuragentError::DsseError("no signatures in envelope".into()));
    }

    let sig_entry = &envelope.signatures[0];
    let pae_bytes = pae(expected_payload_type, &payload_str);

    let pubkey_bytes = B64
        .decode(&sig_entry.public_key)
        .map_err(|e| MuragentError::DsseError(format!("public_key base64: {e}")))?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .as_slice()
        .try_into()
        .map_err(|_| MuragentError::DsseError("public_key not 32 bytes".into()))?;
    let verifying_key = VerifyingKey::from_bytes(&pubkey_arr)
        .map_err(|e| MuragentError::DsseError(format!("pubkey decode: {e}")))?;

    let sig_bytes = B64
        .decode(&sig_entry.sig)
        .map_err(|e| MuragentError::DsseError(format!("sig base64: {e}")))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| MuragentError::DsseError("sig not 64 bytes".into()))?;
    let signature = Signature::from_bytes(&sig_arr);

    verifying_key
        .verify_strict(&pae_bytes, &signature)
        .map_err(|e| MuragentError::InvalidSignature(format!("Ed25519 verify_strict: {e}")))?;

    Ok(())
}

/// Derive keyid from the first 8 hex chars of SHA-256(pubkey).
pub fn keyid_from_pubkey(pubkey: &[u8; 32]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(pubkey);
    let hex = format!("{:x}", hash);
    format!("ed25519-{}", &hex[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;

    #[test]
    fn pae_is_deterministic() {
        let a = pae("application/vnd.in-toto+json", r#"{"a":1}"#);
        let b = pae("application/vnd.in-toto+json", r#"{"a":1}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn pae_byte_lengths_not_char_counts() {
        // "café" = 5 bytes in UTF-8 (é = 2 bytes), 4 chars
        let pae_bytes = pae("type", "café");
        let pae_str = String::from_utf8(pae_bytes).unwrap();
        // The payload length should be 5 (bytes), not 4 (chars)
        assert!(
            pae_str.contains(" 5 café"),
            "payload length should be 5 bytes, got: {pae_str}"
        );
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let identity = AgentIdentity::generate();
        let payload = r#"{"manifest_sha256":"abc123"}"#;
        let envelope = sign("application/vnd.in-toto+json", payload, &identity).unwrap();
        verify(&envelope, "application/vnd.in-toto+json").unwrap();
    }

    #[test]
    fn verify_rejects_wrong_payload_type() {
        let identity = AgentIdentity::generate();
        let envelope = sign("application/vnd.in-toto+json", "{}", &identity).unwrap();
        assert!(verify(&envelope, "wrong/type").is_err());
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let identity = AgentIdentity::generate();
        let mut envelope = sign("application/vnd.in-toto+json", r#"{"a":1}"#, &identity).unwrap();
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        envelope.payload = B64.encode(r#"{"a":2}"#);
        assert!(verify(&envelope, "application/vnd.in-toto+json").is_err());
    }

    #[test]
    fn verify_rejects_empty_signatures() {
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        let envelope = DsseEnvelope {
            payload_type: "application/vnd.in-toto+json".into(),
            payload: B64.encode("{}"),
            signatures: vec![],
        };
        assert!(verify(&envelope, "application/vnd.in-toto+json").is_err());
    }
}
