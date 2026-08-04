//! Signed snapshot-pull request — the agent-side half of the memory-federation
//! pull leg (spec: docs/superpowers/specs/2026-08-04-unified-memory-federation.md).
//! An agent runtime writes one (YAML, tmp+rename) into
//! `<mur_home>/inbox/snapshot-requests/`; the daemon verifies it against the
//! agent's on-disk pubkey and assembles the snapshot central-side.
//! Canonical sign-input EXCLUDES `sig` (v3d ChannelEvent precedent).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{AgentIdentity, verify_bytes};

/// File-drop directory, relative to the MUR home.
pub const SNAPSHOT_REQUEST_DIR: &str = "inbox/snapshot-requests";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRequest {
    pub agent: String,
    pub requested_at: DateTime<Utc>,
    /// Key-rotation version; 0 = initial identity key. Recorded for forward
    /// compatibility — P0 verifies against the CURRENT pubkey only.
    #[serde(default)]
    pub key_version: u32,
    /// Multibase (Base58Btc) Ed25519 signature over the canonical sign-input.
    pub sig: String,
}

/// Canonical signed bytes: domain tag + fields, `sig` excluded.
fn sign_input(agent: &str, requested_at: &DateTime<Utc>, key_version: u32) -> Vec<u8> {
    format!(
        "mur-snapshot-request-v1\n{agent}\n{}\n{key_version}",
        requested_at.to_rfc3339()
    )
    .into_bytes()
}

impl SnapshotRequest {
    pub fn create(agent: &str, identity: &AgentIdentity, now: DateTime<Utc>) -> Self {
        let input = sign_input(agent, &now, 0);
        Self {
            agent: agent.to_string(),
            requested_at: now,
            key_version: 0,
            sig: identity.sign_multibase(&input),
        }
    }

    /// Fail-closed signature check against `pubkey`.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        let input = sign_input(&self.agent, &self.requested_at, self.key_version);
        verify_bytes(pubkey, &input, &self.sig)
    }

    /// Inside the acceptance window? Blunts replay; not a nonce store —
    /// consuming the request file on processing is the other half.
    pub fn is_fresh(&self, now: DateTime<Utc>, max_age_secs: u64) -> bool {
        let age = now.signed_duration_since(self.requested_at);
        age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(max_age_secs as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> AgentIdentity {
        AgentIdentity::generate()
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = identity();
        let req = SnapshotRequest::create("dr_worker_4", &id, Utc::now());
        assert!(req.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn tampered_agent_name_fails_verification() {
        let id = identity();
        let mut req = SnapshotRequest::create("dr_worker_4", &id, Utc::now());
        req.agent = "dr_worker_1".into(); // impersonation attempt
        assert!(!req.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let req = SnapshotRequest::create("a", &identity(), Utc::now());
        assert!(!req.verify(&identity().verifying_key_bytes()));
    }

    #[test]
    fn freshness_window_rejects_old_and_future() {
        let id = identity();
        let now = Utc::now();
        let req = SnapshotRequest::create("a", &id, now);
        assert!(req.is_fresh(now, 600));
        assert!(!req.is_fresh(now + chrono::Duration::seconds(601), 600)); // stale
        assert!(!req.is_fresh(now - chrono::Duration::seconds(1), 600)); // future-dated
    }

    #[test]
    fn yaml_roundtrip_preserves_signature() {
        let id = identity();
        let req = SnapshotRequest::create("a", &id, Utc::now());
        let yaml = serde_yaml_ng::to_string(&req).unwrap();
        let back: SnapshotRequest = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(back.verify(&id.verifying_key_bytes()));
    }
}
