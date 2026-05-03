use serde::{Deserialize, Serialize};

/// One pubkey of a bridge (or other LLM-less peer) that this agent will accept
/// signed envelopes from.
///
/// `key_version` is an optional pin: when `Some(N)`, only envelopes with
/// `key_version == N` are accepted; when `None`, any `key_version` is accepted
/// as long as the pubkey matches. Pins let an operator quarantine a specific
/// rotation without removing the peer entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub pubkey_multibase: String,
    pub name: String,
    /// Optional version pin. None = any key_version with matching pubkey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_version: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let p = TrustedPeer {
            pubkey_multibase: "z6Mk".into(),
            name: "tg_bridge".into(),
            key_version: Some(3),
        };
        let s = serde_yaml_ng::to_string(&p).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<TrustedPeer>(&s).unwrap(), p);
    }
}
