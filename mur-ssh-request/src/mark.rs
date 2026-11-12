//! The signing mark (spec "Signing mark"): one signed note the runtime
//! appends to the agent's start channel when the ssh-agent proxy returns a
//! signature under a live approval. Its presence clears the matching request.
//!
//! Shared here so the writer (`mur-agent-runtime`) and the reader
//! (`mur-core`'s SSH status) agree on one shape. The payload alone proves
//! nothing: a reader must also check the event's signature and match
//! `(action_hash, issued_at)` against a live allow decision.
//!
//! No fingerprint is carried, in any field: the start channel is readable by
//! the agent, and no host-key or key fingerprint is ever shown to the agent
//! (spec Goal 5). `action_hash` names the pair without revealing it; a reader
//! that needs the fingerprints recomputes the hash from the agent's own
//! `ssh.yaml`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `payload.notice` of a signing mark, beside the proxy's classification and
/// refusal notices on the same channel.
pub const SIGNING_MARK_NOTICE: &str = "ssh_signing_mark";

/// The payload of one signing mark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningMark {
    /// Always [`SIGNING_MARK_NOTICE`].
    pub notice: String,
    /// `ssh_action_hash(agent, key_fp, host_key_fp)`: the hash the gate looked up.
    pub action_hash: String,
    /// Copied from the allow decision the signature was approved under.
    pub issued_at: DateTime<Utc>,
    /// The line a person reads in `mur channel follow`. Display only, and
    /// fingerprint-free like the rest of the mark.
    pub text: String,
}

impl SigningMark {
    pub fn new(action_hash: String, issued_at: DateTime<Utc>) -> Self {
        let text = format!(
            "ssh: signed under your approval of {}",
            issued_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        Self {
            notice: SIGNING_MARK_NOTICE.into(),
            action_hash,
            issued_at,
            text,
        }
    }

    /// The channel idempotency key: one mark per approval, across restarts too.
    pub fn idempotency_key(&self) -> String {
        format!(
            "ssh-signing-mark:{}:{}",
            self.action_hash,
            self.issued_at.timestamp_micros()
        )
    }

    /// A mark from a note payload, or `None` when it is not one. Shape only;
    /// see the module docs for what a reader must still check.
    pub fn from_payload(payload: &serde_json::Value) -> Option<Self> {
        if payload.get("notice")?.as_str()? != SIGNING_MARK_NOTICE {
            return None;
        }
        serde_json::from_value(payload.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark() -> SigningMark {
        SigningMark::new("h".into(), "2026-11-11T10:00:00Z".parse().unwrap())
    }

    #[test]
    fn round_trips_through_a_payload() {
        let m = mark();
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["notice"], SIGNING_MARK_NOTICE);
        assert_eq!(SigningMark::from_payload(&v), Some(m));
    }

    #[test]
    fn other_notices_are_not_marks() {
        let mut v = serde_json::to_value(mark()).unwrap();
        v["notice"] = "ssh_proxy_refusal".into();
        assert_eq!(SigningMark::from_payload(&v), None);
        assert_eq!(SigningMark::from_payload(&serde_json::json!({})), None);
    }

    /// Spec Goal 5: the mark lands where the agent can read it, so it carries
    /// no fingerprint and no field that could hold one.
    #[test]
    fn a_mark_carries_no_fingerprint_field() {
        let v = serde_json::to_value(mark()).unwrap();
        let mut fields: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        fields.sort();
        assert_eq!(fields, ["action_hash", "issued_at", "notice", "text"]);
        assert!(!v.to_string().contains("SHA256:"));
    }

    #[test]
    fn one_key_per_approval() {
        let a = mark();
        let mut b = mark();
        b.text = "another rendering".into();
        assert_eq!(a.idempotency_key(), b.idempotency_key());
        b.issued_at += chrono::Duration::seconds(1);
        assert_ne!(a.idempotency_key(), b.idempotency_key());
    }
}
