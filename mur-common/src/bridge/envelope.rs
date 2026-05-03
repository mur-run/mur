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
}
