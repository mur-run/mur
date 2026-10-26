//! Signed fleet-dispatch request (#2116) — how a sealed agent asks the daemon
//! to launch a fleet whose `router` is a DIFFERENT agent.
//!
//! A sealed runtime cannot read `keys/<router>/` (deliberately — #850), and the
//! signing handoff it can give a child carries only its OWN key. So a child it
//! spawns for another router's fleet would write every channel event unsigned.
//! Instead the runtime drops one of these (YAML, tmp+rename) into
//! `<mur_home>/inbox/fleet-dispatch-requests/`; `mur-daemon`, outside every
//! sandbox, verifies it against the requester's on-disk pubkey and launches the
//! fleet, whose child then signs with the router's own key. No agent ever holds
//! another agent's private key.
//!
//! Canonical sign-input EXCLUDES `sig` (v3d ChannelEvent precedent) and is the
//! RFC 8785 form of every other field, so adding a field can never leave it
//! unsigned by accident.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mur_common::identity::{AgentIdentity, verify_bytes};

/// File-drop directory, relative to the MUR home.
pub const FLEET_DISPATCH_REQUEST_DIR: &str = "inbox/fleet-dispatch-requests";

/// Per-run record the daemon writes beside the run's log:
/// `runs/<run_id>/dispatch.json`. Its existence also marks the run id as spent.
pub const FLEET_DISPATCH_RECORD_FILE: &str = "dispatch.json";

/// Domain tag: a signature over one request type must never verify as another.
const DOMAIN: &str = "mur-fleet-dispatch-request-v1\n";

/// What the requester asks for. Everything here is signed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetDispatch {
    /// Canonical name of the agent asking; the key that must verify.
    pub requested_by: String,
    pub fleet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    /// Absolute target directory, when the requester had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub cwd_inferred: bool,
    /// Minted by the requester and already handed back to its caller, so the
    /// handle it returned is the one the run is polled under.
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchRequest {
    #[serde(flatten)]
    pub dispatch: FleetDispatch,
    pub requested_at: DateTime<Utc>,
    /// Key-rotation version; recorded, verified against the CURRENT pubkey.
    #[serde(default)]
    pub key_version: u32,
    /// Multibase (Base58Btc) Ed25519 signature over [`sign_input`].
    pub sig: String,
}

fn sign_input(d: &FleetDispatch, requested_at: &DateTime<Utc>, key_version: u32) -> Vec<u8> {
    let mut body = serde_json::to_value(d).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "requested_at".into(),
            serde_json::Value::String(requested_at.to_rfc3339()),
        );
        obj.insert("key_version".into(), serde_json::Value::from(key_version));
    }
    let mut out = DOMAIN.as_bytes().to_vec();
    out.extend(mur_common::jcs::to_jcs(&body));
    out
}

impl FleetDispatchRequest {
    pub fn create(
        dispatch: FleetDispatch,
        identity: &AgentIdentity,
        key_version: u32,
        now: DateTime<Utc>,
    ) -> Self {
        let sig = identity.sign_multibase(&sign_input(&dispatch, &now, key_version));
        Self {
            dispatch,
            requested_at: now,
            key_version,
            sig,
        }
    }

    /// Fail-closed signature check against `pubkey`.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        let input = sign_input(&self.dispatch, &self.requested_at, self.key_version);
        verify_bytes(pubkey, &input, &self.sig)
    }

    /// Inside the acceptance window? Blunts replay together with the spent
    /// run id ([`FLEET_DISPATCH_RECORD_FILE`]) and consuming the file.
    pub fn is_fresh(&self, now: DateTime<Utc>, max_age_secs: u64) -> bool {
        let age = now.signed_duration_since(self.requested_at);
        age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(max_age_secs as i64)
    }
}

/// The daemon's verdict, written to `runs/<run_id>/dispatch.json`. This is
/// where a run's provenance lives: who asked, and who signs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetDispatchRecord {
    pub run_id: String,
    pub fleet: String,
    /// The agent whose verified signature requested this run.
    pub dispatched_by: String,
    /// The agent whose key signs the run's channel events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    pub status: FleetDispatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetDispatchStatus {
    Launched,
    Rejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch() -> FleetDispatch {
        FleetDispatch {
            requested_by: "mur".into(),
            fleet: "council".into(),
            goal: Some("decide".into()),
            job: None,
            cwd: Some("/work".into()),
            cwd_inferred: false,
            run_id: "fleet-council-1".into(),
        }
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = AgentIdentity::generate();
        let req = FleetDispatchRequest::create(dispatch(), &id, 0, Utc::now());
        assert!(req.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn every_field_is_covered_by_the_signature() {
        let id = AgentIdentity::generate();
        let pk = id.verifying_key_bytes();
        let base = FleetDispatchRequest::create(dispatch(), &id, 0, Utc::now());
        let tamper: [fn(&mut FleetDispatchRequest); 8] = [
            |r| r.dispatch.requested_by = "orchestrator".into(),
            |r| r.dispatch.fleet = "other".into(),
            |r| r.dispatch.goal = Some("rm -rf".into()),
            |r| r.dispatch.job = Some("x".into()),
            |r| r.dispatch.cwd = Some("/elsewhere".into()),
            |r| r.dispatch.cwd_inferred = true,
            |r| r.dispatch.run_id = "fleet-council-2".into(),
            |r| r.key_version = 7,
        ];
        for t in tamper {
            let mut r = base.clone();
            t(&mut r);
            assert!(!r.verify(&pk), "tampered field still verified: {r:?}");
        }
    }

    #[test]
    fn wrong_key_fails_verification() {
        let req =
            FleetDispatchRequest::create(dispatch(), &AgentIdentity::generate(), 0, Utc::now());
        assert!(!req.verify(&AgentIdentity::generate().verifying_key_bytes()));
    }

    #[test]
    fn freshness_window_rejects_old_and_future() {
        let now = Utc::now();
        let req = FleetDispatchRequest::create(dispatch(), &AgentIdentity::generate(), 0, now);
        assert!(req.is_fresh(now, 60));
        assert!(!req.is_fresh(now + chrono::Duration::seconds(61), 60));
        assert!(!req.is_fresh(now - chrono::Duration::seconds(1), 60));
    }

    #[test]
    fn yaml_roundtrip_preserves_signature() {
        let id = AgentIdentity::generate();
        let req = FleetDispatchRequest::create(dispatch(), &id, 3, Utc::now());
        let yaml = serde_yaml_ng::to_string(&req).unwrap();
        let back: FleetDispatchRequest = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.dispatch, req.dispatch);
        assert!(back.verify(&id.verifying_key_bytes()));
    }
}
